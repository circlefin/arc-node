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

use malachitebft_app_channel::app::engine::host::SyncedValueOutcome;
use malachitebft_app_channel::app::types::core::Round;
use malachitebft_app_channel::app::types::ProposedValue;
use malachitebft_app_channel::Reply;

use alloy_rpc_types_engine::ExecutionPayloadV3;
use arc_consensus_types::{Address, ArcContext, Height};
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_eth_engine::persistence_meter::PersistenceMeter;

use malachitebft_app_channel::app::types::core::Validity;

use crate::block::ConsensusBlock;
use crate::metrics::{AppMetrics, InvalidPayloadSource, VerdictFlip};
use crate::payload::{
    establish_block_validity, resolve_verdict_flip, BlockVerdict, EnginePayloadValidator,
    FlipResolution, PayloadOrigin, PayloadValidator, VerdictOutcome,
};
use crate::state::State;
use crate::store::repositories::{InvalidPayloadsRepository, UndecidedBlocksRepository};
use crate::utils::HaltAndWait;

/// Timeout when blocked waiting for EL persistence to catch up during sync.
const SYNC_PERSISTENCE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Handles the `ProcessSyncedValue` message from the consensus engine.
///
/// This is called when the consensus engine has received a value via sync for a given height and round.
/// The application processes the synced value, validates it, and stores it for future use.
/// The reply tells the sync layer how the value was handled:
/// - [`SyncedValueOutcome::Verdict`] — a validated `ProposedValue` (validity rides inside) to forward
///   to consensus; the block is stored in the undecided blocks store for use once consensus reaches
///   that height.
/// - [`SyncedValueOutcome::PeerFault`] — the value is one no correct peer serves: undecodable
///   bytes, or a self-reported hash that is not canonical. Penalize and re-request.
/// - [`SyncedValueOutcome::LocalTransientError`] — a local/transient failure on our side (e.g. the EL
///   being temporarily unavailable); the peer is innocent, so re-request without penalizing it.
pub async fn handle(
    state: &mut State,
    engine: &Engine,
    height: Height,
    round: Round,
    proposer: Address,
    value_bytes: Bytes,
    reply: Reply<SyncedValueOutcome<ArcContext>>,
) -> Result<(), eyre::Error> {
    // Ordinary errors trigger a sync retry. A `HaltAndWait` chain anomaly must
    // instead shut the node down, so it is propagated after replying to consensus.
    let (outcome, halt) = match on_process_synced_value(
        EnginePayloadValidator::new(engine, state.metrics()),
        state.store(),
        state.store(),
        state.persistence_meter(),
        state.metrics(),
        height,
        round,
        proposer,
        value_bytes,
        state.previous_block,
    )
    .await
    {
        Ok(Some(proposal)) => {
            // Mark this height as synced for proposal monitoring.
            if proposal.validity.is_valid() {
                state.mark_height_synced(height);
            }
            (SyncedValueOutcome::Verdict(proposal), None)
        }
        Ok(None) => (SyncedValueOutcome::PeerFault, None),
        Err(e) if e.downcast_ref::<HaltAndWait>().is_some() => {
            error!(%height, %round, %proposer, "ProcessSyncedValue: chain anomaly, halting: {e:#}");
            (SyncedValueOutcome::LocalTransientError, Some(e))
        }
        Err(e) => {
            error!(%height, %round, %proposer, "ProcessSyncedValue failed: {e:#}");
            (SyncedValueOutcome::LocalTransientError, None)
        }
    };

    if let Err(e) = reply.send(outcome) {
        error!("🔴 ProcessSyncedValue: Failed to send reply: {e:?}");
    }

    match halt {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Processes a synced value received from a peer.
///
/// Decodes the raw bytes into an [`ExecutionPayloadV3`], validates it via
/// [`establish_block_validity`], and stores the resulting [`ConsensusBlock`] as an
/// undecided block. If the engine rejects the payload, the invalid-payload
/// **metric** is incremented but no [`InvalidPayload`] forensic row is written:
/// synced bytes are unsigned and a peer can name an honest proposer. The block
/// is kept with [`Validity::Invalid`] so consensus can proceed with the correct
/// validity information.
///
/// Returns `Ok(None)` for bytes that do not SSZ-decode, and for a self-reported hash
/// that is not canonical. Consensus reads that reply as a peer fault, penalizes the
/// peer and requests the height from another one.
///
/// A payload that breaks a binding rule returns the `Invalid` verdict instead, so the
/// value-id check after this reply can tell a bad peer from a real anomaly.
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
    previous_block: Option<ExecutionBlock>,
) -> eyre::Result<Option<ProposedValue<ArcContext>>> {
    let payload = match ExecutionPayloadV3::from_ssz_bytes(&value_bytes) {
        Ok(payload) => payload,
        Err(e) => {
            warn!(
                %height, %round, %proposer,
                "Failed to decode synced value into an execution payload: {e:?}",
            );
            metrics.inc_invalid_payloads_count(InvalidPayloadSource::SyncDecode);

            return Ok(None);
        }
    };

    // Build the block before validation so that
    // `establish_block_validity` can attribute the verdict to this payload.
    let mut block = ConsensusBlock {
        height,
        round,
        valid_round: Round::Nil,
        proposer,
        execution_payload: payload,
        validity: Validity::Valid,
        signature: None,
    };

    let verdict = establish_block_validity(
        &engine,
        &block,
        previous_block.as_ref(),
        &invalid_payloads_repo,
        metrics,
        // Synced `value_bytes` are unsigned. A forensic row would name the
        // certificate proposer, who did not sign these bytes.
        PayloadOrigin::UnverifiedSync,
    )
    .await
    .wrap_err_with(|| {
        format!(
            "Payload validation failed on block built from synced value at height={}, round={} received from {}",
            height, round, proposer,
        )
    })?;

    let validity = verdict.validity();
    block.validity = validity;

    let block_hash = block.self_reported_block_hash();

    if !validity.is_valid() {
        error!(%height, %round, %proposer, %block_hash, "❌ Received invalid payload via sync");
    }

    // Don't key a non-canonical block into the undecided table. Unsigned
    // sync does not persist an `InvalidPayload` row (a peer can name any
    // proposer); the metric above still records the rejection.
    if !block.may_be_stored_as_undecided() {
        warn!(
            %height, %round, %proposer, %block_hash,
            "Synced value self-reported hash is not canonical; not storing as undecided",
        );
        return Ok(None);
    }

    // An unbound payload keeps the `Invalid` verdict and goes to consensus, which
    // compares the value id to the certificate after this reply. That comparison
    // separates the two causes this code cannot: bytes no certificate covers, and a
    // payload the network really committed. A peer fault here would be right for the
    // first only, and would blame honest peers for a state this node holds.
    //
    // It skips dedup on the way: a row already stored `Valid` would otherwise answer
    // for the fresh `Invalid` verdict and carry the payload forward.
    let is_unbound = matches!(verdict, BlockVerdict::Unbound(_));

    // If an undecided block for the synced value's (height, hash) already exists,
    // reconcile the stored verdict with the fresh one via the shared flip policy.
    // The lookup is by hash across all rounds because a locally re-evaluated row
    // can belong to a different round than the synced value.
    let existing = if is_unbound {
        None
    } else {
        undecided_blocks_repo
            .get_by_hash(height, block_hash)
            .await
            .wrap_err_with(|| {
                format!(
                    "Failed to query undecided blocks repo for dedup at \
                     height={height}, block_hash={block_hash}"
                )
            })?
    };

    if let Some(existing) = existing {
        match resolve_verdict_flip(existing.validity, VerdictOutcome::Deterministic(validity)) {
            FlipResolution::KeepStored => {
                // Consensus needs the synced round to find the commit certificate.
                // `decide` finds the stored payload by hash across rounds.
                return Ok(Some(ProposedValue::from(&block)));
            }
            FlipResolution::AcceptFresh => {
                // Deterministic Invalid -> Valid: the stored Invalid never
                // satisfied persistence, so fall through to persist the fresh
                // Valid verdict and wait for the EL.
                warn!(
                    %height, %round, %proposer, %block_hash,
                    from = ?existing.validity, to = ?validity,
                    "Synced value verdict flipped Invalid -> Valid vs stored undecided block; accepting fresh verdict",
                );
                metrics.inc_verdict_flip_count(VerdictFlip::InvalidToValid);
            }
            FlipResolution::HaltAnomaly => {
                // Deterministic Valid -> Invalid: a block we stored as Valid is now
                // rejected against the same parent state — replay divergence. Do
                // not silently overwrite; fail-stop for operator intervention.
                // Recovery is via EL-fork tooling.
                error!(
                    %height, %round, %proposer, %block_hash,
                    "🛑 Chain anomaly: synced value flipped Valid -> Invalid vs stored undecided block; halting",
                );
                metrics.inc_verdict_flip_count(VerdictFlip::ValidToInvalid);
                return Err(HaltAndWait::new(format!(
                    "deterministic Valid->Invalid verdict flip on synced value \
                     at height={height} round={round} block_hash={block_hash}"
                ))
                .into());
            }
        }
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

    /// Sets up the dedup query (`get_by_hash`) on an
    /// `UndecidedBlocksRepository` mock to return `None` so the main path
    /// flows through to `store_undecided_block`. Use in tests that are not
    /// specifically exercising the dedup race.
    fn expect_no_undecided_dedup_hit(mock: &mut MockUndecidedBlocksRepository) {
        mock.expect_get_by_hash().returning(|_, _| Ok(None));
    }

    /// Builds an arbitrary payload that carries `height` as its block number.
    fn payload_at(u: &mut Unstructured, height: Height) -> ExecutionPayloadV3 {
        let mut payload = ExecutionPayloadV3::arbitrary(u).unwrap();
        payload.payload_inner.payload_inner.block_number = height.as_u64();
        payload
    }

    /// Builds a payload for `height` whose embedded `block_hash` equals the hash
    /// recomputed from its contents.
    fn canonical_payload(u: &mut Unstructured, height: Height) -> ExecutionPayloadV3 {
        let mut payload = payload_at(u, height);
        let canonical = arc_consensus_types::block::canonical_block_hash(&payload)
            .expect("recompute canonical hash");
        payload.payload_inner.payload_inner.block_hash = canonical;
        payload
    }

    /// A genuine block from height 5, synced as the value for height 11. The
    /// verdict is `Invalid` and the engine never sees the payload.
    #[tokio::test]
    async fn on_process_synced_value_rejects_a_payload_from_another_height() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(11);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = canonical_payload(&mut u, Height::new(5));
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let mut engine = MockPayloadValidator::new();
        engine.expect_validate_payload().times(0);

        let mut undecided = MockUndecidedBlocksRepository::new();
        expect_no_undecided_dedup_hit(&mut undecided);
        undecided
            .expect_store_undecided_block()
            .times(1)
            .returning(|_| Ok(()));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let metrics = AppMetrics::default();
        let outcome = on_process_synced_value(
            engine,
            undecided,
            invalid,
            NoopPersistenceMeter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
            None,
        )
        .await
        .expect("an unbound synced value is a verdict, not a failure");

        let proposal = outcome.expect("the verdict must reach consensus");
        assert_eq!(proposal.validity, Validity::Invalid);
        assert_eq!(
            metrics.get_invalid_payloads_count_by_source(InvalidPayloadSource::PayloadHeight),
            1,
        );
    }

    /// A payload at the right height that extends some other block. This is the
    /// parent rule reaching an ingestion path, and the reason it records.
    #[tokio::test]
    async fn on_process_synced_value_rejects_a_payload_that_extends_another_block() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(11);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = canonical_payload(&mut u, height);
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        // The immediate predecessor, and not the block the payload extends.
        let previous_block = ExecutionBlock {
            block_hash: arc_consensus_types::B256::repeat_byte(0xAB),
            block_number: 10,
            parent_hash: arc_consensus_types::B256::ZERO,
            timestamp: 0,
        };

        let mut engine = MockPayloadValidator::new();
        engine.expect_validate_payload().times(0);

        let mut undecided = MockUndecidedBlocksRepository::new();
        expect_no_undecided_dedup_hit(&mut undecided);
        undecided
            .expect_store_undecided_block()
            .times(1)
            .returning(|_| Ok(()));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let metrics = AppMetrics::default();
        let outcome = on_process_synced_value(
            engine,
            undecided,
            invalid,
            NoopPersistenceMeter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
            Some(previous_block),
        )
        .await
        .expect("an unbound synced value is a verdict, not a failure");

        let proposal = outcome.expect("the verdict must reach consensus");
        assert_eq!(proposal.validity, Validity::Invalid);
    }

    /// A row stored `Valid` while `previous_block` lagged, re-evaluated once the
    /// parent rule applies. Dedup must not answer for the fresh verdict: the stored
    /// row would carry the payload forward as `Valid`.
    #[tokio::test]
    async fn on_process_synced_value_does_not_dedup_against_a_stored_row_after_a_binding_error() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(11);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = canonical_payload(&mut u, height);
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let previous_block = ExecutionBlock {
            block_hash: arc_consensus_types::B256::repeat_byte(0xAB),
            block_number: 10,
            parent_hash: arc_consensus_types::B256::ZERO,
            timestamp: 0,
        };

        let mut engine = MockPayloadValidator::new();
        engine.expect_validate_payload().times(0);

        let mut undecided = MockUndecidedBlocksRepository::new();
        // Dedup reads the stored row. A binding failure never asks for it, so a row
        // already stored `Valid` cannot answer in place of the fresh verdict.
        undecided.expect_get_by_hash().times(0);
        undecided
            .expect_store_undecided_block()
            .times(1)
            .withf(|b: &ConsensusBlock| b.validity == Validity::Invalid)
            .returning(|_| Ok(()));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let metrics = AppMetrics::default();
        let outcome = on_process_synced_value(
            engine,
            undecided,
            invalid,
            NoopPersistenceMeter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
            Some(previous_block),
        )
        .await
        .expect("an unbound synced value is a verdict, not a failure");

        let proposal = outcome.expect("the verdict must reach consensus");
        assert_eq!(proposal.validity, Validity::Invalid);
        assert_eq!(
            metrics.get_invalid_payloads_count_by_source(InvalidPayloadSource::PayloadParent),
            1,
        );
    }

    /// Batch value sync delivers heights ahead of the node's previous block. That
    /// block is not the parent of the payload, so only the engine decides here.
    #[tokio::test]
    async fn on_process_synced_value_skips_the_parent_rule_for_a_height_ahead() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(11);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = canonical_payload(&mut u, height);
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let previous_block = ExecutionBlock {
            block_hash: arc_consensus_types::B256::repeat_byte(0xAB),
            block_number: 7,
            parent_hash: arc_consensus_types::B256::ZERO,
            timestamp: 0,
        };

        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .times(1)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut undecided = MockUndecidedBlocksRepository::new();
        expect_no_undecided_dedup_hit(&mut undecided);
        undecided
            .expect_store_undecided_block()
            .times(1)
            .returning(|_| Ok(()));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

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
            Some(previous_block),
        )
        .await
        .expect("should succeed")
        .expect("the verdict reaches consensus");

        assert_eq!(proposal.validity, Validity::Valid);
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    async fn test_on_process_synced_value_validity(
        result: PayloadValidationResult,
        expected: Validity,
    ) {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = canonical_payload(&mut u, height);
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

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

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
            None,
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

    /// The value-sync path applies no clock-skew check: a value whose timestamp is
    /// far ahead of local time still validates execution-`Valid` and is stored
    /// `Valid`, so a clock-lagging node adopts a certificate-backed block with no
    /// restart. The skew guard lives only on the live prevote path.
    #[tokio::test]
    async fn on_process_synced_value_ignores_clock_skew_for_future_timestamp() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(9);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);

        // Stamp the payload well beyond any clock-skew tolerance ahead of now.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut payload = payload_at(&mut u, height);
        payload.payload_inner.payload_inner.timestamp = now + 3600;
        payload.payload_inner.payload_inner.block_hash =
            arc_consensus_types::block::canonical_block_hash(&payload)
                .expect("recompute canonical hash");
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .times(1)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut undecided = MockUndecidedBlocksRepository::new();
        expect_no_undecided_dedup_hit(&mut undecided);
        undecided
            .expect_store_undecided_block()
            .withf(|b: &ConsensusBlock| b.validity == Validity::Valid)
            .times(1)
            .returning(|_| Ok(()));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

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
            None,
        )
        .await
        .expect("sync path must not fail on a future-timestamp value")
        .expect("the verdict reaches consensus");

        assert_eq!(
            proposal.validity,
            Validity::Valid,
            "the value-sync path ignores clock skew and adopts the value",
        );
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    /// A synced value whose self-reported hash does not match its payload
    /// contents is recorded as invalid and never stored as an undecided block,
    /// so it cannot overwrite a valid block advertised under the same hash.
    #[tokio::test]
    async fn on_process_synced_value_non_canonical_hash_is_not_stored() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);

        let mut payload = canonical_payload(&mut u, height);
        payload.payload_inner.payload_inner.block_hash =
            arc_consensus_types::B256::repeat_byte(0xAB);
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let mut engine = MockPayloadValidator::new();
        engine.expect_validate_payload().returning(|_| {
            Ok(PayloadValidationResult::Invalid {
                reason: "block hash mismatch".into(),
            })
        });

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
            None,
        )
        .await
        .expect("Failed to process synced value");

        assert!(result.is_none());
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
        invalid.expect_append().times(0);

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
            None,
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
        let payload = payload_at(&mut u, height);
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
            None,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn test_on_process_synced_value_store_error() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let payload = payload_at(&mut u, height);
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
            None,
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
        let payload = payload_at(&mut u, height);
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
            None,
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
        let payload = canonical_payload(&mut u, height);
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
        invalid.expect_append().times(0);

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
            None,
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
        let payload = payload_at(&mut u, height);
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
            None,
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
    ///   - when the fresh verdict matches the stored one, skip the redundant
    ///     `store_undecided_block` upsert and the `wait_for_persisted_block`
    ///     call and return the existing block's validity.
    /// Verdicts can diverge (validation is state-dependent), so a mismatch is
    /// reconciled via `resolve_verdict_flip` — see the flip tests below.
    #[tokio::test]
    async fn on_process_synced_value_dedups_against_existing_undecided_block() {
        let mut u = Unstructured::new(&[7u8; 512]);

        let height = Height::new(42);
        let round = Round::new(0);
        let proposer = Address::new([1u8; 20]);
        let payload = payload_at(&mut u, height);
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
            .expect_get_by_hash()
            .with(eq(height), eq(block_hash))
            .times(1)
            .return_once(move |_, _| Ok(Some(existing_block)));
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
            None,
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

    /// A dedup hit on a row stored at another round replies with the round and
    /// proposer of the synced value.
    #[tokio::test]
    async fn on_process_synced_value_dedup_across_rounds_returns_synced_round() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(42);
        let stored_round = Round::new(1);
        let synced_round = Round::new(2);
        let stored_proposer = Address::new([1u8; 20]);
        let synced_proposer = Address::new([2u8; 20]);
        let payload = canonical_payload(&mut u, height);
        let block_hash = payload.payload_inner.payload_inner.block_hash;
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let existing_block = ConsensusBlock {
            height,
            round: stored_round,
            valid_round: Round::Nil,
            proposer: stored_proposer,
            execution_payload: payload,
            validity: Validity::Valid,
            signature: None,
        };

        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .times(1)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_hash()
            .with(eq(height), eq(block_hash))
            .times(1)
            .return_once(move |_, _| Ok(Some(existing_block)));
        undecided.expect_store_undecided_block().times(0);

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        // No persistence wait — the stored row already satisfied it.
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
            synced_round,
            synced_proposer,
            value_bytes,
            None,
        )
        .await
        .expect("should succeed via dedup short-circuit")
        .expect("expected Some(proposal) on dedup hit");

        assert_eq!(proposal.height, height);
        assert_eq!(proposal.round, synced_round);
        assert_eq!(proposal.proposer, synced_proposer);
        assert_eq!(proposal.validity, Validity::Valid);
        assert_eq!(proposal.value, Value::new(block_hash));
    }

    /// Dedup hit where the fresh verdict flips Invalid -> Valid vs the stored
    /// block: accept the fresh Valid verdict, persist it, and wait for the EL.
    #[tokio::test]
    async fn on_process_synced_value_dedup_invalid_to_valid_accepts_fresh() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(42);
        let round = Round::new(0);
        let proposer = Address::new([1u8; 20]);
        let payload = canonical_payload(&mut u, height);
        let block_hash = payload.payload_inner.payload_inner.block_hash;
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        let existing_block = ConsensusBlock {
            height,
            round,
            valid_round: Round::Nil,
            proposer,
            execution_payload: payload,
            validity: Validity::Invalid,
            signature: None,
        };

        let mut engine = MockPayloadValidator::new();
        engine
            .expect_validate_payload()
            .times(1)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_hash()
            .with(eq(height), eq(block_hash))
            .times(1)
            .return_once(move |_, _| Ok(Some(existing_block)));
        // The fresh Valid verdict is persisted, overwriting the stored Invalid.
        undecided
            .expect_store_undecided_block()
            .withf(|b| b.validity == Validity::Valid)
            .times(1)
            .returning(|_| Ok(()));

        // A Valid fresh verdict writes no forensic record.
        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        // Valid block falls through to the persistence wait.
        let mut persistence_meter = MockPersistenceMeter::new();
        persistence_meter
            .expect_wait_for_persisted_block()
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
            None,
        )
        .await
        .expect("Invalid -> Valid dedup flip should succeed");

        let proposal = proposal.expect("expected Some(proposal)");
        assert_eq!(proposal.validity, Validity::Valid);
        assert_eq!(proposal.value, Value::new(block_hash));
        assert_eq!(
            metrics.get_verdict_flip_count(VerdictFlip::InvalidToValid),
            1
        );
    }

    /// Dedup hit where the fresh verdict flips Valid -> Invalid vs the stored
    /// block: a chain anomaly. The node fail-stops (HaltAndWait) and does not
    /// overwrite the stored verdict.
    #[tokio::test]
    async fn on_process_synced_value_dedup_valid_to_invalid_halts() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(42);
        let round = Round::new(0);
        let proposer = Address::new([1u8; 20]);
        let payload = canonical_payload(&mut u, height);
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

        let mut engine = MockPayloadValidator::new();
        engine.expect_validate_payload().times(1).returning(|_| {
            Ok(PayloadValidationResult::Invalid {
                reason: "rejected on re-check".into(),
            })
        });

        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_hash()
            .with(eq(height), eq(block_hash))
            .times(1)
            .return_once(move |_, _| Ok(Some(existing_block)));
        // No overwrite on the anomaly.
        undecided.expect_store_undecided_block().times(0);

        // Unsigned sync does not write a forensic record (a peer can name any proposer).
        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let mut persistence_meter = MockPersistenceMeter::new();
        persistence_meter.expect_wait_for_persisted_block().times(0);

        let metrics = AppMetrics::default();
        let err = on_process_synced_value(
            engine,
            undecided,
            invalid,
            persistence_meter,
            &metrics,
            height,
            round,
            proposer,
            value_bytes,
            None,
        )
        .await
        .expect_err("Valid -> Invalid dedup flip should halt");

        assert!(
            err.downcast_ref::<HaltAndWait>().is_some(),
            "error should be a HaltAndWait, got: {err:#}",
        );
        assert_eq!(
            metrics.get_verdict_flip_count(VerdictFlip::ValidToInvalid),
            1
        );
    }

    /// Cross-round dedup: the stored undecided block sits at a *different* round
    /// than the synced value's decided round (e.g. the cluster decided V at round 3
    /// after restarting, but the only locally re-evaluated row is round 0's). The
    /// dedup lookup is keyed on `(height, hash)`, so it still finds the stored row
    /// and the Valid -> Invalid flip is caught and halted, rather than silently
    /// inserting a duplicate. A `(round, hash)` lookup would have missed it.
    #[tokio::test]
    async fn on_process_synced_value_dedup_matches_across_rounds() {
        let mut u = Unstructured::new(&[0u8; 512]);

        let height = Height::new(42);
        let stored_round = Round::new(0);
        let synced_round = Round::new(3);
        let proposer = Address::new([1u8; 20]);
        let payload = canonical_payload(&mut u, height);
        let block_hash = payload.payload_inner.payload_inner.block_hash;
        let value_bytes = Bytes::from(payload.as_ssz_bytes());

        // Stored at round 0 with a Valid verdict.
        let existing_block = ConsensusBlock {
            height,
            round: stored_round,
            valid_round: Round::Nil,
            proposer,
            execution_payload: payload,
            validity: Validity::Valid,
            signature: None,
        };

        // The engine now rejects the same bytes on re-check.
        let mut engine = MockPayloadValidator::new();
        engine.expect_validate_payload().times(1).returning(|_| {
            Ok(PayloadValidationResult::Invalid {
                reason: "rejected on re-check".into(),
            })
        });

        // The dedup lookup is by (height, hash) only — it returns the round-0 row
        // even though the synced value arrives at round 3.
        let mut undecided = MockUndecidedBlocksRepository::new();
        undecided
            .expect_get_by_hash()
            .with(eq(height), eq(block_hash))
            .times(1)
            .return_once(move |_, _| Ok(Some(existing_block)));
        // No overwrite on the anomaly.
        undecided.expect_store_undecided_block().times(0);

        // Unsigned sync does not write a forensic record (a peer can name any proposer).
        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let mut persistence_meter = MockPersistenceMeter::new();
        persistence_meter.expect_wait_for_persisted_block().times(0);

        let metrics = AppMetrics::default();
        let err = on_process_synced_value(
            engine,
            undecided,
            invalid,
            persistence_meter,
            &metrics,
            height,
            synced_round,
            proposer,
            value_bytes,
            None,
        )
        .await
        .expect_err("cross-round Valid -> Invalid flip should halt");

        assert!(
            err.downcast_ref::<HaltAndWait>().is_some(),
            "error should be a HaltAndWait, got: {err:#}",
        );
        assert_eq!(
            metrics.get_verdict_flip_count(VerdictFlip::ValidToInvalid),
            1
        );
    }
}
