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

use std::time::{Duration, Instant};

use eyre::{eyre, Context};
use itertools::Itertools;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use malachitebft_app_channel::app::streaming::StreamId;
use malachitebft_app_channel::app::types::core::Round;
use malachitebft_app_channel::app::types::LocallyProposedValue;
use malachitebft_app_channel::{NetworkMsg, Reply};
use malachitebft_core_types::Validity;

use arc_consensus_types::{Address, ArcContext, Height};
use arc_eth_engine::deadline::EngineDeadline;
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_signer::ArcSigningProvider;

use crate::block::ConsensusBlock;
use crate::metrics::AppMetrics;
use crate::payload::{
    generate_payload_with_retry, validate_consensus_block, EnginePayloadGenerator,
    EnginePayloadValidator,
};
use crate::proposal_parts::{prepare_stream, stream_proposal};
use crate::state::State;
use crate::store::repositories::UndecidedBlocksRepository;
use crate::store::Store;
use crate::utils::pretty::PrettyPayload;

type NetworkHandle = mpsc::Sender<NetworkMsg<ArcContext>>;

/// Handles the `GetValue` message from the consensus engine.
///
/// Called when the consensus engine requests a new value to propose in a given height and round.
///
/// Malachite assumes that the application is deterministic when providing proposals, namely replies
/// to the `getValue()` primitive implemented by this handler. This requires storing and re-using
/// previously produced values.
///
/// - First, check if there is a previously built block for the given height and round.
/// - If so, to adhere to the crash-recovery model, the same block must be re-proposed.
/// - Otherwise, which should be common case, build a new block using the execution engine.
/// - Start a new stream to propagate the proposal, with the stored or new block, to all processes.
/// - Returns to the consensus engine the stored or new block's hash as the proposed value.
pub async fn handle(
    state: &mut State,
    network: NetworkHandle,
    engine: &Engine,
    height: Height,
    round: Round,
    timeout: Duration,
    reply: Reply<LocallyProposedValue<ArcContext>>,
) -> eyre::Result<()> {
    let metrics = state.metrics().clone();
    let store = state.store().clone();

    let address = state.address();
    let fee_recipient = state.fee_recipient();
    let stream_id = state.next_stream_id();
    let previous_block = state.previous_block.as_ref();
    let signing_provider = state.signing_provider();

    let proposed_value = on_get_value(
        network,
        engine,
        metrics,
        store,
        height,
        round,
        address,
        previous_block,
        fee_recipient,
        signing_provider,
        stream_id,
        timeout,
    )
    .await?;

    if let Some(proposed_value) = proposed_value {
        if round.as_i64() == 0 {
            if let Some(monitor) = &mut state.proposal_monitor {
                debug_assert_eq!(monitor.height, height, "proposal monitor height mismatch");
                monitor.record_proposal(proposed_value.value.id());
            } else {
                warn!(%height, %round, "No proposal monitor present");
            }
        }

        // Feed the byzantine amnesia state machine with the value we're about
        // to propose locally, so a later nil-prevote at this (height, round)
        // can be overridden with `NilOrVal::Val(value_id)`. No-op when the
        // byzantine feature is off or the amnesia trigger is inactive.
        #[cfg(feature = "byzantine")]
        if let Some(byz) = &state.ctx.byzantine {
            byz.amnesia
                .record_proposed_value(height, round, proposed_value.value.id());
        }

        if let Err(e) = reply.send(proposed_value) {
            error!("🔴 GetValue: Failed to send reply: {e:?}");
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn on_get_value(
    network: NetworkHandle,
    engine: &Engine,
    metrics: AppMetrics,
    store: Store,
    height: Height,
    round: Round,
    address: Address,
    previous_block: Option<&ExecutionBlock>,
    fee_recipient: Address,
    signing_provider: &ArcSigningProvider,
    stream_id: StreamId,
    timeout: Duration,
) -> eyre::Result<Option<LocallyProposedValue<ArcContext>>> {
    let block = get_previously_built_block(&store, address, height, round)
        .await
        .wrap_err_with(|| {
            format!(
                "Proposer failed to get previously built block for height {} and round {}",
                height, round,
            )
        })?;

    let mut block = match block {
        Some(block) => {
            info!(block_hash = %block.block_hash(), "✅ Using previously built block");
            block
        }
        None => {
            info!(%height, %round, "🌈 Building new block");

            let previous_block = previous_block.ok_or_else(|| {
                eyre!("No previous block available to build new block at height={height} and round={round}")
            })?;

            // Engine API calls below share the round's full propose budget;
            // this outer timeout is the binding deadline (per-call timeouts
            // never undercut it, see `EngineDeadline::call_timeout`).
            let deadline = EngineDeadline::within(timeout);

            let task = build_and_validate_block(
                engine,
                &metrics,
                &store,
                height,
                round,
                address,
                previous_block,
                &fee_recipient,
                deadline,
            );

            let result = match tokio::time::timeout_at(deadline.timeout_at(), task).await {
                Ok(result) => result,
                Err(_) => {
                    error!(%height, %round, "⏰ Proposer timed out while building block after {timeout:?}");
                    return Ok(None);
                }
            };

            result.wrap_err_with(|| {
                format!("Proposer failed to build new block at height={height} and round={round}")
            })?
        }
    };

    let proposed_value = LocallyProposedValue::from(&block);

    debug!(
        %height, %round,
        block_size = %block.size_bytes(),
        payload_size = %block.payload_size(),
        "🎁 Sending proposal: {proposed_value:?}",
    );

    let block_hash = block.block_hash();

    let (stream_messages, signature) = prepare_stream(stream_id, signing_provider, &block)
        .await
        .wrap_err_with(|| {
            format!(
                "Proposer failed to prepare stream for block {block_hash} \
                it wants to propose at height={height}, round={round}",
            )
        })?;

    // Store the block with its signature
    block.signature = Some(signature);
    store
        .store_undecided_block(block)
        .await
        .wrap_err_with(|| format!("Proposer failed to store block {block_hash}"))?;

    tokio::spawn(async move {
        if let Err(e) = stream_proposal(network, height, round, stream_messages).await {
            error!(%height, %round, "🔴 Failed to stream proposal parts: {e:#}");
        }
    });

    debug!(%height, %round, "✅ Proposal sent");

    Ok(Some(proposed_value))
}

/// Builds a new execution payload and validates it via the Engine API.
///
/// If the engine rejects the payload, an [`InvalidPayload`] record is
/// persisted (handled by [`validate_consensus_block`]) and the function
/// returns an error since a self-built block should never be invalid.
#[allow(clippy::too_many_arguments)]
async fn build_and_validate_block(
    engine: &Engine,
    metrics: &AppMetrics,
    store: &Store,
    height: Height,
    round: Round,
    proposer: Address,
    previous_block: &ExecutionBlock,
    fee_recipient: &Address,
    deadline: EngineDeadline,
) -> eyre::Result<ConsensusBlock> {
    let start = Instant::now();

    let block = build_block(
        engine,
        metrics,
        height,
        round,
        proposer,
        previous_block,
        fee_recipient,
        deadline,
    )
    .await?;

    let validator = EnginePayloadValidator::new_with_deadline(engine, metrics, deadline);
    let validity = validate_consensus_block(&validator, &block, store, metrics)
        .await
        .wrap_err_with(|| {
            format!(
                "Payload validation failed on self-built block at height={height}, round={round}: {}",
                block.block_hash()
            )
        })?;

    if !validity.is_valid() {
        return Err(eyre!("Self-built block {} is invalid", block.block_hash()));
    }

    debug!(
        "✅ Proposer validated self-built block {}",
        block.block_hash()
    );

    metrics.observe_block_build_time(start.elapsed().as_secs_f64());

    Ok(block)
}

/// Build a new block.
#[allow(clippy::too_many_arguments)]
pub async fn build_block(
    engine: &Engine,
    metrics: &AppMetrics,
    height: Height,
    round: Round,
    proposer: Address,
    previous_block: &ExecutionBlock,
    fee_recipient: &Address,
    deadline: EngineDeadline,
) -> eyre::Result<ConsensusBlock> {
    let generator = EnginePayloadGenerator {
        engine,
        deadline: Some(deadline),
    }; // TODO: make this configurable

    let execution_payload =
        generate_payload_with_retry(previous_block, fee_recipient, &generator, metrics).await?;

    debug!(
        "🌈 Got execution payload: {:?}",
        PrettyPayload(&execution_payload)
    );

    Ok(ConsensusBlock {
        height,
        round,
        valid_round: Round::Nil,
        proposer,
        validity: Validity::Valid,
        execution_payload,
        signature: None,
    })
}

/// Retrieves a previously built block by a proposer for the given height and round, if any.
///
/// There should be at most one block for a given height, round, and proposer.
/// Produces an error if multiple matching blocks are found in the undecided blocks database.
async fn get_previously_built_block(
    undecided_blocks: impl UndecidedBlocksRepository,
    proposer: Address,
    height: Height,
    round: Round,
) -> eyre::Result<Option<ConsensusBlock>> {
    let blocks = undecided_blocks.get_by_round(height, round).await?;
    let block = blocks
        .into_iter()
        .filter(|p| p.proposer == proposer)
        .at_most_one()
        .map_err(|dups| {
            let hashes: Vec<_> = dups.map(|b| b.block_hash()).collect();
            eyre!("Multiple undecided blocks found for proposer {proposer} at height {height} and round {round}: {hashes:?}")
        })?;

    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    use mockall::predicate::*;

    use alloy_primitives::{Address as AlloyAddress, Bloom, Bytes as AlloyBytes, U256};
    use alloy_rpc_types_engine::{ExecutionPayloadV1, ExecutionPayloadV2, ExecutionPayloadV3};
    use arc_consensus_types::{signing::Signature, B256};
    use malachitebft_core_types::Validity;

    use crate::store::repositories::mocks::MockUndecidedBlocksRepository;

    fn test_execution_payload(block_hash_byte: u8) -> ExecutionPayloadV3 {
        ExecutionPayloadV3 {
            payload_inner: ExecutionPayloadV2 {
                payload_inner: ExecutionPayloadV1 {
                    parent_hash: B256::ZERO,
                    fee_recipient: AlloyAddress::ZERO,
                    state_root: B256::ZERO,
                    receipts_root: B256::ZERO,
                    logs_bloom: Bloom::default(),
                    prev_randao: B256::ZERO,
                    block_number: 1,
                    gas_limit: 30000000,
                    gas_used: 0,
                    timestamp: 1000,
                    extra_data: AlloyBytes::default(),
                    base_fee_per_gas: U256::from(1u64),
                    block_hash: B256::repeat_byte(block_hash_byte),
                    transactions: vec![],
                },
                withdrawals: vec![],
            },
            blob_gas_used: 0,
            excess_blob_gas: 0,
        }
    }

    fn test_block(
        height: Height,
        round: Round,
        proposer: Address,
        block_hash_byte: u8,
    ) -> ConsensusBlock {
        ConsensusBlock {
            height,
            round,
            valid_round: Round::Nil,
            proposer,
            validity: Validity::Valid,
            execution_payload: test_execution_payload(block_hash_byte),
            signature: Some(Signature::test()),
        }
    }

    #[tokio::test]
    async fn returns_none_when_no_blocks_stored() {
        let height = Height::new(1);
        let round = Round::new(0);

        let mut mock = MockUndecidedBlocksRepository::new();
        mock.expect_get_by_round()
            .with(eq(height), eq(round))
            .return_once(|_, _| Ok(vec![]));

        let result = get_previously_built_block(mock, Address::new([1u8; 20]), height, round).await;

        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn returns_block_when_single_match() {
        let height = Height::new(5);
        let round = Round::new(2);
        let proposer = Address::new([1u8; 20]);
        let block = test_block(height, round, proposer, 0xAA);
        let expected_hash = block.block_hash();

        let mut mock = MockUndecidedBlocksRepository::new();
        mock.expect_get_by_round()
            .with(eq(height), eq(round))
            .return_once(move |_, _| Ok(vec![block]));

        let result = get_previously_built_block(mock, proposer, height, round).await;

        let found = result.unwrap().unwrap();
        assert_eq!(found.block_hash(), expected_hash);
        assert_eq!(found.proposer, proposer);
    }

    #[tokio::test]
    async fn returns_none_when_proposer_does_not_match() {
        let height = Height::new(5);
        let round = Round::new(2);
        let stored_proposer = Address::new([1u8; 20]);
        let queried_proposer = Address::new([2u8; 20]);
        let block = test_block(height, round, stored_proposer, 0xAA);

        let mut mock = MockUndecidedBlocksRepository::new();
        mock.expect_get_by_round()
            .with(eq(height), eq(round))
            .return_once(move |_, _| Ok(vec![block]));

        let result = get_previously_built_block(mock, queried_proposer, height, round).await;

        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn returns_matching_block_among_different_proposers() {
        let height = Height::new(5);
        let round = Round::new(2);
        let proposer_a = Address::new([1u8; 20]);
        let proposer_b = Address::new([2u8; 20]);
        let block_a = test_block(height, round, proposer_a, 0xAA);
        let block_b = test_block(height, round, proposer_b, 0xBB);
        let expected_hash = block_a.block_hash();

        let mut mock = MockUndecidedBlocksRepository::new();
        mock.expect_get_by_round()
            .with(eq(height), eq(round))
            .return_once(move |_, _| Ok(vec![block_a, block_b]));

        let result = get_previously_built_block(mock, proposer_a, height, round).await;

        let found = result.unwrap().unwrap();
        assert_eq!(found.block_hash(), expected_hash);
        assert_eq!(found.proposer, proposer_a);
    }

    #[tokio::test]
    async fn errors_when_multiple_blocks_for_same_proposer() {
        let height = Height::new(5);
        let round = Round::new(2);
        let proposer = Address::new([1u8; 20]);
        let block_1 = test_block(height, round, proposer, 0xAA);
        let block_2 = test_block(height, round, proposer, 0xBB);

        let mut mock = MockUndecidedBlocksRepository::new();
        mock.expect_get_by_round()
            .with(eq(height), eq(round))
            .return_once(move |_, _| Ok(vec![block_1, block_2]));

        let result = get_previously_built_block(mock, proposer, height, round).await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Multiple undecided blocks found"),
            "Expected 'Multiple undecided blocks found' in error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn propagates_repository_error() {
        let height = Height::new(5);
        let round = Round::new(2);

        let mut mock = MockUndecidedBlocksRepository::new();
        mock.expect_get_by_round()
            .with(eq(height), eq(round))
            .return_once(|_, _| Err(std::io::Error::other("db connection lost")));

        let result = get_previously_built_block(mock, Address::new([1u8; 20]), height, round).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("db connection lost"));
    }
}
