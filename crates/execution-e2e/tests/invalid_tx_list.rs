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

//! E2E tests for the invalid_tx_list functionality.
//!
//! The invalid_tx_list is an LRU cache that stores transaction hashes of transactions that
//! caused payload builder failures. This enables fast rejection during validation pre-check
//! to avoid repeatedly attempting to build blocks with problematic transactions.
//!
//! Key behavior:
//! - Unprocessable transactions (wrapped as `UnprocessableTransactionError`) are added to the cache
//! - When the payload builder panics, all pending transactions are added to the cache
//! - Cached transactions are rejected during validation pre-check with InvalidTxError
//! - LRU eviction removes oldest entries when capacity is exceeded
//!
//! Test coverage:
//! - Basic functionality: cache miss allows validation
//! - Disabled invalid_tx_list falls through to full validation
//! - Payload builder panic populates cache and resubmission is rejected

use arc_execution_e2e::{
    actions::{AssertTxIncluded, ProduceBlocks, SendTransaction, TxStatus},
    ArcSetup, ArcTestBuilder,
};
use arc_execution_txpool::InvalidTxListConfig;
use eyre::Result;
use rstest::rstest;

/// Verifies that transactions not in the invalid_tx_list go through full validation
/// and are included in blocks across different configurations:
/// - Enabled with small/large capacity
/// - Disabled (falls through to full validation)
/// - Multiple independent transactions in a single block
#[rstest]
#[case::enabled(true, 1000, 1)]
#[case::disabled(false, 0, 1)]
#[case::large_capacity(true, 100_000, 1)]
#[case::multiple_txs(true, 1000, 3)]
#[tokio::test]
async fn test_normal_tx_processing(
    #[case] enabled: bool,
    #[case] capacity: u32,
    #[case] num_txs: usize,
) -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut builder = ArcTestBuilder::new().with_setup(
        ArcSetup::new().with_invalid_tx_list_config(InvalidTxListConfig { enabled, capacity }),
    );

    let tx_names: Vec<String> = (1..=num_txs).map(|i| format!("tx{i}")).collect();
    for name in &tx_names {
        builder = builder.with_action(SendTransaction::new(name));
    }

    builder = builder.with_action(ProduceBlocks::new(1));

    for name in &tx_names {
        builder = builder.with_action(AssertTxIncluded::new(name).expect(TxStatus::Success));
    }

    builder.run().await
}

/// Payload Builder Panic Populates Invalid TX List and Resubmission is Rejected
///
/// Replicates the production flow when a single transaction causes a panic during execution:
/// 1. Submit two transactions: one valid transfer, one targeting a panicking precompile
/// 2. Trigger payload building — the per-transaction `catch_unwind` (payload.rs:589-622)
///    catches the panic and wraps it as `UnprocessableTransactionError`. The outer
///    `handle_build_res` calls `purge_unprocessable_tx`, which removes only the
///    offending transaction from the pool and adds it to the invalid_tx_list.
///    The valid transaction remains in the pool.
/// 3. Produce a subsequent block — succeeds (valid tx is included)
/// 4. Resubmit the panicking transaction — rejected with InvalidTxError
#[cfg(feature = "integration")]
#[tokio::test]
async fn test_payload_builder_panic_populates_invalid_tx_list() -> Result<()> {
    use alloy_primitives::U256;
    use arc_execution_e2e::ArcEnvironment;
    use arc_execution_txpool::ArcTransactionValidatorError;
    use arc_precompiles::precompile_provider::PANIC_PRECOMPILE_ADDRESS;
    use reth_transaction_pool::error::{PoolError, PoolErrorKind};
    use reth_transaction_pool::{TransactionOrigin, TransactionPool};

    reth_tracing::init_test_tracing();

    let mut env = ArcEnvironment::new();
    ArcSetup::new()
        .with_invalid_tx_list_config(InvalidTxListConfig {
            enabled: true,
            capacity: 1000,
        })
        .apply(&mut env)
        .await?;

    // Step 1: Submit two transactions, one valid, and one targeting the panicking precompile
    let (good_tx_hash, _) = SendTransaction::new("good_tx")
        .execute_and_return(&mut env)
        .await?;

    let (panicking_tx_hash, panicking_tx) = SendTransaction::new("panic_tx")
        .with_to(PANIC_PRECOMPILE_ADDRESS)
        .with_value(U256::ZERO)
        .with_gas_limit(100_000)
        .execute_and_return(&mut env)
        .await?;

    // Step 2: Attempt to produce a block. The payload builder executes both txs.
    // The panicking precompile triggers a panic caught by the per-transaction
    // catch_unwind, which wraps it as UnprocessableTransactionError. Only the
    // offending tx is purged from the pool and added to invalid_tx_list.
    // The build itself fails, but the side effect is what we're testing.
    let mut produce = ProduceBlocks::new(1);
    let result = arc_execution_e2e::Action::execute(&mut produce, &mut env).await;
    assert!(
        result.is_err(),
        "Expected payload building to fail after panic"
    );

    // Assert the panicking tx was purged from the pool
    let pool_size = env.node().inner.pool.len();
    assert_eq!(
        pool_size, 1,
        "Pool should have one transaction after panicking tx is purged"
    );

    assert!(
        env.node().inner.pool.contains(&good_tx_hash),
        "Good tx should be in the pool"
    );

    assert!(
        !env.node().inner.pool.contains(&panicking_tx_hash),
        "Panicking tx should not be in the pool"
    );

    // Step 3: Produce a block — succeeds now that the panicking tx has been purged
    let mut produce_after = ProduceBlocks::new(1);
    arc_execution_e2e::Action::execute(&mut produce_after, &mut env).await?;

    // Step 4: Resubmit the panicking transaction — should be rejected by invalid_tx_list
    let result = env
        .node()
        .inner
        .pool
        .add_consensus_transaction(panicking_tx, TransactionOrigin::Local)
        .await;

    match result {
        Err(PoolError {
            kind: PoolErrorKind::InvalidTransaction(ref e),
            ..
        }) => {
            let arc_err = e
                .downcast_other_ref::<ArcTransactionValidatorError>()
                .expect("Expected ArcTransactionValidatorError");
            assert!(
                matches!(arc_err, ArcTransactionValidatorError::InvalidTxError),
                "Expected InvalidTxError, got: {arc_err:?}"
            );
            Ok(())
        }
        Ok(_) => Err(eyre::eyre!(
            "Transaction {panicking_tx_hash} accepted on resubmission, expected rejection"
        )),
        Err(e) => Err(eyre::eyre!(
            "Transaction {panicking_tx_hash} rejected with unexpected error: {e:?}"
        )),
    }
}
