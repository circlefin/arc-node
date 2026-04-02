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

use arc_consensus_types::Address;
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_eth_engine::rpc::EngineApiRpcError;

use crate::block::ConsensusBlock;
use crate::metrics::app::AppMetrics;
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
            warn!(
                timestamp = timestamp,
                skew = timestamp - now,
                "Clock skew detected: using parent timestamp",
            );
        }

        let _guard = metrics.start_engine_api_timer("generate_block");

        generator
            .generate_block(previous_block, timestamp, fee_recipient)
            .await
    };

    let mut attempt_num = 0;

    call_once
        .retry(RETRY_POLICY.build())
        .sleep(tokio::time::sleep) // give reth time to breathe
        .notify(|_e, dur| {
            attempt_num += 1;
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
}

impl<'a> PayloadGenerator for EnginePayloadGenerator<'a> {
    async fn generate_block(
        &self,
        parent: &ExecutionBlock,
        timestamp: u64,
        fee_recipient: &Address,
    ) -> eyre::Result<ExecutionPayloadV3> {
        self.engine
            .generate_block(parent, timestamp, fee_recipient)
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
}

impl<'a> EnginePayloadValidator<'a> {
    pub fn new(engine: &'a Engine, metrics: &'a AppMetrics) -> Self {
        Self { engine, metrics }
    }
}

impl PayloadValidator for EnginePayloadValidator<'_> {
    async fn validate_payload(
        &self,
        payload: &ExecutionPayloadV3,
    ) -> eyre::Result<PayloadValidationResult> {
        validate_payload(self.engine, payload, self.metrics).await
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
/// - `Ok(Valid)`: the engine accepted the payload, or returned an unexpected status
///   such as `SYNCING` or `ACCEPTED` (logged as a warning).
/// - `Ok(Invalid { reason })`: the engine explicitly rejected the payload, either
///   via its status response (`INVALID`) or via a JSON-RPC error
///   (`EngineApiRpcError`).
/// - `Err(..)`: the engine replied with status `SYNCING` or `ACCEPTED`, or an
///   unrelated internal error occurred in the call stack.
async fn validate_payload(
    engine: &Engine,
    execution_payload: &ExecutionPayloadV3,
    metrics: &AppMetrics,
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
        .notify_new_block(execution_payload, versioned_hashes)
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
                // JSON-RPC error here means that the call to
                // `engine.newPayload` failed the preliminary structural
                // validation of the payload.
                // Instead of returning an error and possibly crashing the app,
                // we mark the payload as invalid.
                error!(
                    %block_hash,
                    "Invalid payload: {engine_api_error}",
                );
                return Ok(PayloadValidationResult::Invalid {
                    reason: engine_api_error.to_string(),
                });
            }

            // Unrelated internal error in the call stack.
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
pub async fn validate_consensus_block(
    payload_validator: &impl PayloadValidator,
    block: &ConsensusBlock,
    store: &impl InvalidPayloadsRepository,
) -> eyre::Result<Validity> {
    let result = payload_validator
        .validate_payload(&block.execution_payload)
        .await?;

    match result {
        PayloadValidationResult::Valid => Ok(Validity::Valid),
        PayloadValidationResult::Invalid { reason } => {
            let invalid = InvalidPayload::new_from_block(block, &reason);
            store.append(invalid).await?;
            Ok(Validity::Invalid)
        }
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
        mock.expect_new_payload().returning(|_, _, _| {
            Ok(PayloadStatus {
                status: PayloadStatusEnum::Valid,
                latest_valid_hash: None,
            })
        });

        let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
        let payload = test_payload(0);
        let metrics = AppMetrics::default();

        let result = validate_payload(&engine, &payload, &metrics)
            .await
            .expect("payload validation should succeed");

        assert_eq!(result, PayloadValidationResult::Valid);
    }

    #[tokio::test]
    async fn validate_payload_returns_invalid_on_invalid_status() {
        let mut mock = MockEngineAPI::new();
        mock.expect_new_payload().returning(|_, _, _| {
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

        let result = validate_payload(&engine, &payload, &metrics)
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
    async fn validate_payload_returns_invalid_on_rpc_error() {
        let mut mock = MockEngineAPI::new();
        mock.expect_new_payload().returning(|_, _, _| {
            let rpc_error = EngineApiRpcError::new(42, "engine API error", None);
            Err(eyre::Report::new(rpc_error))
        });

        let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
        let payload = test_payload(0);
        let metrics = AppMetrics::default();

        let result = validate_payload(&engine, &payload, &metrics)
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
    async fn validate_payload_propagates_other_errors() {
        let mut mock = MockEngineAPI::new();
        mock.expect_new_payload()
            .returning(|_, _, _| Err(eyre::eyre!("some error")));

        let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
        let payload = test_payload(0);
        let metrics = AppMetrics::default();

        let err = validate_payload(&engine, &payload, &metrics)
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
            mock.expect_new_payload().returning(move |_, _, _| {
                Ok(PayloadStatus {
                    status: status_for_mock.clone(),
                    latest_valid_hash: None,
                })
            });

            let engine = Engine::new(Box::new(mock), Box::new(MockEthereumAPI::new()));
            let payload = test_payload(0);
            let metrics = AppMetrics::default();

            let result = validate_payload(&engine, &payload, &metrics)
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

    #[tokio::test]
    async fn validate_consensus_block_returns_valid() {
        let mut validator = MockPayloadValidator::new();
        validator
            .expect_validate_payload()
            .returning(|_| Ok(PayloadValidationResult::Valid));

        let mut store = MockInvalidPayloadsRepository::new();
        store.expect_append().times(0);

        let block = test_block();
        let result = validate_consensus_block(&validator, &block, &store)
            .await
            .expect("should succeed");

        assert_eq!(result, Validity::Valid);
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

        let block = test_block();
        let result = validate_consensus_block(&validator, &block, &store)
            .await
            .expect("should succeed");

        assert_eq!(result, Validity::Invalid);
    }

    #[tokio::test]
    async fn validate_consensus_block_propagates_validation_error() {
        let mut validator = MockPayloadValidator::new();
        validator
            .expect_validate_payload()
            .returning(|_| Err(eyre!("engine down")));

        let mut store = MockInvalidPayloadsRepository::new();
        store.expect_append().times(0);

        let block = test_block();
        let err = validate_consensus_block(&validator, &block, &store)
            .await
            .expect_err("should propagate error");

        assert!(
            err.to_string().contains("engine down"),
            "error should contain the original message, \
             got: {err}",
        );
    }

    #[tokio::test]
    async fn validate_consensus_block_propagates_store_error() {
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

        let block = test_block();
        let err = validate_consensus_block(&validator, &block, &store)
            .await
            .expect_err("should propagate store error");

        assert!(
            err.to_string().contains("disk full"),
            "error should contain the store error message, \
             got: {err}",
        );
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
