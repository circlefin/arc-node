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

use alloy_primitives::{address, TxHash};
use alloy_rpc_types_eth::TransactionRequest;
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use arc_execution_txpool::InvalidTxListConfig;
use arc_precompiles::precompile_provider::PANIC_PRECOMPILE_ADDRESS;
use eyre::Result;
use jsonrpsee::core::client::Error as RpcClientError;
use reth_transaction_pool::TransactionPool;
use rstest::rstest;

const NORMAL_TX_RECIPIENT: alloy_primitives::Address =
    address!("0x000000000000000000000000000000000000bEEF");

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

    let mut node = ArcTestNode::start(
        ArcSetup::new().with_invalid_tx_list_config(InvalidTxListConfig { enabled, capacity }),
    )
    .await?;
    let signer = node.wallet_signer(0)?;

    let mut txs = Vec::with_capacity(num_txs);
    for _ in 0..num_txs {
        txs.push(
            node.send_tx(
                signer.clone(),
                TransactionRequest {
                    from: Some(signer.address()),
                    to: Some(TxKind::Call(NORMAL_TX_RECIPIENT)),
                    ..Default::default()
                },
            )
            .await?,
        );
    }

    node.produce_block().await?;

    for tx in txs {
        let receipt = node.get_receipt(tx).await?;
        assert!(receipt.status());
    }

    Ok(())
}

/// Payload builder panic populates invalid tx list and resubmission is rejected.
///
/// Replicates the production flow when a single transaction causes a panic during execution:
/// 1. Submit two transactions: one valid transfer, one targeting a panicking precompile
/// 2. Trigger payload building - the per-transaction `catch_unwind` (payload.rs:589-622)
///    catches the panic and wraps it as `UnprocessableTransactionError`. Only the
///    offending transaction is purged from the pool and added to the invalid_tx_list.
///    The valid transaction remains includable.
/// 3. Resubmit the panicking transaction — rejected by `eth_sendRawTransaction`.
#[tokio::test]
async fn test_payload_builder_panic_populates_invalid_tx_list() -> Result<()> {
    use alloy_primitives::U256;

    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new().with_invalid_tx_list_config(
        InvalidTxListConfig {
            enabled: true,
            capacity: 1000,
        },
    ))
    .await?;
    let signer = node.wallet_signer(0)?;
    // Step 1: Submit two transactions, one valid, and one targeting the panicking precompile
    let good_tx_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(NORMAL_TX_RECIPIENT)),
                ..Default::default()
            },
        )
        .await?;

    // Keep the signed transaction so the test can resubmit it later and
    // inspect the raw txpool rejection.
    let panicking_tx_signed = node
        .sign_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(PANIC_PRECOMPILE_ADDRESS)),
                value: Some(U256::ZERO),
                gas: Some(100_000),
                ..Default::default()
            },
        )
        .await?;
    let panicking_tx_hash = *panicking_tx_signed.tx_hash();
    node.send_signed_tx(panicking_tx_signed.clone()).await?;

    // Step 2: The panicking tx is quarantined. The first build may either fail after
    // quarantine or succeed with the valid tx, so only assert the stable invariant here.
    let first_produce_result = node.produce_block().await;
    let pool = &node.node.inner.pool;
    assert!(
        !pool.contains(&panicking_tx_hash),
        "Panicking tx should be removed from the pool"
    );

    if first_produce_result.is_err() {
        let pool = &node.node.inner.pool;
        assert!(
            pool.contains(&good_tx_hash),
            "Good tx should remain in the pool when the first build fails"
        );
        node.produce_block().await?;
    }

    let receipt = node.get_receipt(good_tx_hash).await?;
    assert!(receipt.status());
    let pool = &node.node.inner.pool;
    assert!(
        !pool.contains(&good_tx_hash),
        "Good tx should be removed from the pool after inclusion"
    );
    assert!(
        !pool.contains(&panicking_tx_hash),
        "Panicking tx should remain removed from the pool"
    );

    // Step 3: Resubmit the panicking transaction — should be rejected by invalid_tx_list.
    expect_invalid_tx_list_rejection(
        node.send_signed_tx(panicking_tx_signed).await,
        panicking_tx_hash,
        "expected rejection",
    )
}

/// Default-on regression: with no explicit `with_invalid_tx_list_config`, the
/// `InvalidTxListConfig::default()` must still quarantine a tx that triggers
/// `UnprocessableTransactionError` on resubmission.
#[tokio::test]
async fn test_invalid_tx_list_default_on_quarantines_panicking_tx() -> Result<()> {
    use alloy_primitives::U256;

    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    node.send_tx(
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(NORMAL_TX_RECIPIENT)),
            ..Default::default()
        },
    )
    .await?;

    let panicking_tx_signed = node
        .sign_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(PANIC_PRECOMPILE_ADDRESS)),
                value: Some(U256::ZERO),
                gas: Some(100_000),
                ..Default::default()
            },
        )
        .await?;
    let panicking_tx_hash = *panicking_tx_signed.tx_hash();
    node.send_signed_tx(panicking_tx_signed.clone()).await?;

    let _ = node.produce_block().await;

    expect_invalid_tx_list_rejection(
        node.send_signed_tx(panicking_tx_signed).await,
        panicking_tx_hash,
        "expected rejection under default config",
    )
}

fn expect_invalid_tx_list_rejection(
    result: eyre::Result<TxHash>,
    tx_hash: TxHash,
    context: &str,
) -> Result<()> {
    match result {
        Ok(_) => Err(eyre::eyre!(
            "Transaction {tx_hash} accepted on resubmission, {context}"
        )),
        Err(err) => {
            let rpc_err = err.downcast_ref::<RpcClientError>().ok_or_else(|| {
                eyre::eyre!("Transaction {tx_hash} rejected outside JSON-RPC path: {err:?}")
            })?;

            let RpcClientError::Call(call_err) = rpc_err else {
                return Err(eyre::eyre!(
                    "Transaction {tx_hash} rejected with unexpected RPC error: {rpc_err:?}"
                ));
            };
            assert!(
                call_err
                    .message()
                    .to_lowercase()
                    .contains("invalid tx list"),
                "Expected invalid tx list rejection in RPC error, got: {call_err:?}"
            );
            Ok(())
        }
    }
}
