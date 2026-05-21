// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::time::Duration;

use bytes::Bytes;
use eyre::Context;
use ssz::Decode;
use tracing::{error, warn};

use malachitebft_app_channel::app::types::core::Round;
use malachitebft_app_channel::app::types::ProposedValue;
use malachitebft_app_channel::Reply;

use alloy_rpc_types_engine::ExecutionPayloadV3;
use arc_consensus_types::{Address, ArcContext, Height};
use arc_eth_engine::engine::Engine;
use arc_eth_engine::persistence_meter::PersistenceMeter;

use malachitebft_app_channel::app::types::core::Validity;

use crate::block::ConsensusBlock;
use crate::metrics::{AppMetrics, InvalidPayloadSource};
use crate::payload::{validate_consensus_block, EnginePayloadValidator, PayloadValidator};
use crate::state::State;
use crate::store::repositories::{InvalidPayloadsRepository, UndecidedBlocksRepository};
use arc_consensus_db::invalid_payloads::InvalidPayload;

/// Timeout when blocked waiting for EL persistence to catch up during sync.
const SYNC_PERSISTENCE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Handles the `ProcessSyncedValue` message from the consensus engine.
///
/// This is called when the consensus engine has received a value via sync for a given height and round.
/// The application processes the synced value, validates it, and stores it for future use.
/// If the value is valid, it is returned as a `ProposedValue` to the consensus engine.
/// If the value is invalid, `None` is returned.
/// In both cases, the block is stored in the undecided blocks store for use once consensus reaches
/// that height.
pub async fn handle(
    state: &mut State,
    engine: &Engine,
    height: Height,
    round: Round,
    proposer: Address,
    value_bytes: Bytes,
    reply: Reply<Option<ProposedValue<ArcContext>>>,
) -> Result<(), eyre::Error> {
    let proposal = match on_process_synced_value(
        EnginePayloadValidator::new(engine, state.metrics()),
        state.store(),
        state.store(),
        state.persistence_meter(),
        state.metrics(),
        height,
        round,
        proposer,
        value_bytes,
    )
    .await
    {
        Ok(proposal) => proposal,
        Err(e) => {
            error!(%height, %round, %proposer, "ProcessSyncedValue failed: {e:#}");
            if let Err(send_err) = reply.send(None) {
                error!("🔴 ProcessSyncedValue: Failed to send error reply: {send_err:?}");
            }
            return Err(e);
        }
    };

    // Mark this height as synced for proposal monitoring
    if let Some(p) = &proposal
        && p.validity.is_valid()
    {
        state.mark_height_synced(height);
    }

    if let Err(e) = reply.send(proposal) {
        error!("🔴 ProcessSyncedValue: Failed to send reply: {e:?}");
    }

    Ok(())
}

/// Processes a synced value received from a peer.
///
/// Decodes the raw bytes into an [`ExecutionPayloadV3`], validates it via
/// [`validate_consensus_block`], and stores the resulting [`ConsensusBlock`] as an
/// undecided block. If the engine rejects the payload, an
/// [`InvalidPayload`](crate::invalid_payloads::InvalidPayload) record is persisted
/// through `store` and the block is kept with [`Validity::Invalid`] so that
/// consensus can proceed with the correct validity information.
///
/// Returns `Ok(None)` when the raw bytes cannot be SSZ-decoded (the error is logged
/// but not propagated).
#[allow(clippy::too_many_arguments)]
async fn on_process_synced_value(
    engine: impl PayloadValidator,
    undecided_blocks_repo: impl UndecidedBlocksRepository,
    invalid_payloads_repo: impl InvalidPayloadsRepository,
    persistence_meter: impl PersistenceMeter,
    metrics: &AppMetrics,
    height: Height,
    round: Round,
    proposer: Address,
    value_bytes: Bytes,
) -> eyre::Result<Option<ProposedValue<ArcContext>>> {
    let payload = match ExecutionPayloadV3::from_ssz_bytes(&value_bytes) {
        Ok(payload) => payload,
        Err(e) => {
            warn!(
                %height, %round, %proposer,
                "Failed to decode synced value into an execution payload: {e:?}",
            );
            metrics.inc_invalid_payloads_count(InvalidPayloadSource::SyncDecode);

            let invalid =
                InvalidPayload::new_without_payload(height, round, proposer, &format!("{e:?}"));

            invalid_payloads_repo.append(invalid).await.wrap_err_with(|| {
                format!(
                    "Failed to store invalid payload after receiving synced value (height={height}, round={round}, proposer={proposer})",
                )
            })?;

            return Ok(None);
        }
    };

    // Build the block before validation so that
    // `validate_consensus_block` can record an `InvalidPayload`
    // with the full block context if the engine rejects it.
    let mut block = ConsensusBlock {
        height,
        round,
        valid_round: Round::Nil,
        proposer,
        execution_payload: payload,
        validity: Validity::Valid,
        signature: None,
    };

    let validity = validate_consensus_block(
        &engine, &block, &invalid_payloads_repo, metrics,
    )
    .await
    .wrap_err_with(|| {
        format!(
            "Payload validation failed on block built from synced value at height={}, round={} received from {}",
            height, round, proposer,
        )
    })?;

    block.validity = validity;

    let block_hash = block.block_hash();

    if !validity.is_valid() {
        error!(%height, %round, %proposer, %block_hash, "❌ Received invalid payload via sync");
    }

    // If a undecided block for the sync value height round and hash exists then skip `wait_for_persisted_block`
    // so consensus path is not blocked on EL persistence.
    if let Some(existing) = undecided_blocks_repo
        .get_by_round_and_hash(height, round, block_hash)
        .await
        .wrap_err_with(|| {
            format!(
                "Failed to query undecided blocks repo for dedup at \
                 height={height}, round={round}, block_hash={block_hash}"
            )
        })?
    {
        debug_assert_eq!(
            existing.validity, validity,
            "dedup hit at height={height}, round={round}, block_hash={block_hash}: \
             existing.validity ({:?}) != freshly-computed validity ({validity:?})",
            existing.validity,
        );
        return Ok(Some(ProposedValue::from(&existing)));
    }

    let proposal = ProposedValue::from(&block);

    undecided_blocks_repo.store_undecided_block(block).await.wrap_err_with(|| {
        format!(
            "Failed to store undecided block {} synced from the network for height={}, round={}, proposer={}",
            block_hash, height, round, proposer,
        )
    })?;

    if validity.is_valid() {
        if let Err(e) = persistence_meter
            .wait_for_persisted_block(height.as_u64(), SYNC_PERSISTENCE_WAIT_TIMEOUT)
            .await
        {
            error!(
                block_number = height.as_u64(),
                %e,
                "ProcessSyncedValue: persistence backpressure timed out, proceeding"
            );
        }
    }

    Ok(Some(proposal))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::payload::{MockPayloadValidator, PayloadValidationResult};
    use crate::store::repositories::mocks::{
        MockInvalidPayloadsRepository, MockUndecidedBlocksRepository,
    };

    use arbitrary::{Arbitrary, Unstructured};
    use arc_consensus_types::Value;
    use arc_eth_engine::mocks::MockPersistenceMeter;
    use arc_eth_engine::persistence_meter::NoopPersistenceMeter;
    use bytes::Bytes;
    use malachitebft_core_types::Validity;
    use mockall::predicate::*;
    use ssz::Encode;
    use std::io;

    /// Sets up the dedup query (`get_by_round_and_hash`) on an
    /// `UndecidedBlocksRepository` mock to return `None` so the main path
    /// flows through to `store_undecided_block`. Use in tests that are not
    /// specifically exercising the dedup race.
    fn expect_no_undecided_dedup_hit(mock: &mut MockUndecidedBlocksRepository) {
        mock.expect_get_by_round_and_hash()
            .returning(|_, _, _| Ok(None));
    }

    async fn test_on_process_synced_value_validity(
        result: PayloadValidationResult,
        expected: Validity,
    ) {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .with(always())
            .returning(move |_| Ok(result.clone()));

        let mut undecided = MockUndecidedBlocksRepository::new();
        expect_no_undecided_dedup_hit(&mut undecided);
        undecided
            .expect_store_undecided_block()
            .withf(move |block| {
                block.height == height && block.round == round && block.proposer == proposer
            })
            .times(1)
            .returning(|_| Ok(()));

        let is_invalid = !expected.is_valid();
        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid
            .expect_append()
            .times(if is_invalid { 1 } else { 0 })
            .returning(|_| Ok(()));

        let metrics = AppMetrics::default();
        let Some(proposal) = on_process_synced_value(
            engine,
            undecided,
            invalid,
            NoopPersistenceMeter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await
        .expect("Failed to process synced value") else {
            panic!("Expected proposal to be Some even for invalid payload");
        };

        assert_eq!(proposal.validity, expected);
        let expected_count = if expected.is_valid() { 0 } else { 1 };
        assert_eq!(metrics.get_invalid_payloads_count(), expected_count);
    }

    #[tokio::test]
    async fn on_process_synced_value_invalid_payload() {
        test_on_process_synced_value_validity(
            PayloadValidationResult::Invalid {
                reason: "test rejection".into(),
            },
            Validity::Invalid,
        )
        .await;
    }

    #[tokio::test]
    async fn on_process_synced_value_valid_payload() {
        test_on_process_synced_value_validity(PayloadValidationResult::Valid, Validity::Valid)
            .await;
    }

    #[tokio::test]
    async fn test_on_process_synced_value_invalid_bytes() {
        let mut engine = MockPayloadValidator::new();
        let mut undecided = MockUndecidedBlocksRepository::new();
        let mut invalid = MockInvalidPayloadsRepository::new();

        // Expectation: If bytes are invalid, we should NOT validate the payload
        // and definitely NOT store the block.
        engine.expect_validate_payload().times(0);
        undecided.expect_store_undecided_block().times(0);
        invalid.expect_append().times(1).returning(|_| Ok(()));

        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let value_bytes = Bytes::from(vec![0u8; 10]);

        let metrics = AppMetrics::default();
        let proposal = on_process_synced_value(
            engine,
            undecided,
            invalid,
            NoopPersistenceMeter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await
        .expect("Failed to process synced value");

        assert!(proposal.is_none());
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    // These two tests cover error paths in `on_process_synced_value` that were
    // previously untested. They do NOT directly test the error-reply-before-propagation
    // fix in `handle()`, which requires `State` and `Engine` -- concrete types
    // with no test builder.
    #[tokio::test]
    async fn on_process_synced_value_engine_validation_error() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .returning(|_| Err(io::Error::other("Simulated engine error").into()));

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided.expect_store_undecided_block().times(0);

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let metrics = AppMetrics::default();
        let result = on_process_synced_value(
            engine,
            undecided,
            invalid,
            NoopPersistenceMeter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn on_process_synced_value_invalid_payload_store_fails() {
        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let value_bytes = Bytes::from(vec![0u8; 10]); // garbage bytes trigger SSZ decode failure

        let mut engine = MockPayloadValidator::new();
        engine.expect_validate_payload().times(0);

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided.expect_store_undecided_block().times(0);

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid
            .expect_append()
            .times(1)
            .returning(|_| Err(io::Error::other("Simulated invalid payload store error")));

        let metrics = AppMetrics::default();
        let result = on_process_synced_value(
            engine,
            undecided,
            invalid,
            NoopPersistenceMeter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    #[tokio::test]
    async fn test_on_process_synced_value_store_error() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut undecided = MockUndecidedBlocksRepository::new();
        expect_no_undecided_dedup_hit(&mut undecided);
        undecided
            .expect_store_undecided_block()
            .times(1)
            .returning(|_| Err(io::Error::other("Simulated store error")));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let metrics = AppMetrics::default();
        let result = on_process_synced_value(
            engine,
            undecided,
            invalid,
            NoopPersistenceMeter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().downcast_ref::<io::Error>().is_some());
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn on_process_synced_value_calls_persistence_meter_for_valid_payload() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(42);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut undecided = MockUndecidedBlocksRepository::new();
        expect_no_undecided_dedup_hit(&mut undecided);
        undecided
            .expect_store_undecided_block()
            .times(1)
            .returning(|_| Ok(()));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let mut persistence_meter = MockPersistenceMeter::new();
        persistence_meter
            .expect_wait_for_persisted_block()
            .withf(|&block, _| block == 42)
            .times(1)
            .return_once(|_, _| Ok(()));

        let metrics = AppMetrics::default();
        let proposal = on_process_synced_value(
            engine,
            undecided,
            invalid,
            persistence_meter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await
        .expect("should succeed");

        assert!(proposal.is_some());
        assert_eq!(proposal.unwrap().validity, Validity::Valid);
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn on_process_synced_value_skips_persistence_meter_for_invalid_payload() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(42);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let mut engine = MockPayloadValidator::new();
        engine.expect_validate_payload().returning(|_| {
            Ok(PayloadValidationResult::Invalid {
                reason: "bad".into(),
            })
        });

        let mut undecided = MockUndecidedBlocksRepository::new();
        expect_no_undecided_dedup_hit(&mut undecided);
        undecided
            .expect_store_undecided_block()
            .times(1)
            .returning(|_| Ok(()));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(1).returning(|_| Ok(()));

        let mut persistence_meter = MockPersistenceMeter::new();
        persistence_meter.expect_wait_for_persisted_block().times(0);

        let metrics = AppMetrics::default();
        let proposal = on_process_synced_value(
            engine,
            undecided,
            invalid,
            persistence_meter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await
        .expect("should succeed");

        assert!(proposal.is_some());
        assert_eq!(proposal.unwrap().validity, Validity::Invalid);
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    #[tokio::test]
    async fn on_process_synced_value_proceeds_when_persistence_meter_fails() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(7);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut undecided = MockUndecidedBlocksRepository::new();
        expect_no_undecided_dedup_hit(&mut undecided);
        undecided
            .expect_store_undecided_block()
            .times(1)
            .returning(|_| Ok(()));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let mut persistence_meter = MockPersistenceMeter::new();
        persistence_meter
            .expect_wait_for_persisted_block()
            .withf(|&block, _| block == 7)
            .times(1)
            .return_once(|_, _| Err(eyre::eyre!("persistence meter timeout")));

        let metrics = AppMetrics::default();
        let proposal = on_process_synced_value(
            engine,
            undecided,
            invalid,
            persistence_meter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await
        .expect("should succeed even when meter fails");

        assert!(proposal.is_some());
        assert_eq!(proposal.unwrap().validity, Validity::Valid);
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    /// Race: the proposer's gossiped proposal arrived first and was already
    /// stored as `UndecidedBlock(height, round, block_hash)` (and the EL has
    /// it persisted) by the time the in-flight ProcessSyncedValue with
    /// identical bytes runs. We must:
    ///   - still run engine validation (defense in depth: the synced bytes
    ///     are from a peer we don't trust implicitly), but
    ///   - skip the redundant `store_undecided_block` upsert and the
    ///     `wait_for_persisted_block` call,
    /// and return a `ProposedValue` carrying the existing block's validity
    /// (which equals the freshly-validated one, since engine validation is
    /// deterministic).
    #[tokio::test]
    async fn on_process_synced_value_dedups_against_existing_undecided_block() {
        let mut u = Unstructured::new(&[7u8; 512]);

        let height = Height::new(42);
        let round = Round::new(0);
        let proposer = Address::new([1u8; 20]);
        let payload = ExecutionPayloadV3::arbitrary(&mut u).unwrap();
        let block_hash = payload.payload_inner.payload_inner.block_hash;
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let existing_block = ConsensusBlock {
            height,
            round,
            valid_round: Round::Nil,
            proposer,
            execution_payload: payload,
            validity: Validity::Valid,
            signature: None,
        };

        // Engine validation still runs once (defense in depth on the synced
        // bytes), and accepts.
        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .times(1)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_round_and_hash()
            .with(eq(height), eq(round), eq(block_hash))
            .times(1)
            .return_once(move |_, _, _| Ok(Some(existing_block)));
        // No redundant upsert.
        undecided.expect_store_undecided_block().times(0);

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        // No persistence wait — the proposer's path already satisfied it.
        let mut persistence_meter = MockPersistenceMeter::new();
        persistence_meter.expect_wait_for_persisted_block().times(0);

        let metrics = AppMetrics::default();
        let proposal = on_process_synced_value(
            engine,
            undecided,
            invalid,
            persistence_meter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await
        .expect("should succeed via dedup short-circuit");

        let proposal = proposal.expect("expected Some(proposal) on dedup hit");
        assert_eq!(proposal.height, height);
        assert_eq!(proposal.round, round);
        assert_eq!(proposal.proposer, proposer);
        assert_eq!(proposal.validity, Validity::Valid);
        assert_eq!(proposal.value, Value::new(block_hash));
    }
}
