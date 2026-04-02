// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
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

use eyre::{eyre, Context};
use tracing::{debug, error, info, warn};

use malachitebft_core_types::CommitCertificate;

use arc_consensus_types::{ArcContext, Height};
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;

use crate::block::ConsensusBlock;
use crate::finalize::{BlockFinalizer, EngineBlockFinalizer};
use crate::metrics::AppMetrics;
use crate::state::{Decision, NextHeightInfo, State};
use crate::stats::Stats;
use crate::store::repositories::{DecidedBlocksRepository, UndecidedBlocksRepository};
use crate::store::services::{ProdPruningService, PruningService};
use crate::utils::sync_state::{sync_state, SyncState};

/// Handles the `Decided` message from the consensus engine.
///
/// This is called when the consensus engine has decided on a value for a given height and round.
/// The application processes the decided value, executes the decided block and, based on the
/// output of those steps, stores the decision result as either `Success` or `Failure`.
///
/// The `Finalized` message that will follow this message sends the appropriate `Next` message to
/// consensus to start the next height, or in case of failure, restart the current height.
#[tracing::instrument(
    name = "decided",
    skip_all,
    fields(
        height = %certificate.height,
        round = %certificate.round,
    )
)]
pub async fn handle(
    state: &mut State,
    engine: &Engine,
    certificate: CommitCertificate<ArcContext>,
) -> eyre::Result<()> {
    let decided_height = certificate.height;
    let decided_value_id = certificate.value_id;

    store_proposal_monitor_on_decision(state, decided_height, &decided_value_id).await;

    let (store, metrics, stats) = (state.store(), state.metrics(), state.stats());

    let block_finalizer = EngineBlockFinalizer::new(engine, stats, metrics);
    let pruning_service = ProdPruningService::new(store, &state.config().prune);

    let block = decide(
        block_finalizer,
        store, // undecided blocks repository
        store, // decided blocks repository
        pruning_service,
        certificate,
        stats,
        metrics,
    )
    .await;

    match block {
        Ok(block) => {
            info!("🟢 Successfully committed the decided value");

            let catch_up_threshold = state.env_config().sync_catch_up_threshold;
            let new_sync_state = sync_state(block.timestamp, catch_up_threshold);

            if SyncState::fell_behind(state.sync_state, new_sync_state) {
                debug!("Node fell behind: transitioned from InSync to CatchingUp");
                state.metrics().inc_sync_fell_behind_count();
            }

            state.sync_state = new_sync_state;

            let next_height_info =
                prepare_next_height(decided_height, block, new_sync_state, engine).await?;

            state.decision = Some(Decision::Success(Box::new(next_height_info)));
        }
        Err(e) => {
            error!("🔴 Failed to process decided value: {e:#}");

            state.decision = Some(Decision::Failure(e));
        }
    }

    Ok(())
}

/// Update proposal monitor data upon decision.
async fn store_proposal_monitor_on_decision(
    state: &mut State,
    decided_height: Height,
    decided_value_id: &arc_consensus_types::ValueId,
) {
    let Some(mut monitor) = state.proposal_monitor.take() else {
        warn!(%decided_height, "No proposal monitor found for decided height");
        return;
    };
    assert!(monitor.height == decided_height);

    monitor.mark_decided(decided_value_id);

    if let Err(e) = state.store().store_proposal_monitor_data(monitor).await {
        error!(
            %decided_height,
            "Failed to store proposal monitor data: {e}"
        );
    }
}

/// Commits a value with the given certificate, finalizes the block,
/// updates internal state and moves to the next height.
async fn decide(
    block_finalizer: impl BlockFinalizer,
    undecided_blocks: impl UndecidedBlocksRepository,
    decided_blocks: impl DecidedBlocksRepository,
    pruning_service: impl PruningService,
    certificate: CommitCertificate<ArcContext>,
    stats: &Stats,
    metrics: &AppMetrics,
) -> eyre::Result<ExecutionBlock> {
    let height = certificate.height;
    let round = certificate.round;
    let value_id = certificate.value_id;

    // NOTE: here the node searches for the block with maching value_id from any round
    // It needs to read the complete undecided blocks table, but the expectation is it should be small.
    let block = match undecided_blocks
        .get_first(height, value_id.block_hash())
        .await
    {
        Ok(Some(block)) => block,
        Ok(None) => {
            return Err(eyre!(
                "Cannot find undecided block for certificate with height={height}, round={round}, value_id={value_id}"
            ));
        }
        Err(e) => {
            return Err(eyre!(
                "Failed to retrieve undecided block for certificate with height={height}, round={round}, value_id={value_id}: {e}"
            ));
        }
    };

    debug!(
        "🎁 Block size: {:?}, payload size: {:?}",
        block.size_bytes(),
        block.payload_size()
    );

    // Commit the decision to the store before finalizing the block.
    // This way we ensure that latest decided height >= latest finalized block.
    let new_latest_block = commit(
        block_finalizer,
        decided_blocks,
        pruning_service,
        certificate,
        &block,
    )
    .await
    .wrap_err_with(|| {
        format!("Failed to commit block at height={height}, round={round}, value_id={value_id}")
    })?;

    // Update the latest block
    info!(
        "🔍 Updating latest block with timestamp: {:?}",
        new_latest_block.timestamp
    );

    // Update block finalize time metric
    metrics.observe_block_finalize_time(stats.height_started().elapsed().as_secs_f64());

    Ok(new_latest_block)
}

/// Commits a value with the given certificate, cleanup stale consensus data and prune historical data
async fn commit(
    block_finalizer: impl BlockFinalizer,
    decided_blocks: impl DecidedBlocksRepository,
    pruning_service: impl PruningService,
    certificate: CommitCertificate<ArcContext>,
    block: &ConsensusBlock,
) -> eyre::Result<ExecutionBlock> {
    let certificate_height = certificate.height;
    let certificate_round = certificate.round;
    let value_id = certificate.value_id;

    decided_blocks
        .store(certificate, block.execution_payload.clone(), block.proposer)
        .await
        .wrap_err_with(|| {
            format!("Failed to store decided block at height={certificate_height}, round={certificate_round}, value_id={value_id}")
        })?;

    // Clean up stale consensus data (undecided blocks and pending proposals up to the certificate height)
    if let Err(e) = pruning_service
        .clean_stale_consensus_data(certificate_height)
        .await
    {
        error!("Failed to clean stale consensus data: {e}");
    }

    // Finalize the decided payload
    let (new_latest_block, _latest_valid_hash) =
        block_finalizer.finalize_decided_block(certificate_height, &block.execution_payload)
        .await
        .wrap_err_with(|| {
            format!("Failed to finalize block at height={certificate_height}, round={certificate_round}, value_id={value_id}")
        })?;

    // Prune historical decided certificates if pruning is enabled
    if let Err(e) = pruning_service
        .prune_historical_certs(certificate_height)
        .await
    {
        error!("Failed to prune historical data: {e}");
    }

    // Prune decided blocks
    // NOTE: Always performed, even if pruning is disabled, as CL does not store
    // historical blocks anymore, besides the few needed to recover from EL amnesia.
    if let Err(e) = pruning_service.prune_decided_blocks().await {
        error!("Failed to prune decided blocks: {e}");
    }

    Ok(new_latest_block)
}

/// Prepares the state for the next height by incrementing the height,
/// fetching the new validator set and consensus params,
/// and determining the target block time based on the sync state.
///
/// ## Arguments
/// * `decided_height`: The height that was just decided.
/// * `decided_block`: The block that was just decided.
/// * `engine`: The Ethereum engine to fetch validator sets and consensus params.
async fn prepare_next_height(
    decided_height: Height,
    decided_block: ExecutionBlock,
    sync_state: SyncState,
    engine: &Engine,
) -> eyre::Result<NextHeightInfo> {
    let next_height = decided_height.increment();

    // Fetch the validator set for the next height
    // NOTE: Validator set is fetched at the decided height for the next height
    let validator_set = engine
        .eth
        .get_active_validator_set(decided_height.as_u64())
        .await
        .wrap_err_with(|| {
            format!("Failed to fetch validator set at height {decided_height} for next height {next_height}")
        })?;

    // Fetch the consensus params for the next height
    // NOTE: Consensus params are fetched at the decided height for the next height
    let consensus_params = engine
        .eth
        .get_consensus_params(decided_height.as_u64())
        .await
        .inspect_err(|e| {
            let next_height = decided_height.increment();
            error!(%decided_height, %next_height, "Failed to fetch consensus params for next height: {e}");
            error!(%decided_height, %next_height, "Using default consensus params as a fallback");
        })
        .unwrap_or_default();

    // If we are catching up, we skip the stable block times logic and start the next height right away.
    let target_time = match sync_state {
        SyncState::InSync => consensus_params.target_block_time(),
        SyncState::CatchingUp => {
            debug!("Node is catching up: no target duration for the next height");
            None
        }
    };

    Ok(NextHeightInfo {
        next_height,
        validator_set,
        consensus_params,
        decided_block,
        target_time,
    })
}

#[cfg(test)]
mod tests {

    use eyre::eyre;
    use mockall::predicate::*;

    use alloy_primitives::{Address as AlloyAddress, Bloom, Bytes as AlloyBytes, U256};
    use alloy_rpc_types_engine::{ExecutionPayloadV1, ExecutionPayloadV2, ExecutionPayloadV3};
    use arc_consensus_types::signing::Signature;
    use arc_consensus_types::{Address, Height, Round, ValueId, B256};
    use malachitebft_app_channel::app::types::core::Validity;
    use malachitebft_core_types::{CommitCertificate, CommitSignature};

    use crate::finalize::MockBlockFinalizer;
    use crate::metrics::AppMetrics;
    use crate::stats::Stats;
    use crate::store::repositories::mocks::{
        MockDecidedBlocksRepository, MockUndecidedBlocksRepository,
    };
    use crate::store::services::mocks::MockPruningService;

    use super::*;

    // Helper functions for creating test fixtures
    fn test_execution_block(height: u64, timestamp: u64) -> ExecutionBlock {
        ExecutionBlock {
            block_hash: B256::repeat_byte((height % 256) as u8),
            block_number: height,
            parent_hash: if height > 0 {
                B256::repeat_byte(((height - 1) % 256) as u8)
            } else {
                B256::ZERO
            },
            timestamp,
        }
    }

    fn test_execution_payload(height: u64, timestamp: u64) -> ExecutionPayloadV3 {
        ExecutionPayloadV3 {
            payload_inner: ExecutionPayloadV2 {
                payload_inner: ExecutionPayloadV1 {
                    parent_hash: if height > 0 {
                        B256::repeat_byte(((height - 1) % 256) as u8)
                    } else {
                        B256::ZERO
                    },
                    fee_recipient: AlloyAddress::ZERO,
                    state_root: B256::ZERO,
                    receipts_root: B256::ZERO,
                    logs_bloom: Bloom::default(),
                    prev_randao: B256::ZERO,
                    block_number: height,
                    gas_limit: 30000000,
                    gas_used: 0,
                    timestamp,
                    extra_data: AlloyBytes::default(),
                    base_fee_per_gas: U256::from(1u64),
                    block_hash: B256::repeat_byte((height % 256) as u8),
                    transactions: vec![],
                },
                withdrawals: vec![],
            },
            blob_gas_used: 0,
            excess_blob_gas: 0,
        }
    }

    fn test_consensus_block(height: u64, round: u32, timestamp: u64) -> ConsensusBlock {
        ConsensusBlock {
            height: Height::new(height),
            round: Round::new(round),
            valid_round: Round::new(0),
            proposer: Address::default(),
            validity: Validity::Valid,
            execution_payload: test_execution_payload(height, timestamp),
            signature: Some(Signature::test()),
        }
    }

    fn test_commit_certificate(
        height: u64,
        round: u32,
        block_hash: B256,
    ) -> CommitCertificate<ArcContext> {
        CommitCertificate {
            height: Height::new(height),
            round: Round::new(round),
            value_id: ValueId::new(block_hash),
            commit_signatures: vec![CommitSignature::new(Address::default(), Signature::test())],
        }
    }

    fn test_metrics() -> AppMetrics {
        AppMetrics::default()
    }

    fn test_stats() -> Stats {
        Stats::default()
    }

    // Tests for decide() function

    // Successful decision with valid block found
    #[tokio::test]
    async fn test_decide_success() {
        let height = 5u64;
        let round = 2u32;
        let timestamp = 1000u64;
        let block_hash = B256::repeat_byte((height % 256) as u8);
        let certificate = test_commit_certificate(height, round, block_hash);
        let consensus_block = test_consensus_block(height, round, timestamp);
        let expected_execution_block = test_execution_block(height, timestamp);

        let mut undecided_blocks = MockUndecidedBlocksRepository::new();
        undecided_blocks
            .expect_get_first()
            .with(eq(Height::new(height)), eq(block_hash))
            .return_once(move |_, _| Ok(Some(consensus_block.clone())));

        let mut decided_blocks = MockDecidedBlocksRepository::new();
        decided_blocks
            .expect_store()
            .return_once(move |cert, payload, proposer| {
                assert_eq!(cert.height, Height::new(height));
                assert_eq!(cert.round, Round::new(round));
                assert_eq!(payload.payload_inner.payload_inner.block_number, height);
                assert_eq!(proposer, Address::default());
                Ok(())
            });

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer
            .expect_finalize_decided_block()
            .return_once(move |h, _| {
                assert_eq!(h, Height::new(height));
                Ok((expected_execution_block, block_hash))
            });

        let mut pruning_service = MockPruningService::new();
        pruning_service
            .expect_clean_stale_consensus_data()
            .return_once(|_| Ok(()));
        pruning_service
            .expect_prune_historical_certs()
            .return_once(|_| Ok(vec![]));
        pruning_service
            .expect_prune_decided_blocks()
            .return_once(|| Ok(vec![]));

        let metrics = test_metrics();
        let stats = test_stats();

        let result = decide(
            block_finalizer,
            undecided_blocks,
            decided_blocks,
            pruning_service,
            certificate,
            &stats,
            &metrics,
        )
        .await;

        let block = result.unwrap();
        assert_eq!(block.block_number, height);
        assert_eq!(block.timestamp, timestamp);
    }

    // Block not found in undecided blocks
    #[tokio::test]
    async fn test_decide_block_not_found() {
        let height = 5u64;
        let round = 2u32;
        let block_hash = B256::repeat_byte((height % 256) as u8);
        let certificate = test_commit_certificate(height, round, block_hash);

        let mut undecided_blocks = MockUndecidedBlocksRepository::new();
        undecided_blocks
            .expect_get_first()
            .with(eq(Height::new(height)), eq(block_hash))
            .return_once(|_, _| Ok(None));

        let decided_blocks = MockDecidedBlocksRepository::new();
        let block_finalizer = MockBlockFinalizer::new();
        let pruning_service = MockPruningService::new();
        let metrics = test_metrics();
        let stats = test_stats();

        let result = decide(
            block_finalizer,
            undecided_blocks,
            decided_blocks,
            pruning_service,
            certificate,
            &stats,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Cannot find undecided block"));
    }

    // Repository error when fetching undecided block
    #[tokio::test]
    async fn test_decide_undecided_blocks_fetch_error() {
        let height = 5u64;
        let round = 2u32;
        let block_hash = B256::repeat_byte((height % 256) as u8);
        let certificate = test_commit_certificate(height, round, block_hash);

        let mut undecided_blocks = MockUndecidedBlocksRepository::new();
        undecided_blocks
            .expect_get_first()
            .with(eq(Height::new(height)), eq(block_hash))
            .return_once(|_, _| Err(std::io::Error::other("Database error")));

        let decided_blocks = MockDecidedBlocksRepository::new();
        let block_finalizer = MockBlockFinalizer::new();
        let pruning_service = MockPruningService::new();
        let metrics = test_metrics();
        let stats = test_stats();

        let result = decide(
            block_finalizer,
            undecided_blocks,
            decided_blocks,
            pruning_service,
            certificate,
            &stats,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Failed to retrieve undecided block"));
    }

    // Commit failure propagates error
    #[tokio::test]
    async fn test_decide_commit_failure() {
        let height = 5u64;
        let round = 2u32;
        let timestamp = 1000u64;
        let block_hash = B256::repeat_byte((height % 256) as u8);
        let certificate = test_commit_certificate(height, round, block_hash);
        let consensus_block = test_consensus_block(height, round, timestamp);

        let mut undecided_blocks = MockUndecidedBlocksRepository::new();
        undecided_blocks
            .expect_get_first()
            .with(eq(Height::new(height)), eq(block_hash))
            .return_once(move |_, _| Ok(Some(consensus_block.clone())));

        let mut decided_blocks = MockDecidedBlocksRepository::new();
        decided_blocks
            .expect_store()
            .return_once(|_, _, _| Err(std::io::Error::other("Store failed")));

        let block_finalizer = MockBlockFinalizer::new();
        let pruning_service = MockPruningService::new();
        let metrics = test_metrics();
        let stats = test_stats();

        let result = decide(
            block_finalizer,
            undecided_blocks,
            decided_blocks,
            pruning_service,
            certificate,
            &stats,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Failed to commit block"));
    }

    // Tests for commit() function

    // Successful commit flow
    #[tokio::test]
    async fn test_commit_success() {
        let height = 5u64;
        let round = 2u32;
        let timestamp = 1000u64;
        let block_hash = B256::repeat_byte((height % 256) as u8);
        let certificate = test_commit_certificate(height, round, block_hash);
        let consensus_block = test_consensus_block(height, round, timestamp);
        let expected_execution_block = test_execution_block(height, timestamp);

        let mut decided_blocks = MockDecidedBlocksRepository::new();
        decided_blocks
            .expect_store()
            .return_once(move |cert, payload, proposer| {
                assert_eq!(cert.height, Height::new(height));
                assert_eq!(cert.round, Round::new(round));
                assert_eq!(payload.payload_inner.payload_inner.block_number, height);
                assert_eq!(proposer, Address::default());
                Ok(())
            });

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer
            .expect_finalize_decided_block()
            .return_once(move |h, _| {
                assert_eq!(h, Height::new(height));
                Ok((expected_execution_block, block_hash))
            });

        let mut pruning_service = MockPruningService::new();
        pruning_service
            .expect_clean_stale_consensus_data()
            .with(eq(Height::new(height)))
            .return_once(|_| Ok(()));
        pruning_service
            .expect_prune_historical_certs()
            .with(eq(Height::new(height)))
            .return_once(|_| Ok(vec![Height::new(1), Height::new(2)]));
        pruning_service
            .expect_prune_decided_blocks()
            .return_once(|| Ok(vec![Height::new(0)]));

        let result = commit(
            block_finalizer,
            decided_blocks,
            pruning_service,
            certificate,
            &consensus_block,
        )
        .await;

        let block = result.unwrap();
        assert_eq!(block.block_number, height);
        assert_eq!(block.timestamp, timestamp);
    }

    // DecidedBlocksRepository store failure
    #[tokio::test]
    async fn test_commit_store_failure() {
        let height = 5u64;
        let round = 2u32;
        let timestamp = 1000u64;
        let block_hash = B256::repeat_byte((height % 256) as u8);
        let certificate = test_commit_certificate(height, round, block_hash);
        let consensus_block = test_consensus_block(height, round, timestamp);

        let mut decided_blocks = MockDecidedBlocksRepository::new();
        decided_blocks
            .expect_store()
            .return_once(|_, _, _| Err(std::io::Error::other("Store failed")));

        let block_finalizer = MockBlockFinalizer::new();
        let pruning_service = MockPruningService::new();

        let result = commit(
            block_finalizer,
            decided_blocks,
            pruning_service,
            certificate,
            &consensus_block,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Failed to store decided block"));
    }

    // BlockFinalizer finalization failure
    #[tokio::test]
    async fn test_commit_finalization_failure() {
        let height = 5u64;
        let round = 2u32;
        let timestamp = 1000u64;
        let block_hash = B256::repeat_byte((height % 256) as u8);
        let certificate = test_commit_certificate(height, round, block_hash);
        let consensus_block = test_consensus_block(height, round, timestamp);

        let mut decided_blocks = MockDecidedBlocksRepository::new();
        decided_blocks.expect_store().return_once(|_, _, _| Ok(()));

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer
            .expect_finalize_decided_block()
            .return_once(|_, _| Err(eyre!("Finalization failed")));

        let mut pruning_service = MockPruningService::new();
        pruning_service
            .expect_clean_stale_consensus_data()
            .return_once(|_| Ok(()));

        let result = commit(
            block_finalizer,
            decided_blocks,
            pruning_service,
            certificate,
            &consensus_block,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Failed to finalize block"));
    }

    // Pruning errors are logged but don't fail the operation
    #[tokio::test]
    async fn test_commit_pruning_errors_logged_not_fatal() {
        let height = 5u64;
        let round = 2u32;
        let timestamp = 1000u64;
        let block_hash = B256::repeat_byte((height % 256) as u8);
        let certificate = test_commit_certificate(height, round, block_hash);
        let consensus_block = test_consensus_block(height, round, timestamp);
        let expected_execution_block = test_execution_block(height, timestamp);

        let mut decided_blocks = MockDecidedBlocksRepository::new();
        decided_blocks.expect_store().return_once(|_, _, _| Ok(()));

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer
            .expect_finalize_decided_block()
            .return_once(move |_, _| Ok((expected_execution_block, block_hash)));

        let mut pruning_service = MockPruningService::new();

        // Stale data cleanup fails - but is logged, not fatal
        pruning_service
            .expect_clean_stale_consensus_data()
            .return_once(|_| Err(std::io::Error::other("Cleanup failed")));

        // Historical cert pruning fails - but is logged, not fatal
        pruning_service
            .expect_prune_historical_certs()
            .return_once(|_| Err(std::io::Error::other("Historical prune failed")));

        // Decided blocks pruning must succeed
        pruning_service
            .expect_prune_decided_blocks()
            .return_once(|| Ok(vec![]));

        let result = commit(
            block_finalizer,
            decided_blocks,
            pruning_service,
            certificate,
            &consensus_block,
        )
        .await;

        // Despite pruning errors, the commit should succeed
        let block = result.unwrap();
        assert_eq!(block.block_number, height);
    }
}
