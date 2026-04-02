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

use eyre::{eyre, Context};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use arc_consensus_types::{ArcContext, ConsensusParams, Height, ValidatorSet, ValueSyncConfig};
use arc_eth_engine::capabilities::check_capabilities;
use arc_eth_engine::engine::{Engine, EngineAPI, EthereumAPI};
use arc_eth_engine::json_structures::ExecutionBlock;
use malachitebft_app_channel::Reply;
use malachitebft_core_types::HeightParams;

use crate::finalize::{BlockFinalizer, EngineBlockFinalizer};
use crate::metrics::AppMetrics;
use crate::payload::{EnginePayloadValidator, PayloadValidationResult, PayloadValidator};
use crate::state::State;
use crate::store::repositories::{
    CertificatesRepository, PayloadsRepository, PendingProposalsRepository,
};

/// Handles the `ConsensusReady` message from the consensus engine.
///
/// This is called when the consensus engine is ready to start. The application performs a handshake
/// and replay with the execution client to ensure a consistent state, and then provides the
/// consensus engine with the starting height and the active validator set.
pub async fn handle(
    state: &mut State,
    engine: &Engine,
    reply: Reply<(Height, HeightParams<ArcContext>)>,
) -> eyre::Result<()> {
    let (store, stats, metrics) = (state.store(), state.stats(), state.metrics());
    let max_pending_proposals = max_pending_proposals(&state.config().value_sync);

    let payload_validator = EnginePayloadValidator::new(engine, metrics);
    let block_finalizer = EngineBlockFinalizer::new(engine, stats, metrics);

    let (next_height, next_validator_set, next_consensus_params, previous_block) =
        on_consensus_ready(
            metrics,
            store,
            store,
            store,
            payload_validator,
            block_finalizer,
            engine.api.as_ref(),
            engine.eth.as_ref(),
            max_pending_proposals,
        )
        .await?;

    let timeouts = next_consensus_params.timeouts();

    // Update state with the previous block, current height and validator set
    state.previous_block = Some(previous_block);
    state.current_height = next_height;
    state.set_validator_set(next_validator_set.clone());
    state.set_consensus_params(next_consensus_params);

    let next_height_params = HeightParams::new(next_validator_set, timeouts, None);

    if let Err(e) = reply.send((next_height, next_height_params)) {
        error!("🔴 ConsensusReady: Failed to send reply: {e:?}");
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn on_consensus_ready(
    metrics: &AppMetrics,
    pending_proposals_repository: impl PendingProposalsRepository,
    payloads_repository: impl PayloadsRepository,
    certificates_repository: impl CertificatesRepository,
    payload_validator: impl PayloadValidator,
    block_finalizer: impl BlockFinalizer,
    engine_api: impl EngineAPI,
    ethereum_api: impl EthereumAPI,
    max_pending_proposals: usize,
) -> eyre::Result<(Height, ValidatorSet, ConsensusParams, ExecutionBlock)> {
    // Perform handshake and replay any missing blocks
    let HandshakeResult {
        previous_block,
        latest_height,
        next_height,
    } = handshake_and_replay(
        &payload_validator,
        &block_finalizer,
        &engine_api,
        &ethereum_api,
        &certificates_repository,
        &payloads_repository,
        metrics,
    )
    .await?;

    // Prune pending proposals that are either no longer relevant or exceed storage limits
    enforce_pending_proposals_limit(
        pending_proposals_repository,
        metrics,
        max_pending_proposals,
        next_height,
    )
    .await?;

    // The validator set for the next height is the one at the latest committed height
    let validator_set = ethereum_api
        .get_active_validator_set(latest_height.as_u64())
        .await
        .wrap_err("Failed to get the validator set at ConsensusReady")?;

    // The consensus params for the next height are the same as the latest committed height
    let consensus_params = ethereum_api
        .get_consensus_params(latest_height.as_u64())
        .await
        .inspect_err(|e| {
            error!(%latest_height, "Failed to get the consensus params at ConsensusReady: {e}");
            error!(%latest_height, "Using default consensus params as a fallback");
        })
        .unwrap_or_default();

    Ok((next_height, validator_set, consensus_params, previous_block))
}

enum ReplayBlockError {
    /// The payload for this height was not found in the repository during replay.
    PayloadMissing(Height),
    /// Any other error during replay.
    Other(eyre::Report),
}

impl From<eyre::Report> for ReplayBlockError {
    fn from(err: eyre::Report) -> Self {
        Self::Other(err)
    }
}

/// Replays a single previously-decided block during startup.
///
/// Fetches the stored execution payload for the given `height`,
/// validates it via the engine, and finalises it with a
/// fork-choice update. This brings the execution client back
/// in sync with the consensus layer when the EL is behind.
///
/// Returns the resulting [`ExecutionBlock`] so the caller can
/// chain consecutive replays.
///
/// An invalid payload at this stage is treated as a fatal error
/// because the block was already decided by consensus -- there
/// is no recovery path and no need to record an
/// [`InvalidPayload`][crate::invalid_payloads::InvalidPayload].
async fn replay_block(
    height: Height,
    payloads_repository: impl PayloadsRepository,
    payload_validator: impl PayloadValidator,
    block_finalizer: impl BlockFinalizer,
) -> Result<ExecutionBlock, ReplayBlockError> {
    info!("🔄 Replay: replaying block at height {height} from Consensus to Execution Client");

    let payload = payloads_repository
        .get(height)
        .await
        .wrap_err_with(|| format!("Replay: failed to fetch payload for height {height}"))?
        .ok_or(ReplayBlockError::PayloadMissing(height))?;

    let payload_hash = payload.payload_inner.payload_inner.block_hash;

    // EngineAPI: New payload
    let result = payload_validator
        .validate_payload(&payload)
        .await
        .wrap_err_with(|| {
            format!("Payload validation failed while replaying block at height={height}, payload_hash={}", payload_hash)
        })?;

    if let PayloadValidationResult::Invalid { reason } = result {
        return Err(eyre!(
            "Replay: Execution payload validation failed for block {payload_hash}: {reason}"
        )
        .into());
    }

    // EngineAPI: ForkchoiceUpdated
    let (new_latest_block, latest_valid_hash) = block_finalizer
        .finalize_decided_block(height, &payload)
        .await
        .wrap_err_with(|| {
            format!(
            "Failed to finalize block while replaying height={height}, payload_hash={payload_hash}"
        )
        })?;

    info!(
        "🔍 Replay: Updated canonical latest block; timestamp: {:?}, hash {:?}",
        new_latest_block.timestamp, latest_valid_hash
    );

    Ok(new_latest_block)
}

/// Trigger EL peer-to-peer sync via `forkchoice_updated`, then poll until complete.
/// If `timeout` is `Some`, the function will return an error after the specified duration.
/// If `timeout` is `None`, a default timeout of 1 hour is used.
async fn checkpoint_sync(
    target_block_hash: arc_consensus_types::BlockHash,
    target_block_height: Height,
    engine_api: impl EngineAPI,
    ethereum_api: impl EthereumAPI,
    timeout: Option<Duration>,
) -> eyre::Result<ExecutionBlock> {
    use alloy_rpc_types_engine::PayloadStatusEnum;

    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3600);
    let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);

    info!(block_hash = %target_block_hash, height = %target_block_height, timeout = ?timeout, "🔄 Checkpoint sync: targeting block hash");

    let fcu_result = engine_api
        .forkchoice_updated(target_block_hash, None)
        .await
        .wrap_err("Checkpoint sync: forkchoice_updated failed")?;

    match fcu_result.payload_status.status {
        PayloadStatusEnum::Syncing => {
            info!("🔄 Checkpoint sync: EL acknowledged, polling until complete");
        }
        PayloadStatusEnum::Valid => {
            info!(block_hash = %target_block_hash, height = %target_block_height, "🔄 Checkpoint sync: EL already has the target block");
            let height_str = format!("0x{:x}", target_block_height.as_u64());
            return ethereum_api
                .get_block_by_number(&height_str)
                .await?
                .ok_or_else(|| {
                    eyre!(
                        "Checkpoint sync: no block at height {target_block_height} after Valid FCU"
                    )
                });
        }
        status => {
            return Err(eyre!("Checkpoint sync: unexpected FCU status: {status}"));
        }
    }

    const POLL_INTERVAL: Duration = Duration::from_secs(2);
    let start = tokio::time::Instant::now();
    let height_str = format!("0x{:x}", target_block_height.as_u64());

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        if start.elapsed() > timeout {
            return Err(eyre!("Checkpoint sync: timed out after {timeout:?}"));
        }

        let block = ethereum_api.get_block_by_number(&height_str).await?;
        if let Some(b) = block {
            if b.block_hash == target_block_hash {
                info!(
                    height = b.block_number,
                    block_hash = %b.block_hash,
                    "🔄 Checkpoint sync: complete"
                );
                return Ok(b);
            }
            return Err(eyre!(
                "Checkpoint sync: block hash mismatch at height {target_block_height}: \
                 expected {target_block_hash}, got {}",
                b.block_hash
            ));
        }

        info!(target_height = %target_block_height, "🔄 Checkpoint sync: still syncing");
    }
}

#[derive(Debug)]
struct HandshakeResult {
    previous_block: ExecutionBlock,
    latest_height: Height,
    next_height: Height,
}

async fn handshake_and_replay(
    payload_validator: impl PayloadValidator,
    block_finalizer: impl BlockFinalizer,
    engine_api: impl EngineAPI,
    ethereum_api: impl EthereumAPI,
    certificates_repository: impl CertificatesRepository,
    payloads_repository: impl PayloadsRepository,
    metrics: &AppMetrics,
) -> eyre::Result<HandshakeResult> {
    // Node start-up: https://hackmd.io/@danielrachi/engine_api#Node-startup
    // Check compatibility with execution client
    {
        let _guard = metrics.start_engine_api_timer("check_capabilities");
        check_capabilities(&engine_api)
            .await
            .wrap_err("Call to check_capabilities failed in handshake_and_replay")?
    }

    // N.B. We only support the following cases:
    //
    // latest_height(EL) == latest_height(Consensus) --> Nothing to replay
    // latest_height(EL) < latest_height(Consensus)  --> Replay all the missing blocks
    //
    // The following case is an error condition (unrecoverable):
    // latest_height(Consensus) < latest_height(EL)

    // Get the latest block from the execution engine
    let mut latest_block = {
        let _guard = metrics.start_engine_api_timer("get_block_by_number");

        ethereum_api
            .get_block_by_number("latest")
            .await?
            .ok_or_else(|| {
                eyre::eyre!("Handshake: Could not get latest block from execution client")
            })?
    };

    debug!("👉 Handshake: EL's latest_block: {:?}", latest_block);

    let latest_height_el = Height::new(latest_block.block_number);
    let latest_height_cons = certificates_repository
        .max_height()
        .await
        .wrap_err("Handshake: failed to get latest consensus height")?
        .unwrap_or_default();

    if latest_height_el > latest_height_cons {
        return Err(eyre!(
            "Handshake: inconsistent state: EL latest height ({latest_height_el}) \
            is greater than CL latest committed height ({latest_height_cons}). \
            Something is wrong with CL DB",
        ));
    }

    info!(
        "🤝 Handshake: EL latest height: {}, CL latest committed height: {}",
        latest_height_el, latest_height_cons
    );

    // Replay missing blocks from CL payloads. If a payload is missing at any height,
    // fall back to checkpoint sync using the block hash from the CL's latest certificate.
    let mut replay_height = latest_height_el.increment();
    while replay_height <= latest_height_cons {
        match replay_block(
            replay_height,
            &payloads_repository,
            &payload_validator,
            &block_finalizer,
        )
        .await
        {
            Ok(block) => latest_block = block,
            Err(ReplayBlockError::PayloadMissing(h)) => {
                warn!(height = %h, latest_height_cons = %latest_height_cons, "🔄 Handshake: payload missing, triggering checkpoint sync");

                // This can happen when the EL does not shut down cleanly and does not flush a large
                // number of blocks to disk, "lower" than what the CL has pruned to. In that case, no
                // payload will be found to replay, since neither the CL's DB (due to pruning) or the
                // EL (due to the bad shutdown) will have it.
                //
                // In this case, we take the decided blockhash from the last stored certificate, and
                // trigger a checkpoint sync on the EL.
                let certificate = certificates_repository
                    .get(latest_height_cons)
                    .await
                    .wrap_err("Handshake: failed to get certificate")?
                    .ok_or_else(|| {
                        eyre!("Handshake: no certificate at height {latest_height_cons}")
                    })?;

                let target_hash = certificate.certificate.value_id.block_hash();
                latest_block = checkpoint_sync(
                    target_hash,
                    latest_height_cons,
                    &engine_api,
                    &ethereum_api,
                    None,
                )
                .await?;
                break;
            }
            Err(ReplayBlockError::Other(e)) => return Err(e),
        }

        replay_height = replay_height.increment();
    }

    let replayed_blocks = latest_height_cons
        .as_u64()
        .saturating_sub(latest_height_el.as_u64());
    metrics.set_handshake_replay_blocks(replayed_blocks);

    let latest_height_el = Height::new(latest_block.block_number);
    let next_height = latest_height_el.increment();

    info!("🤝 Handshake complete: Next height will be {next_height}");

    Ok(HandshakeResult {
        previous_block: latest_block,
        latest_height: latest_height_cons,
        next_height,
    })
}

/// Maximum number of pending proposals allowed
/// Defined to be equal to the size of the consensus input buffer,
/// which is itself sized to handle all in-flight sync responses.
fn max_pending_proposals(config: &ValueSyncConfig) -> usize {
    let limit = config.parallel_requests * config.batch_size;
    assert!(limit > 0, "max_pending_proposals must be greater than 0");
    limit
}

/// Enforce pending proposals limit on startup.
/// Cleans up any excess proposals from previous runs.
async fn enforce_pending_proposals_limit(
    pending_proposals_repository: impl PendingProposalsRepository,
    metrics: &AppMetrics,
    max_pending_proposals: usize,
    current_height: Height,
) -> eyre::Result<()> {
    pending_proposals_repository
        .enforce_limit(max_pending_proposals, current_height)
        .await
        .wrap_err("failed to enforce pending proposals limit on startup")?;

    // Update metrics
    let pending_count = pending_proposals_repository
        .count()
        .await
        .wrap_err("failed to get pending proposals count after enforcing limit")?;

    metrics.observe_pending_proposal_parts_count(pending_count);

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::finalize::MockBlockFinalizer;
    use crate::metrics::AppMetrics;
    use crate::payload::{MockPayloadValidator, PayloadValidationResult};
    use crate::store::repositories::mocks::{
        MockCertificatesRepository, MockPayloadsRepository, MockPendingProposalsRepository,
    };

    use alloy_primitives::{Address as AlloyAddress, Bloom, Bytes as AlloyBytes, U256};
    use alloy_rpc_types_engine::{ExecutionPayloadV1, ExecutionPayloadV2, ExecutionPayloadV3};
    use arc_consensus_types::{ValidatorSet, B256};
    use arc_eth_engine::capabilities::EngineCapabilities;
    use arc_eth_engine::mocks::{MockEngineAPI, MockEthereumAPI};
    use eyre::eyre;
    use mockall::predicate::*;

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

    fn test_payload(height: u64, timestamp: u64) -> ExecutionPayloadV3 {
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

    fn test_validator_set() -> ValidatorSet {
        ValidatorSet {
            validators: Arc::new(vec![]),
        }
    }

    fn test_consensus_params() -> ConsensusParams {
        ConsensusParams::default()
    }

    fn test_metrics() -> AppMetrics {
        AppMetrics::default()
    }

    fn setup_mock_engine_api_success() -> MockEngineAPI {
        let mut engine_api = MockEngineAPI::new();
        engine_api
            .expect_exchange_capabilities()
            .return_once(|| Ok(EngineCapabilities::all()));
        engine_api
    }

    fn setup_mock_ethereum_api_with_block(
        block: ExecutionBlock,
        validator_set_height: u64,
    ) -> MockEthereumAPI {
        let mut ethereum_api = MockEthereumAPI::new();
        ethereum_api
            .expect_get_block_by_number()
            .with(eq("latest"))
            .returning(move |_| Ok(Some(block)));
        ethereum_api
            .expect_get_active_validator_set()
            .with(eq(validator_set_height))
            .returning(|_| Ok(test_validator_set()));
        ethereum_api
            .expect_get_consensus_params()
            .with(eq(validator_set_height))
            .returning(|_| Ok(test_consensus_params()));
        ethereum_api
    }

    fn setup_mock_ethereum_api_no_block() -> MockEthereumAPI {
        let mut ethereum_api = MockEthereumAPI::new();
        ethereum_api
            .expect_get_block_by_number()
            .with(eq("latest"))
            .returning(|_| Ok(None));
        ethereum_api
    }

    // Test 1: Exact sync (EL == CL)
    #[tokio::test]
    async fn test_exact_sync_no_replay_needed() {
        let el_height = 5u64;
        let cl_height = Height::new(el_height);
        let latest_block = test_execution_block(el_height, 1000);
        let expected_replayed = 0u64;

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator.expect_validate_payload().never();

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer.expect_finalize_decided_block().never();

        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(latest_block, el_height);

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo.expect_get().never();

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let HandshakeResult {
            previous_block,
            latest_height,
            next_height,
        } = result.unwrap();

        assert_eq!(previous_block.block_number, el_height);
        assert_eq!(latest_height, cl_height);
        assert_eq!(next_height, cl_height.increment());
        assert_eq!(metrics.get_handshake_replay_blocks(), expected_replayed);
    }

    // Test 2: Single block replay (EL + 1 == CL)
    #[tokio::test]
    async fn test_single_block_replay() {
        let el_height = 4u64;
        let cl_height = Height::new(el_height + 1);
        let latest_block_el = test_execution_block(el_height, 1000);
        let replayed_block = test_execution_block(cl_height.as_u64(), 1100);
        let payload_to_replay = test_payload(cl_height.as_u64(), 1100);
        let expected_replayed = 1u64;

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator
            .expect_validate_payload()
            .return_once(move |p| {
                assert_eq!(
                    p.payload_inner.payload_inner.block_number,
                    cl_height.as_u64()
                );

                Ok(PayloadValidationResult::Valid)
            });

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer
            .expect_finalize_decided_block()
            .return_once(move |h, p| {
                assert_eq!(h, cl_height);
                assert_eq!(
                    p.payload_inner.payload_inner.block_number,
                    cl_height.as_u64()
                );

                Ok((replayed_block, replayed_block.block_hash))
            });

        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(latest_block_el, cl_height.as_u64());

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo.expect_get().return_once(move |h| {
            assert_eq!(h, cl_height);
            Ok(Some(payload_to_replay))
        });

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let HandshakeResult {
            previous_block,
            latest_height,
            next_height,
        } = result.unwrap();

        assert_eq!(previous_block.block_number, cl_height.as_u64());
        assert_eq!(latest_height, cl_height);
        assert_eq!(next_height, cl_height.increment());
        assert_eq!(metrics.get_handshake_replay_blocks(), expected_replayed);
    }

    async fn do_multiple_block_replay(num_blocks: usize) {
        let el_height = 3u64;
        let cl_height = Height::new(el_height + num_blocks as u64);
        let latest_block_el = test_execution_block(el_height, 1000);
        let expected_replayed = num_blocks as u64;

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator
            .expect_validate_payload()
            .times(num_blocks)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer
            .expect_finalize_decided_block()
            .times(num_blocks)
            .returning(|height, _| {
                let h = height.as_u64();
                let block = test_execution_block(h, 1000 + h);
                Ok((block, block.block_hash))
            });

        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(latest_block_el, cl_height.as_u64());

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo
            .expect_get()
            .times(num_blocks)
            .returning(|height| Ok(Some(test_payload(height.as_u64(), 1000 + height.as_u64()))));

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let HandshakeResult {
            previous_block,
            latest_height,
            next_height,
        } = result.unwrap();

        assert_eq!(previous_block.block_number, cl_height.as_u64());
        assert_eq!(latest_height, cl_height);
        assert_eq!(next_height, cl_height.increment());
        assert_eq!(metrics.get_handshake_replay_blocks(), expected_replayed);
    }

    // Test 3: Multiple block replay (EL + i == CL)
    #[tokio::test]
    async fn test_multiple_block_replay() {
        for i in 1..=10 {
            println!("multiple_block_replay: Running test with {i} blocks");
            do_multiple_block_replay(i).await;
        }
    }

    // Test 4: Fresh start (both at genesis)
    #[tokio::test]
    async fn test_fresh_start_at_genesis() {
        let el_height = 0u64;
        let cl_height = Height::new(0);
        let genesis_block = test_execution_block(el_height, 0);

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator.expect_validate_payload().never();

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer.expect_finalize_decided_block().never();

        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(genesis_block, el_height);

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo.expect_get().never();

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let HandshakeResult {
            previous_block,
            latest_height,
            next_height,
        } = result.unwrap();

        assert_eq!(previous_block.block_number, 0);
        assert_eq!(latest_height, Height::new(0));
        assert_eq!(next_height, Height::new(1));
    }

    // Test 5: EL ahead of CL
    #[tokio::test]
    async fn test_el_ahead_of_cl_error() {
        let el_height = 10u64;
        let cl_height = Height::new(5);
        let latest_block_el = test_execution_block(el_height, 1000);

        let payload_validator = MockPayloadValidator::new();
        let block_finalizer = MockBlockFinalizer::new();
        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(latest_block_el, cl_height.as_u64());

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        let payloads_repo = MockPayloadsRepository::new();
        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("inconsistent state"));
        assert!(err_msg.contains("EL latest height"));
        assert!(err_msg.contains("CL latest committed height"));
    }

    // Test 6: Missing latest block from EL
    #[tokio::test]
    async fn test_missing_latest_block_from_el() {
        let payload_validator = MockPayloadValidator::new();
        let block_finalizer = MockBlockFinalizer::new();
        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_no_block();

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo.expect_max_height().never();

        let payloads_repo = MockPayloadsRepository::new();
        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Could not get latest block from execution client"));
    }

    // Test 7: Missing payload during replay triggers checkpoint sync, which fails
    // if the CL's latest certificate is also missing (corrupted CL DB)
    #[tokio::test]
    async fn test_missing_payload_and_certificate_during_replay() {
        let el_height = 4u64;
        let cl_height = Height::new(5);
        let latest_block_el = test_execution_block(el_height, 1000);

        let payload_validator = MockPayloadValidator::new();
        let block_finalizer = MockBlockFinalizer::new();
        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(latest_block_el, cl_height.as_u64());

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        // No payload found for replay height
        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo.expect_get().times(1).returning(|_| Ok(None));

        // Will fallback to checkpoint sync, but no certificate found either
        certificates_repo.expect_get().return_once(move |height| {
            assert_eq!(height, cl_height);
            Ok(None)
        });

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("no certificate at height"));
    }

    // Test 8: Invalid payload during replay
    #[tokio::test]
    async fn test_invalid_payload_during_replay() {
        let el_height = 4u64;
        let cl_height = Height::new(5);
        let latest_block_el = test_execution_block(el_height, 1000);
        let payload_to_replay = test_payload(5, 1100);

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator
            .expect_validate_payload()
            .return_once(|_| {
                Ok(PayloadValidationResult::Invalid {
                    reason: "test".into(),
                })
            });

        let block_finalizer = MockBlockFinalizer::new();
        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(latest_block_el, cl_height.as_u64());

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo
            .expect_get()
            .return_once(move |_| Ok(Some(payload_to_replay.clone())));

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Execution payload validation failed"));
    }

    // Test 9: Finalization failure during replay
    #[tokio::test]
    async fn test_finalization_failure_during_replay() {
        let el_height = 4u64;
        let cl_height = Height::new(5);
        let latest_block_el = test_execution_block(el_height, 1000);
        let payload_to_replay = test_payload(5, 1100);

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator
            .expect_validate_payload()
            .return_once(|_| Ok(PayloadValidationResult::Valid));

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer
            .expect_finalize_decided_block()
            .return_once(|_, _| Err(eyre!("Finalization failed")));

        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(latest_block_el, cl_height.as_u64());

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo
            .expect_get()
            .return_once(move |_| Ok(Some(payload_to_replay)));

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Failed to finalize block"));
    }

    // Test 10: Capability check failure
    #[tokio::test]
    async fn test_capability_check_failure() {
        let payload_validator = MockPayloadValidator::new();
        let block_finalizer = MockBlockFinalizer::new();
        let mut engine_api = MockEngineAPI::new();
        engine_api
            .expect_exchange_capabilities()
            .return_once(|| Err(eyre!("Capability check failed")));

        let ethereum_api = setup_mock_ethereum_api_with_block(test_execution_block(5, 1000), 5);

        let certificates_repo = MockCertificatesRepository::new();
        let payloads_repo = MockPayloadsRepository::new();
        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("check_capabilities"));
    }

    // Test 11: Empty consensus DB
    #[tokio::test]
    async fn test_empty_consensus_db() {
        let el_height = 0u64;
        let genesis_block = test_execution_block(el_height, 0);

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator.expect_validate_payload().never();

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer.expect_finalize_decided_block().never();

        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(genesis_block, el_height);

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(|| Ok(None)); // Empty DB returns None

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo.expect_get().never();

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let HandshakeResult {
            previous_block,
            latest_height,
            next_height,
        } = result.unwrap();

        assert_eq!(previous_block.block_number, 0);
        assert_eq!(latest_height, Height::new(0)); // Defaults to 0
        assert_eq!(next_height, Height::new(1));
    }

    // Test 12: Large gap replay
    #[tokio::test]
    async fn test_large_gap_replay() {
        let el_height = 0u64;
        let cl_height = Height::new(20);
        let latest_block_el = test_execution_block(el_height, 0);

        let mut payload_validator = MockPayloadValidator::new();
        payload_validator
            .expect_validate_payload()
            .times(20) // Heights 1 to 20
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer
            .expect_finalize_decided_block()
            .times(20)
            .returning(|height, _| {
                let h = height.as_u64();
                let block = test_execution_block(h, h * 100);
                Ok((block, block.block_hash))
            });

        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(latest_block_el, cl_height.as_u64());

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo
            .expect_get()
            .times(20)
            .returning(|height| Ok(Some(test_payload(height.as_u64(), height.as_u64() * 100))));

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let HandshakeResult {
            previous_block,
            latest_height,
            next_height,
        } = result.unwrap();

        assert_eq!(previous_block.block_number, 20);
        assert_eq!(latest_height, cl_height);
        assert_eq!(next_height, Height::new(21));
    }

    // Test 17: Repository errors propagate correctly
    #[tokio::test]
    async fn test_certificates_repo_error_propagates() {
        let payload_validator = MockPayloadValidator::new();
        let block_finalizer = MockBlockFinalizer::new();
        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(test_execution_block(5, 1000), 5);

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(|| Err(std::io::Error::other("Decided blocks fetch error")));

        let payloads_repo = MockPayloadsRepository::new();
        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("failed to get latest consensus height"));
    }

    #[tokio::test]
    async fn test_payloads_repo_error_propagates() {
        let el_height = 4u64;
        let cl_height = Height::new(5);
        let latest_block_el = test_execution_block(el_height, 1000);

        let payload_validator = MockPayloadValidator::new();
        let block_finalizer = MockBlockFinalizer::new();
        let engine_api = setup_mock_engine_api_success();
        let ethereum_api = setup_mock_ethereum_api_with_block(latest_block_el, cl_height.as_u64());

        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo
            .expect_get()
            .return_once(|_| Err(std::io::Error::other("Payload fetch error")));

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Replay: failed to fetch payload"),
            "Expected payload fetch error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_on_consensus_ready_fallback_to_default_params() {
        let metrics = test_metrics();

        let mut pending_proposals_repo = MockPendingProposalsRepository::new();
        pending_proposals_repo
            .expect_enforce_limit()
            .returning(|_, _| Ok(Vec::new()));
        pending_proposals_repo.expect_count().returning(|| Ok(0));

        let payloads_repo = MockPayloadsRepository::new();
        let mut certificates_repo = MockCertificatesRepository::new();
        let payload_validator = MockPayloadValidator::new();
        let block_finalizer = MockBlockFinalizer::new();
        let engine_api = setup_mock_engine_api_success();

        let latest_height = Height::new(5);
        let latest_block = test_execution_block(latest_height.as_u64(), 1000);

        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(latest_height)));

        let mut ethereum_api = MockEthereumAPI::new();
        ethereum_api
            .expect_get_block_by_number()
            .with(eq("latest"))
            .returning(move |_| Ok(Some(latest_block)));
        ethereum_api
            .expect_get_active_validator_set()
            .with(eq(latest_height.as_u64()))
            .returning(|_| Ok(test_validator_set()));
        ethereum_api
            .expect_get_consensus_params()
            .with(eq(latest_height.as_u64()))
            .returning(|_| Err(eyre!("mock get_consensus_params failed")));

        let (next_height, _, consensus_params, _) = on_consensus_ready(
            &metrics,
            pending_proposals_repo,
            payloads_repo,
            certificates_repo,
            payload_validator,
            block_finalizer,
            &engine_api,
            &ethereum_api,
            10,
        )
        .await
        .unwrap();

        assert_eq!(next_height, latest_height.increment());
        assert_eq!(consensus_params, ConsensusParams::default());
    }

    // Test: Partial replay then checkpoint sync
    // Replays heights 5..7 successfully, payload missing at height 8,
    // triggers checkpoint sync from that point.
    // This also covers the case where height 5 (or initial height) does not
    // have a payload in the repository.
    #[tokio::test]
    async fn test_partial_replay_then_checkpoint_sync() {
        use alloy_rpc_types_engine::{ForkchoiceUpdated, PayloadStatus, PayloadStatusEnum};
        use arc_consensus_types::{CommitCertificateType, Round, StoredCommitCertificate, ValueId};
        use malachitebft_core_types::CommitCertificate;

        // Replay covers heights [5, 10]
        let el_height = 4u64;
        let cl_height = Height::new(10);

        // Check point block hash
        let check_point_block_hash = B256::repeat_byte(0xAA);
        let check_point_block = ExecutionBlock {
            block_hash: check_point_block_hash,
            block_number: cl_height.as_u64(),
            parent_hash: B256::repeat_byte(0x09),
            timestamp: 2000,
        };
        let latest_block_el = test_execution_block(el_height, 1000);

        // Heights 5, 6, 7 replay successfully; height 8 is missing
        let mut payload_validator = MockPayloadValidator::new();
        payload_validator
            .expect_validate_payload()
            .times(3)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut block_finalizer = MockBlockFinalizer::new();
        block_finalizer
            .expect_finalize_decided_block()
            .times(3)
            .returning(|height, _| {
                assert!(height >= Height::new(5) && height <= Height::new(7));
                let h = height.as_u64();
                let block = test_execution_block(h, 1000 + h);
                Ok((block, block.block_hash))
            });

        let mut engine_api = setup_mock_engine_api_success();
        engine_api
            .expect_forkchoice_updated()
            .return_once(move |_, _| {
                Ok(ForkchoiceUpdated::new(PayloadStatus::new(
                    PayloadStatusEnum::Syncing,
                    None,
                )))
            });

        let mut ethereum_api = MockEthereumAPI::new();
        // Handshake fetches "latest" for the initial EL height
        ethereum_api
            .expect_get_block_by_number()
            .with(eq("latest"))
            .returning(move |_| Ok(Some(latest_block_el)));
        // Checkpoint sync polls by target height
        ethereum_api
            .expect_get_block_by_number()
            .with(eq("0xa"))
            .returning(move |_| Ok(Some(check_point_block)));
        ethereum_api
            .expect_get_active_validator_set()
            .with(eq(cl_height.as_u64()))
            .returning(|_| Ok(test_validator_set()));
        ethereum_api
            .expect_get_consensus_params()
            .with(eq(cl_height.as_u64()))
            .returning(|_| Ok(test_consensus_params()));

        // Simulate having a cert at the latest height
        let mut certificates_repo = MockCertificatesRepository::new();
        certificates_repo
            .expect_max_height()
            .return_once(move || Ok(Some(cl_height)));
        certificates_repo.expect_get().return_once(move |_| {
            Ok(Some(StoredCommitCertificate {
                certificate: CommitCertificate::new(
                    cl_height,
                    Round::new(0),
                    ValueId::new(check_point_block_hash),
                    vec![],
                ),
                certificate_type: CommitCertificateType::Minimal,
                proposer: None,
            }))
        });

        let mut payloads_repo = MockPayloadsRepository::new();
        payloads_repo
            .expect_get()
            .times(4) // 3 successful + 1 missing at height 8
            .returning(|height| {
                let h = height.as_u64();
                if h <= 7 {
                    Ok(Some(test_payload(h, 1000 + h)))
                } else {
                    Ok(None)
                }
            });

        let metrics = test_metrics();

        let result = handshake_and_replay(
            &payload_validator,
            &block_finalizer,
            &engine_api,
            &ethereum_api,
            &certificates_repo,
            &payloads_repo,
            &metrics,
        )
        .await;

        let HandshakeResult {
            previous_block,
            latest_height,
            next_height,
        } = result.unwrap();

        assert_eq!(previous_block.block_hash, check_point_block_hash);
        assert_eq!(previous_block.block_number, cl_height.as_u64());
        assert_eq!(latest_height, cl_height);
        assert_eq!(next_height, cl_height.increment());
    }

    // Test: checkpoint_sync returns once target block matches the target hash
    #[tokio::test]
    async fn test_checkpoint_sync_polls_until_match() {
        use alloy_rpc_types_engine::{ForkchoiceUpdated, PayloadStatus, PayloadStatusEnum};

        let target_height = Height::new(50);
        let check_point_block_hash = B256::repeat_byte(0xBB);
        let check_point_block = ExecutionBlock {
            block_hash: check_point_block_hash,
            block_number: 50,
            parent_hash: B256::repeat_byte(0x31),
            timestamp: 5000,
        };

        let mut engine_api = MockEngineAPI::new();
        engine_api
            .expect_forkchoice_updated()
            .return_once(move |_, _| {
                Ok(ForkchoiceUpdated::new(PayloadStatus::new(
                    PayloadStatusEnum::Syncing,
                    None,
                )))
            });

        let mut ethereum_api = MockEthereumAPI::new();
        let mut poll_count = 0u32;
        ethereum_api
            .expect_get_block_by_number()
            .with(eq("0x32"))
            .returning(move |_| {
                poll_count += 1;
                if poll_count < 3 {
                    // First two polls: block not yet available at target height
                    Ok(None)
                } else {
                    // Third poll: matches target
                    Ok(Some(check_point_block))
                }
            });

        let result = checkpoint_sync(
            check_point_block_hash,
            target_height,
            &engine_api,
            &ethereum_api,
            None,
        )
        .await;

        let block = result.unwrap();
        assert_eq!(block.block_hash, check_point_block_hash);
        assert_eq!(block.block_number, 50);
    }

    // Test: checkpoint_sync times out if the EL never reaches the target block
    #[tokio::test(start_paused = true)]
    async fn test_checkpoint_sync_times_out() {
        use alloy_rpc_types_engine::{ForkchoiceUpdated, PayloadStatus, PayloadStatusEnum};

        let target_hash = B256::repeat_byte(0xCC);
        let mut engine_api = MockEngineAPI::new();
        engine_api.expect_forkchoice_updated().return_once(|_, _| {
            Ok(ForkchoiceUpdated::new(PayloadStatus::new(
                PayloadStatusEnum::Syncing,
                None,
            )))
        });

        let mut ethereum_api = MockEthereumAPI::new();
        // Block at target height is never available, forcing a timeout
        ethereum_api
            .expect_get_block_by_number()
            .with(eq("0x32"))
            .returning(|_| Ok(None));

        let timeout = Some(Duration::from_secs(10));
        let result = checkpoint_sync(
            target_hash,
            Height::new(50),
            &engine_api,
            &ethereum_api,
            timeout,
        )
        .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("timed out"));
    }

    // Test: checkpoint_sync fails on unexpected FCU status
    #[tokio::test]
    async fn test_checkpoint_sync_unexpected_fcu_status() {
        use alloy_rpc_types_engine::{ForkchoiceUpdated, PayloadStatus, PayloadStatusEnum};

        let target_hash = B256::repeat_byte(0xDD);
        let mut engine_api = MockEngineAPI::new();
        engine_api.expect_forkchoice_updated().return_once(|_, _| {
            Ok(ForkchoiceUpdated::new(PayloadStatus::new(
                PayloadStatusEnum::Invalid {
                    validation_error: "block not found".to_string(),
                },
                None,
            )))
        });

        let ethereum_api = MockEthereumAPI::new();

        let result = checkpoint_sync(
            target_hash,
            Height::new(100),
            &engine_api,
            &ethereum_api,
            None,
        )
        .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("unexpected FCU status"));
    }

    // Test: checkpoint_sync fails when block at target height has a different hash
    #[tokio::test]
    async fn test_checkpoint_sync_block_hash_mismatch() {
        use alloy_rpc_types_engine::{ForkchoiceUpdated, PayloadStatus, PayloadStatusEnum};

        let target_hash = B256::repeat_byte(0xAA);
        let wrong_hash = B256::repeat_byte(0xFF);
        let target_height = Height::new(50);

        let mut engine_api = MockEngineAPI::new();
        engine_api.expect_forkchoice_updated().return_once(|_, _| {
            Ok(ForkchoiceUpdated::new(PayloadStatus::new(
                PayloadStatusEnum::Syncing,
                None,
            )))
        });

        let mut ethereum_api = MockEthereumAPI::new();
        let height_str = format!("0x{:x}", target_height.as_u64());
        ethereum_api
            .expect_get_block_by_number()
            .with(eq(height_str))
            .returning(move |_| {
                Ok(Some(ExecutionBlock {
                    block_hash: wrong_hash,
                    block_number: target_height.as_u64(),
                    parent_hash: B256::ZERO,
                    timestamp: 1000,
                }))
            });

        let result =
            checkpoint_sync(target_hash, target_height, &engine_api, &ethereum_api, None).await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("block hash mismatch"));
    }

    // Test: checkpoint_sync uses hex quantity when EL already has the target block.
    #[tokio::test]
    async fn test_checkpoint_sync_valid_status_uses_hex_block_number() {
        use alloy_rpc_types_engine::{ForkchoiceUpdated, PayloadStatus, PayloadStatusEnum};

        let target_height = Height::new(50);
        let target_hash = B256::repeat_byte(0xAB);
        let target_block = ExecutionBlock {
            block_hash: target_hash,
            block_number: target_height.as_u64(),
            parent_hash: B256::repeat_byte(0x31),
            timestamp: 5000,
        };

        let mut engine_api = MockEngineAPI::new();
        engine_api
            .expect_forkchoice_updated()
            .return_once(move |_, _| {
                Ok(ForkchoiceUpdated::new(PayloadStatus::new(
                    PayloadStatusEnum::Valid,
                    None,
                )))
            });

        let mut ethereum_api = MockEthereumAPI::new();
        ethereum_api
            .expect_get_block_by_number()
            .with(eq("0x32"))
            .return_once(move |_| Ok(Some(target_block)));

        let result =
            checkpoint_sync(target_hash, target_height, &engine_api, &ethereum_api, None).await;

        let block = result.unwrap();
        assert_eq!(block.block_hash, target_hash);
        assert_eq!(block.block_number, target_height.as_u64());
    }
}
