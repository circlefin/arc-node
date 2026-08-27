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

use backon::{BackoffBuilder, ConstantBuilder, Retryable};
use tracing::{error, warn};

use malachitebft_app_channel::app::types::core::Validity;

use alloy_rpc_types_engine::{ExecutionPayloadV3, PayloadStatusEnum};

use arc_consensus_types::{Address, BlockHash, Height, Round};
use arc_eth_engine::deadline::EngineDeadline;
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_eth_engine::rpc::EngineApiRpcError;

use crate::block::ConsensusBlock;
use crate::metrics::app::{AppMetrics, InvalidPayloadSource};
use crate::store::repositories::InvalidPayloadsRepository;
use arc_consensus_db::invalid_payloads::InvalidPayload;

pub async fn generate_payload_with_retry(
    previous_block: &ExecutionBlock,
    fee_recipient: &Address,
    generator: &impl PayloadGenerator,
    metrics: &AppMetrics,
) -> eyre::Result<ExecutionPayloadV3> {
    const MAX_RETRIES: usize = 5;
    const RETRY_POLICY: ConstantBuilder = ConstantBuilder::new()
        .with_delay(Duration::from_millis(100))
        .with_max_times(MAX_RETRIES);

    let call_once = || async {
        // Ensure timestamp is non-decreasing by setting it to max(previous_block.timestamp, now())
        // This allows us to continue making progress, proposing blocks that have
        // the same block timestamp as the "jumped" block until enough time has elapsed such
        // that we can continue making progress with advancing timestamps.
        let now = Engine::timestamp_now();
        let timestamp = std::cmp::max(previous_block.timestamp, now);

        if previous_block.timestamp > now {
            // timestamp >= now (since max chose previous_block.timestamp > now)
            let skew = timestamp.saturating_sub(now);
            warn!(
                timestamp = timestamp,
                skew = skew,
                "Clock skew detected: using parent timestamp",
            );
        }

        let _guard = metrics.start_engine_api_timer("generate_block");

        generator
            .generate_block(previous_block, timestamp, fee_recipient)
            .await
    };

    let mut attempt_num = 0usize;

    call_once
        .retry(RETRY_POLICY.build())
        .sleep(tokio::time::sleep) // give reth time to breathe
        .notify(|_e, dur| {
            // Bounded by MAX_RETRIES (5)
            #[allow(clippy::arithmetic_side_effects)]
            {
                attempt_num += 1;
            }
            let attempts_left = MAX_RETRIES.saturating_sub(attempt_num);
            error!(
                attempt = attempt_num,
                attempts_left,
                delay_ms = dur.as_millis(),
                "reth forgot its payload id; retrying (forking off the same previous block)"
            );
        })
        .when(|e| {
            EngineApiRpcError::try_from(e)
                .map(|err| err.is_unknown_payload())
                .unwrap_or(false)
        })
        .await
}

/// Introduced to improve testability of `generate_payload_with_retry`
#[cfg_attr(test, mockall::automock)]
pub trait PayloadGenerator: Send + Sync {
    async fn generate_block(
        &self,
        parent: &ExecutionBlock,
        timestamp: u64,
        fee_recipient: &Address,
    ) -> eyre::Result<ExecutionPayloadV3>;
}

pub struct EnginePayloadGenerator<'a> {
    pub engine: &'a Engine,
    /// Consensus budget for the proposer's build sequence; extends the
    /// engine's per-call timeout floors when set.
    pub deadline: Option<EngineDeadline>,
}

impl<'a> PayloadGenerator for EnginePayloadGenerator<'a> {
    async fn generate_block(
        &self,
        parent: &ExecutionBlock,
        timestamp: u64,
        fee_recipient: &Address,
    ) -> eyre::Result<ExecutionPayloadV3> {
        self.engine
            .generate_block(parent, timestamp, fee_recipient, self.deadline)
            .await
    }
}

/// Abstraction over execution payload validation.
///
/// This trait exists so that handler code can validate payloads
/// without depending on the concrete [`Engine`] type, making it
/// possible to substitute a mock in unit tests.
#[cfg_attr(test, mockall::automock)]
pub trait PayloadValidator {
    /// Validates an execution payload via the engine.
    async fn validate_payload(
        &self,
        payload: &ExecutionPayloadV3,
    ) -> eyre::Result<PayloadValidationResult>;
}

impl<T> PayloadValidator for &T
where
    T: PayloadValidator + ?Sized,
{
    async fn validate_payload(
        &self,
        payload: &ExecutionPayloadV3,
    ) -> eyre::Result<PayloadValidationResult> {
        (*self).validate_payload(payload).await
    }
}

/// [`PayloadValidator`] backed by a real [`Engine`] instance.
///
/// Delegates to the module-private [`validate_payload`] function,
/// which sends the payload to the execution client via
/// `engine.newPayload` and interprets the response.
pub struct EnginePayloadValidator<'a> {
    engine: &'a Engine,
    metrics: &'a AppMetrics,
    deadline: Option<EngineDeadline>,
}

impl<'a> EnginePayloadValidator<'a> {
    pub fn new(engine: &'a Engine, metrics: &'a AppMetrics) -> Self {
        Self {
            engine,
            metrics,
            deadline: None,
        }
    }

    /// Validator for the proposer's self-validation path: the consensus
    /// budget extends the engine's per-call timeout floor.
    pub fn new_with_deadline(
        engine: &'a Engine,
        metrics: &'a AppMetrics,
        deadline: EngineDeadline,
    ) -> Self {
        Self {
            engine,
            metrics,
            deadline: Some(deadline),
        }
    }
}

impl PayloadValidator for EnginePayloadValidator<'_> {
    async fn validate_payload(
        &self,
        payload: &ExecutionPayloadV3,
    ) -> eyre::Result<PayloadValidationResult> {
        validate_payload(self.engine, payload, self.metrics, self.deadline).await
    }
}

/// Result of validating an execution payload via the engine.
///
/// Carries the engine's verdict so that callers can act on it (e.g. store the
/// rejection reason) without losing the detail across the call boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadValidationResult {
    /// The engine accepted the payload.
    Valid,
    /// The engine rejected the payload for the given reason.
    Invalid { reason: String },
}

impl std::fmt::Display for PayloadValidationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Valid => write!(f, "Valid"),
            Self::Invalid { reason } => {
                write!(f, "Invalid: {reason}")
            }
        }
    }
}

/// Validates an execution payload by sending it to the engine via `newPayload`.
///
/// # Return values
///
/// - `Ok(Valid)`: the engine accepted the payload.
/// - `Ok(Invalid { reason })`: the engine rejected the payload, either via its
///   status response (`INVALID`) or via a non-internal JSON-RPC error.
/// - `Err(..)`: no verdict was obtained — the engine replied with an unexpected
///   status (`SYNCING` or `ACCEPTED`, logged as a warning), returned a JSON-RPC
///   internal error, or an unrelated internal error occurred in the call stack.
async fn validate_payload(
    engine: &Engine,
    execution_payload: &ExecutionPayloadV3,
    metrics: &AppMetrics,
    deadline: Option<EngineDeadline>,
) -> eyre::Result<PayloadValidationResult> {
    let block_hash = execution_payload.payload_inner.payload_inner.block_hash;

    // EIP-4844 blobs are not supported and not needed for our use case.
    //
    // Rationale:
    // - Blobs are not required for private or public testnet deployments.
    //   Integration teams don't use them; blobs are typically used by L2s.
    // - Proper blob support requires propagating the actual blob data via
    //   consensus layer gossip mechanisms, which our current malachite-app
    //   implementation does not handle.
    // - Managing blob hashes alone without blob propagation is insufficient
    //   and would be an incomplete implementation.
    // - If blob support becomes necessary in the future, it will require
    //   a complete design including blob propagation mechanisms.
    let versioned_hashes = Vec::new();
    let _guard = metrics.start_engine_api_timer("notify_new_block");

    match engine
        .notify_new_block(execution_payload, versioned_hashes, deadline)
        .await
    {
        Ok(status) => match status.status {
            PayloadStatusEnum::Valid => Ok(PayloadValidationResult::Valid),
            PayloadStatusEnum::Invalid { validation_error } => {
                Ok(PayloadValidationResult::Invalid {
                    reason: validation_error,
                })
            }
            // The remaining cases are SYNCING and ACCEPTED:
            // - SYNCING: we don't expect this in ARC because the CL and EL are kept
            //   in sync. As a result, the EL should always have the information it
            //   needs to validate a payload.
            // - ACCEPTED: we don't expect to have side chains in ARC, so this status
            //   should never be returned.
            _ => {
                let height = execution_payload.payload_inner.payload_inner.block_number;
                warn!(
                    %block_hash,
                    %height,
                    "Unexpected payload status: {status:?}",
                );
                Err(eyre::eyre!(
                    "unexpected {status:?} status from engine for block {block_hash} at height {height}"
                ))
            }
        },
        Err(e) => {
            if let Ok(engine_api_error) = EngineApiRpcError::try_from(&e) {
                if !engine_api_error.is_internal_error() {
                    error!(
                        %block_hash,
                        "Invalid payload: {engine_api_error}",
                    );
                    return Ok(PayloadValidationResult::Invalid {
                        reason: engine_api_error.to_string(),
                    });
                }
            }

            // Internal failures provide no deterministic payload verdict.
            let msg = format!(
                "call to EngineAPI::new_payload failed when validating block: {block_hash}",
            );
            Err(e.wrap_err(msg))
        }
    }
}

/// Validates a consensus block's payload and stores it in the database
/// if the engine rejects it.
///
/// This is the higher-level entry point for callers that have a
/// [`ConsensusBlock`] and an [`InvalidPayloadsRepository`]. It delegates
/// to [`PayloadValidator::validate_payload`] for the actual engine call
/// and then persists an [`InvalidPayload`] record when the verdict is
/// `Invalid`.
///
/// # Return contract
///
/// - `Ok(Validity::Valid)`: the engine accepted the payload.
/// - `Ok(Validity::Invalid)`: the engine rejected the payload. Persisting
///   the forensic [`InvalidPayload`] record is **best-effort**: a failure
///   to append is logged at `error` but does not change the verdict
///   returned to the caller. The engine's verdict is authoritative and
///   must reach the consensus layer so the corresponding undecided block
///   is marked `Invalid` rather than left with a placeholder `Valid`.
/// - `Err(_)`: no verdict was obtained (engine transport error,
///   `SYNCING`/`ACCEPTED` status, etc.).
pub async fn validate_consensus_block(
    payload_validator: &impl PayloadValidator,
    block: &ConsensusBlock,
    store: &impl InvalidPayloadsRepository,
    metrics: &AppMetrics,
) -> eyre::Result<Validity> {
    let result = payload_validator
        .validate_payload(&block.execution_payload)
        .await?;

    match result {
        PayloadValidationResult::Valid => Ok(Validity::Valid),
        PayloadValidationResult::Invalid { reason } => {
            warn!(
                height = %block.height,
                round = %block.round,
                block_hash = %block.self_reported_block_hash(),
                proposer = %block.proposer,
                reason = %reason,
                "Engine rejected payload, storing for forensics",
            );
            metrics.inc_invalid_payloads_count(InvalidPayloadSource::EngineReject);
            let invalid = InvalidPayload::new_from_block(block, &reason);
            if let Err(e) = store.append(invalid).await {
                error!(
                    height = %block.height,
                    round = %block.round,
                    block_hash = %block.self_reported_block_hash(),
                    proposer = %block.proposer,
                    "Failed to persist invalid-payload forensic record: {e}",
                );
            }
            Ok(Validity::Invalid)
        }
    }
}

/// An execution payload that does not belong at its place in the chain.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PayloadBindingError {
    #[error("payload block number {actual} does not match consensus height {expected}")]
    HeightMismatch { expected: u64, actual: u64 },

    #[error(
        "payload parent hash {actual} is not the block finalized at the previous height ({expected})"
    )]
    ParentMismatch {
        expected: BlockHash,
        actual: BlockHash,
    },
}

impl PayloadBindingError {
    /// The counter label for this rule. A wrong block number is a property of the
    /// payload alone. A wrong parent compares it against local state, so that label
    /// also rises when this node holds the wrong view of the previous height.
    pub fn invalid_payload_source(&self) -> InvalidPayloadSource {
        match self {
            Self::HeightMismatch { .. } => InvalidPayloadSource::PayloadHeight,
            Self::ParentMismatch { .. } => InvalidPayloadSource::PayloadParent,
        }
    }
}

/// Makes sure that an execution payload belongs at the given consensus height.
///
/// Arc keeps one execution block per consensus height. A payload therefore
/// carries that height as its block number, and it extends the block that the
/// node finalized at the height before.
///
/// The parent rule needs the immediate predecessor. During batch value sync,
/// `previous_block` can lag by several heights. This function therefore applies
/// the parent rule only when `previous_block` sits one height below `height`.
pub fn check_payload_binding(
    payload: &ExecutionPayloadV3,
    height: Height,
    previous_block: Option<&ExecutionBlock>,
) -> Result<(), PayloadBindingError> {
    let payload = &payload.payload_inner.payload_inner;
    let height = height.as_u64();

    if payload.block_number != height {
        return Err(PayloadBindingError::HeightMismatch {
            expected: height,
            actual: payload.block_number,
        });
    }

    let Some(previous) = previous_block else {
        return Ok(());
    };

    if previous.block_number.checked_add(1) != Some(height) {
        return Ok(());
    }

    if payload.parent_hash != previous.block_hash {
        return Err(PayloadBindingError::ParentMismatch {
            expected: previous.block_hash,
            actual: payload.parent_hash,
        });
    }

    Ok(())
}

/// A validity verdict together with the rule that produced it.
///
/// A caller that reconciles a fresh verdict with a stored one needs to know
/// which rule spoke. Only an engine verdict can differ between two runs against
/// the same parent state, so only an engine verdict describes replay divergence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockVerdict {
    /// The binding rules rejected the payload, and this is the rule that did. The
    /// engine was not asked. The rule reaches the caller because the two do not
    /// read alike: one blames the payload, the other can blame this node.
    Unbound(PayloadBindingError),
    /// The engine returned this verdict.
    Engine(Validity),
}

impl BlockVerdict {
    pub fn validity(&self) -> Validity {
        match self {
            Self::Unbound(_) => Validity::Invalid,
            Self::Engine(validity) => *validity,
        }
    }
}

/// Establishes the validity of a block that arrived from the network.
///
/// The binding rules run first. The engine never sees a payload that breaks
/// them, so such a payload never enters the block tree of the execution client.
/// A payload that keeps the rules gets its verdict from [`validate_consensus_block`].
pub async fn establish_block_validity(
    payload_validator: &impl PayloadValidator,
    block: &ConsensusBlock,
    previous_block: Option<&ExecutionBlock>,
    store: &impl InvalidPayloadsRepository,
    metrics: &AppMetrics,
) -> eyre::Result<BlockVerdict> {
    if let Err(error) =
        check_payload_binding(&block.execution_payload, block.height, previous_block)
    {
        warn!(
            height = %block.height,
            round = %block.round,
            block_hash = %block.self_reported_block_hash(),
            proposer = %block.proposer,
            reason = %error,
            "Payload is not bound to its place in the chain, storing for forensics",
        );

        metrics.inc_invalid_payloads_count(error.invalid_payload_source());

        persist_invalid_payload_best_effort(
            store,
            InvalidPayload::new_from_block(block, &error.to_string()),
            block.height,
            block.round,
            block.proposer,
        )
        .await;

        return Ok(BlockVerdict::Unbound(error));
    }

    validate_consensus_block(payload_validator, block, store, metrics)
        .await
        .map(BlockVerdict::Engine)
}

/// Persists a forensic [`InvalidPayload`] record on a best-effort basis.
///
/// A persistence failure is logged at `error` and swallowed so it cannot abort the
/// caller's primary path. For call sites where the block was never assembled, so only
/// `height`, `round`, and `proposer` are available; [`validate_consensus_block`] logs
/// `block_hash` too because it has the assembled block.
pub(crate) async fn persist_invalid_payload_best_effort(
    store: &impl InvalidPayloadsRepository,
    invalid: InvalidPayload,
    height: Height,
    round: Round,
    proposer: Address,
) {
    if let Err(e) = store.append(invalid).await {
        error!(
            %height, %round, %proposer,
            "Failed to persist invalid-payload forensic record: {e}",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use eyre::eyre;

    use malachitebft_app_channel::app::types::core::Validity;

    use alloy_primitives::{Address as AlloyAddress, Bloom, Bytes as AlloyBytes, U256};
    use alloy_rpc_types_engine::{
        ExecutionPayloadV1, ExecutionPayloadV2, ExecutionPayloadV3, PayloadStatus,
    };

    use arc_consensus_types::{Address, Height, Round, B256};
    use arc_eth_engine::engine::{MockEngineAPI, MockEthereumAPI};
    use arc_eth_engine::json_structures::ExecutionBlock;

    use crate::block::ConsensusBlock;
    use crate::metrics::app::AppMetrics;
    use crate::store::repositories::mocks::MockInvalidPayloadsRepository;
    use arc_consensus_db::invalid_payloads::InvalidPayload;

    fn test_payload(timestamp: u64) -> ExecutionPayloadV3 {
        ExecutionPayloadV3 {
            payload_inner: ExecutionPayloadV2 {
                payload_inner: ExecutionPayloadV1 {
                    parent_hash: B256::ZERO,
                    fee_recipient: AlloyAddress::ZERO,
                    state_root: B256::ZERO,
                    receipts_root: B256::ZERO,
                    logs_bloom: Bloom::default(),
                    prev_randao: B256::ZERO,
                    block_number: 0,
                    gas_limit: 0,
                    gas_used: 0,
                    timestamp,
                    extra_data: AlloyBytes::default(),
                    base_fee_per_gas: U256::from(1u64),
                    block_hash: B256::ZERO,
                    transactions: vec![],
                },
                withdrawals: vec![],
            },
            blob_gas_used: 0,
            excess_blob_gas: 0,
        }
    }

    #[tokio::test]
    async fn validate_payload_returns_valid_on_ok_status() {
        let mut mock = MockEngineAPI::new();
        mock.expect_new_payload().returning(|_, _, _, _| {
            Ok(PayloadStatus {
                status: PayloadStatusEnum::Valid,
                latest_valid_hash: None,
            })
        });

        let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
        let payload = test_payload(0);
        let metrics = AppMetrics::default();

        let result = validate_payload(&engine, &payload, &metrics, None)
            .await
            .expect("payload validation should succeed");

        assert_eq!(result, PayloadValidationResult::Valid);
    }

    #[tokio::test]
    async fn validate_payload_returns_invalid_on_invalid_status() {
        let mut mock = MockEngineAPI::new();
        mock.expect_new_payload().returning(|_, _, _, _| {
            Ok(PayloadStatus {
                status: PayloadStatusEnum::Invalid {
                    validation_error: "validation error".to_string(),
                },
                latest_valid_hash: None,
            })
        });

        let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
        let payload = test_payload(0);
        let metrics = AppMetrics::default();

        let result = validate_payload(&engine, &payload, &metrics, None)
            .await
            .expect("payload validation should succeed");

        assert_eq!(
            result,
            PayloadValidationResult::Invalid {
                reason: "validation error".to_string(),
            },
        );
    }

    #[tokio::test]
    async fn validate_payload_returns_invalid_on_non_internal_rpc_error() {
        let mut mock = MockEngineAPI::new();
        mock.expect_new_payload().returning(|_, _, _, _| {
            let rpc_error = EngineApiRpcError::new(42, "engine API error", None);
            Err(eyre::Report::new(rpc_error))
        });

        let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
        let payload = test_payload(0);
        let metrics = AppMetrics::default();

        let result = validate_payload(&engine, &payload, &metrics, None)
            .await
            .expect("should succeed without error");

        match &result {
            PayloadValidationResult::Invalid { reason } => {
                assert!(
                    reason.contains("engine API error"),
                    "reason should contain the RPC error message, got: {reason}",
                );
            }
            other => {
                panic!("expected Invalid, got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn validate_payload_propagates_internal_rpc_error() {
        let mut mock = MockEngineAPI::new();
        mock.expect_new_payload().returning(|_, _, _, _| {
            let rpc_error = EngineApiRpcError::new(-32603, "Internal error", None);
            Err(eyre::Report::new(rpc_error))
        });

        let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
        let payload = test_payload(0);
        let metrics = AppMetrics::default();

        let err = validate_payload(&engine, &payload, &metrics, None)
            .await
            .expect_err("internal RPC error should not produce a payload verdict");

        let engine_api_error = EngineApiRpcError::try_from(&err)
            .expect("error chain should preserve the Engine API error");
        assert!(engine_api_error.is_internal_error());
        assert!(
            err.to_string()
                .contains("call to EngineAPI::new_payload failed"),
            "error message should describe the failure, got: {err}",
        );
    }

    #[tokio::test]
    async fn validate_payload_propagates_other_errors() {
        let mut mock = MockEngineAPI::new();
        mock.expect_new_payload()
            .returning(|_, _, _, _| Err(eyre::eyre!("some error")));

        let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
        let payload = test_payload(0);
        let metrics = AppMetrics::default();

        let err = validate_payload(&engine, &payload, &metrics, None)
            .await
            .expect_err("payload validation should return an error");

        let msg = err.to_string();
        assert!(
            msg.contains("call to EngineAPI::new_payload failed"),
            "error message should describe the failure, got: {msg}",
        );
    }

    #[tokio::test]
    async fn validate_payload_returns_err_on_unexpected_status() {
        let test_cases = [PayloadStatusEnum::Syncing, PayloadStatusEnum::Accepted];

        for status in test_cases {
            let mut mock = MockEngineAPI::new();
            let status_for_mock = status.clone();
            mock.expect_new_payload().returning(move |_, _, _, _| {
                Ok(PayloadStatus {
                    status: status_for_mock.clone(),
                    latest_valid_hash: None,
                })
            });

            let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
            let payload = test_payload(0);
            let metrics = AppMetrics::default();

            let result = validate_payload(&engine, &payload, &metrics, None)
                .await
                .expect_err("payload validation should return an error");

            let got_msg = result.to_string();
            let want_status = PayloadStatus {
                status,
                latest_valid_hash: None,
            };
            let want_err_msg = format!(
                "unexpected {want_status:?} status from engine for block {} \
                 at height {}",
                payload.payload_inner.payload_inner.block_hash,
                payload.payload_inner.payload_inner.block_number,
            );
            assert_eq!(got_msg, want_err_msg);
        }
    }

    fn test_block() -> ConsensusBlock {
        ConsensusBlock {
            height: Height::new(1),
            round: Round::new(0),
            valid_round: Round::Nil,
            proposer: Address::new([0u8; 20]),
            execution_payload: test_payload(0),
            validity: Validity::Valid,
            signature: None,
        }
    }

    /// Payload that carries `height` as its block number and `parent_hash` as its parent.
    fn bound_payload(height: u64, parent_hash: B256) -> ExecutionPayloadV3 {
        let mut payload = test_payload(0);
        payload.payload_inner.payload_inner.block_number = height;
        payload.payload_inner.payload_inner.parent_hash = parent_hash;
        payload
    }

    fn prev_block(number: u64, block_hash: B256) -> ExecutionBlock {
        ExecutionBlock {
            block_hash,
            block_number: number,
            parent_hash: B256::ZERO,
            timestamp: 0,
        }
    }

    fn block_at(height: u64, payload: ExecutionPayloadV3) -> ConsensusBlock {
        ConsensusBlock {
            height: Height::new(height),
            round: Round::new(0),
            valid_round: Round::Nil,
            proposer: Address::new([0u8; 20]),
            execution_payload: payload,
            validity: Validity::Valid,
            signature: None,
        }
    }

    #[test]
    fn binding_accepts_payload_at_its_height_that_extends_the_previous_block() {
        let parent = B256::repeat_byte(0x11);
        let payload = bound_payload(11, parent);

        check_payload_binding(&payload, Height::new(11), Some(&prev_block(10, parent)))
            .expect("a payload at its height that extends the previous block is bound");
    }

    /// A fresh block forked from an older canonical ancestor: block number 5
    /// proposed at height 11.
    #[test]
    fn binding_rejects_block_number_below_the_consensus_height() {
        let payload = bound_payload(5, B256::repeat_byte(0x44));

        let error = check_payload_binding(&payload, Height::new(11), None)
            .expect_err("block number 5 does not belong at height 11");

        assert!(
            matches!(
                error,
                PayloadBindingError::HeightMismatch {
                    expected: 11,
                    actual: 5
                }
            ),
            "got {error:?}",
        );
    }

    /// The height rule holds even when the payload extends the previous block,
    /// which is what a payload replayed from an older height looks like.
    #[test]
    fn binding_rejects_block_number_above_the_consensus_height() {
        let parent = B256::repeat_byte(0x11);
        let payload = bound_payload(12, parent);

        let error = check_payload_binding(&payload, Height::new(11), Some(&prev_block(10, parent)))
            .expect_err("block number 12 does not belong at height 11");

        assert!(
            matches!(
                error,
                PayloadBindingError::HeightMismatch {
                    expected: 11,
                    actual: 12
                }
            ),
            "got {error:?}",
        );
    }

    #[test]
    fn binding_rejects_parent_that_is_not_the_previous_block() {
        let expected = B256::repeat_byte(0x11);
        let other = B256::repeat_byte(0x22);
        let payload = bound_payload(11, other);

        let error =
            check_payload_binding(&payload, Height::new(11), Some(&prev_block(10, expected)))
                .expect_err("a payload that extends another block is not bound");

        match error {
            PayloadBindingError::ParentMismatch {
                expected: e,
                actual: a,
            } => {
                assert_eq!(e, expected);
                assert_eq!(a, other);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// Batch value sync leaves `previous_block` several heights behind, so it is
    /// not the parent of the payload under test and the parent rule cannot apply.
    #[test]
    fn binding_skips_the_parent_rule_when_the_previous_block_lags() {
        let payload = bound_payload(11, B256::repeat_byte(0x22));

        check_payload_binding(
            &payload,
            Height::new(11),
            Some(&prev_block(7, B256::repeat_byte(0x11))),
        )
        .expect("the parent rule does not apply to a previous block that lags");
    }

    #[test]
    fn binding_skips_the_parent_rule_without_a_previous_block() {
        let payload = bound_payload(11, B256::repeat_byte(0x22));

        check_payload_binding(&payload, Height::new(11), None)
            .expect("the parent rule needs a previous block");
    }

    #[tokio::test]
    async fn establish_block_validity_rejects_an_unbound_payload_without_asking_the_engine() {
        let mut validator = MockPayloadValidator::new();
        validator.expect_validate_payload().times(0);

        let mut store = MockInvalidPayloadsRepository::new();
        store
            .expect_append()
            .times(1)
            .withf(|ip: &InvalidPayload| {
                ip.height == Height::new(11)
                    && ip.reason.contains("does not match consensus height")
            })
            .returning(|_| Ok(()));

        let metrics = AppMetrics::default();
        let block = block_at(11, bound_payload(5, B256::ZERO));

        let verdict = establish_block_validity(&validator, &block, None, &store, &metrics)
            .await
            .expect("a binding error is a verdict, not a failure");

        assert_eq!(
            verdict,
            BlockVerdict::Unbound(PayloadBindingError::HeightMismatch {
                expected: 11,
                actual: 5,
            }),
            "the caller must be able to tell which rule rejected the payload",
        );
        assert_eq!(verdict.validity(), Validity::Invalid);
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    /// The parent rule reaches the caller as its own variant. A caller that answers
    /// the two rules differently reads this, and the two rules blame different
    /// parties.
    #[tokio::test]
    async fn establish_block_validity_reports_which_rule_rejected_the_payload() {
        let expected = B256::repeat_byte(0xAB);

        let mut validator = MockPayloadValidator::new();
        validator.expect_validate_payload().times(0);

        let mut store = MockInvalidPayloadsRepository::new();
        store.expect_append().times(1).returning(|_| Ok(()));

        let metrics = AppMetrics::default();
        let actual = B256::repeat_byte(0xCD);
        let block = block_at(11, bound_payload(11, actual));

        let verdict = establish_block_validity(
            &validator,
            &block,
            Some(&prev_block(10, expected)),
            &store,
            &metrics,
        )
        .await
        .expect("a binding error is a verdict, not a failure");

        assert_eq!(
            verdict,
            BlockVerdict::Unbound(PayloadBindingError::ParentMismatch { expected, actual }),
        );
        assert_eq!(
            metrics.get_invalid_payloads_count_by_source(InvalidPayloadSource::PayloadParent),
            1,
        );
    }

    #[tokio::test]
    async fn establish_block_validity_asks_the_engine_about_a_bound_payload() {
        let parent = B256::repeat_byte(0x11);

        let mut validator = MockPayloadValidator::new();
        validator
            .expect_validate_payload()
            .times(1)
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut store = MockInvalidPayloadsRepository::new();
        store.expect_append().times(0);

        let metrics = AppMetrics::default();
        let block = block_at(11, bound_payload(11, parent));

        let verdict = establish_block_validity(
            &validator,
            &block,
            Some(&prev_block(10, parent)),
            &store,
            &metrics,
        )
        .await
        .expect("should succeed");

        assert_eq!(verdict, BlockVerdict::Engine(Validity::Valid));
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn validate_consensus_block_returns_valid() {
        let mut validator = MockPayloadValidator::new();
        validator
            .expect_validate_payload()
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut store = MockInvalidPayloadsRepository::new();
        store.expect_append().times(0);

        let metrics = AppMetrics::default();
        let block = test_block();
        let result = validate_consensus_block(&validator, &block, &store, &metrics)
            .await
            .expect("should succeed");

        assert_eq!(result, Validity::Valid);
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn validate_consensus_block_returns_invalid_and_stores() {
        let mut validator = MockPayloadValidator::new();
        validator.expect_validate_payload().returning(|_| {
            Ok(PayloadValidationResult::Invalid {
                reason: "bad block".into(),
            })
        });

        let mut store = MockInvalidPayloadsRepository::new();
        store
            .expect_append()
            .times(1)
            .withf(|ip: &InvalidPayload| {
                ip.height == Height::new(1)
                    && ip.round == Round::new(0)
                    && ip.proposer_address == Address::new([0u8; 20])
                    && ip.reason == "bad block"
                    && ip.payload.is_some()
            })
            .returning(|_| Ok(()));

        let metrics = AppMetrics::default();
        let block = test_block();
        let result = validate_consensus_block(&validator, &block, &store, &metrics)
            .await
            .expect("should succeed");

        assert_eq!(result, Validity::Invalid);
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    #[tokio::test]
    async fn validate_consensus_block_propagates_validation_error() {
        let mut validator = MockPayloadValidator::new();
        validator
            .expect_validate_payload()
            .returning(|_| Err(eyre!("engine down")));

        let mut store = MockInvalidPayloadsRepository::new();
        store.expect_append().times(0);

        let metrics = AppMetrics::default();
        let block = test_block();
        let err = validate_consensus_block(&validator, &block, &store, &metrics)
            .await
            .expect_err("should propagate error");

        assert!(
            err.to_string().contains("engine down"),
            "error should contain the original message, \
             got: {err}",
        );
        assert_eq!(metrics.get_invalid_payloads_count(), 0);
    }

    #[tokio::test]
    async fn validate_consensus_block_returns_invalid_when_forensics_persist_fails() {
        // When the engine returns Invalid but persisting the forensic record
        // fails (e.g. transient DB issue), the engine's verdict is still the
        // authoritative answer and must be returned. Otherwise the caller
        // (validate_undecided_blocks) treats it as "no verdict obtained" and
        // leaves the placeholder `Valid` in undecided_blocks, masking a
        // rejected block.
        let mut validator = MockPayloadValidator::new();
        validator.expect_validate_payload().returning(|_| {
            Ok(PayloadValidationResult::Invalid {
                reason: "bad".into(),
            })
        });

        let mut store = MockInvalidPayloadsRepository::new();
        store
            .expect_append()
            .times(1)
            .returning(|_| Err(std::io::Error::other("disk full")));

        let metrics = AppMetrics::default();
        let block = test_block();
        let validity = validate_consensus_block(&validator, &block, &store, &metrics)
            .await
            .expect("verdict should be returned even when forensics persist fails");

        assert_eq!(validity, Validity::Invalid);
        assert_eq!(metrics.get_invalid_payloads_count(), 1);
    }

    #[tokio::test]
    async fn persist_invalid_payload_best_effort_swallows_persistence_failure() {
        let mut store = MockInvalidPayloadsRepository::new();
        store
            .expect_append()
            .times(1)
            .returning(|_| Err(std::io::Error::other("disk full")));

        let height = Height::new(1);
        let round = Round::new(0);
        let proposer = Address::new([0u8; 20]);
        let invalid = InvalidPayload::new_without_payload(height, round, proposer, "bad");

        persist_invalid_payload_best_effort(&store, invalid, height, round, proposer).await;
    }

    #[derive(Clone, Debug)]
    enum Scenario {
        Success,
        UnknownPayloadUntil { succeed_on: usize },
        OtherError,
    }

    struct TestPayloadGenerator {
        scenario: Scenario,
        attempts: AtomicUsize,
    }

    impl TestPayloadGenerator {
        fn new(scenario: Scenario) -> Self {
            Self {
                scenario,
                attempts: AtomicUsize::new(0),
            }
        }

        fn dummy_payload(timestamp: u64) -> ExecutionPayloadV3 {
            test_payload(timestamp)
        }
    }

    impl PayloadGenerator for TestPayloadGenerator {
        async fn generate_block(
            &self,
            _parent: &ExecutionBlock,
            timestamp: u64,
            _fee_recipient: &Address,
        ) -> eyre::Result<ExecutionPayloadV3> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            match self.scenario {
                Scenario::Success => Ok(Self::dummy_payload(timestamp)),
                Scenario::UnknownPayloadUntil { succeed_on } => {
                    if attempt < succeed_on {
                        Err(EngineApiRpcError::new(-38001, "Unknown payload", None).into())
                    } else {
                        Ok(Self::dummy_payload(timestamp))
                    }
                }
                Scenario::OtherError => Err(eyre!("a different error")),
            }
        }
    }

    fn parent_block(timestamp: u64) -> ExecutionBlock {
        ExecutionBlock {
            block_hash: B256::ZERO,
            block_number: 0,
            parent_hash: B256::ZERO,
            timestamp,
        }
    }

    fn fee_recipient() -> Address {
        AlloyAddress::ZERO.into()
    }

    fn metrics() -> AppMetrics {
        AppMetrics::new()
    }

    #[tokio::test]
    async fn retry_success_first_attempt() {
        let generator = TestPayloadGenerator::new(Scenario::Success);
        let payload =
            generate_payload_with_retry(&parent_block(0), &fee_recipient(), &generator, &metrics())
                .await
                .expect("payload generation should succeed on first try");

        assert_eq!(
            generator.attempts.load(Ordering::SeqCst),
            1,
            "should only attempt once"
        );
        assert!(payload.timestamp() >= parent_block(0).timestamp);
    }

    #[tokio::test]
    async fn retry_unknown_until_success() {
        let succeed_on = 6; // 5 failures + 1 success; limit of max retries
        let generator = TestPayloadGenerator::new(Scenario::UnknownPayloadUntil { succeed_on });
        let payload = generate_payload_with_retry(
            &parent_block(10),
            &fee_recipient(),
            &generator,
            &metrics(),
        )
        .await
        .expect("payload should eventually succeed");

        assert_eq!(
            generator.attempts.load(Ordering::SeqCst),
            succeed_on,
            "attempt count should equal succeed_on"
        );
        assert!(payload.timestamp() >= parent_block(10).timestamp);
    }

    #[tokio::test]
    async fn retry_unknown_too_late() {
        let succeed_on = 7; // exceeds max retries
        let generator = TestPayloadGenerator::new(Scenario::UnknownPayloadUntil { succeed_on });
        let err = generate_payload_with_retry(
            &parent_block(100),
            &fee_recipient(),
            &generator,
            &metrics(),
        )
        .await
        .expect_err("should fail after exhausting retries");

        let engine_err =
            EngineApiRpcError::try_from(err).expect("error should be EngineApiRpcError");
        assert!(
            engine_err.is_unknown_payload(),
            "error should be UnknownPayload kind"
        );
        assert_eq!(
            generator.attempts.load(Ordering::SeqCst),
            6,
            "total attempts should be 6 (1 initial + 5 retries)"
        );
    }

    #[tokio::test]
    async fn retry_immediate_other_error() {
        let generator = TestPayloadGenerator::new(Scenario::OtherError);
        let err = generate_payload_with_retry(
            &parent_block(1000),
            &fee_recipient(),
            &generator,
            &metrics(),
        )
        .await
        .expect_err("should fail immediately without retry");

        if let Ok(engine_err) = EngineApiRpcError::try_from(err) {
            assert!(
                !engine_err.is_unknown_payload(),
                "should not classify as UnknownPayload"
            );
        }
        assert_eq!(
            generator.attempts.load(Ordering::SeqCst),
            1,
            "should only attempt once"
        );
    }
}
