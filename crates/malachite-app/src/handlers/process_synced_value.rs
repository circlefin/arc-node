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

use bytes::Bytes;
use eyre::Context;
use ssz::Decode;
use tracing::error;

use malachitebft_app_channel::app::types::core::Round;
use malachitebft_app_channel::app::types::ProposedValue;
use malachitebft_app_channel::Reply;

use alloy_rpc_types_engine::ExecutionPayloadV3;
use arc_consensus_types::{Address, ArcContext, Height};
use arc_eth_engine::engine::Engine;

use malachitebft_app_channel::app::types::core::Validity;

use crate::block::ConsensusBlock;
use crate::payload::{validate_consensus_block, EnginePayloadValidator, PayloadValidator};
use crate::state::State;
use crate::store::repositories::{InvalidPayloadsRepository, UndecidedBlocksRepository};
use arc_consensus_db::invalid_payloads::InvalidPayload;

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
    let proposal = on_process_synced_value(
        EnginePayloadValidator::new(engine, state.metrics()),
        state.store(),
        state.store(),
        height,
        round,
        proposer,
        value_bytes,
    )
    .await?;

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
/// undecided block. If the engine rejects the payload, an [`InvalidPayload`]
/// [`InvalidPayload`](crate::invalid_payloads::InvalidPayload) record is persisted
/// through `store` and the block is kept with [`Validity::Invalid`] so that
/// consensus can proceed with the correct validity information.
///
/// Returns `Ok(None)` when the raw bytes cannot be SSZ-decoded (the error is logged
/// but not propagated).
async fn on_process_synced_value(
    engine: impl PayloadValidator,
    undecided_blocks_repo: impl UndecidedBlocksRepository,
    invalid_payloads_repo: impl InvalidPayloadsRepository,
    height: Height,
    round: Round,
    proposer: Address,
    value_bytes: Bytes,
) -> eyre::Result<Option<ProposedValue<ArcContext>>> {
    let payload = match ExecutionPayloadV3::from_ssz_bytes(&value_bytes) {
        Ok(payload) => payload,
        Err(e) => {
            let invalid =
                InvalidPayload::new_without_payload(height, round, proposer, &format!("{e:?}"));

            invalid_payloads_repo.append(invalid).await.wrap_err_with(|| {
                format!(
                    "Failed to store invalid payload after receiving synced value (height={height}, round={round}, proposer={proposer})",
                )
            })?;

            error!(
                %height, %round, %proposer,
                "Failed to decode synced value into an execution payload: {e:?}",
            );

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
        &engine, &block, &invalid_payloads_repo,
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

    let proposal = ProposedValue::from(&block);

    undecided_blocks_repo.store(block).await.wrap_err_with(|| {
        format!(
            "Failed to store undecided block {} synced from the network for height={}, round={}, proposer={}",
            block_hash, height, round, proposer,
        )
    })?;

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
    use bytes::Bytes;
    use malachitebft_core_types::Validity;
    use mockall::predicate::*;
    use ssz::Encode;
    use std::io;

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
        undecided
            .expect_store()
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

        let Some(proposal) = on_process_synced_value(
            engine,
            undecided,
            invalid,
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
        undecided.expect_store().times(0);
        invalid.expect_append().times(1).returning(|_| Ok(()));

        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let value_bytes = Bytes::from(vec![0u8; 10]);

        let proposal = on_process_synced_value(
            engine,
            undecided,
            invalid,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await
        .expect("Failed to process synced value");

        assert!(proposal.is_none());
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
        undecided
            .expect_store()
            .times(1)
            .returning(|_| Err(io::Error::other("Simulated store error")));

        let mut invalid = MockInvalidPayloadsRepository::new();
        invalid.expect_append().times(0);

        let result = on_process_synced_value(
            engine,
            undecided,
            invalid,
            height,
            round,
            proposer,
            value_bytes,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().downcast_ref::<io::Error>().is_some());
    }
}
