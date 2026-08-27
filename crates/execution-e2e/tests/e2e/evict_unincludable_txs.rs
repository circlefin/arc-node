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

use alloy_primitives::{Address, U256};
use alloy_rpc_types_eth::TransactionRequest;
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;
use reth_transaction_pool::TransactionPool;

/// The build loop must NOT evict a merely temporarily-invalid tx.
///
/// A second tx from the same sender becomes unaffordable once the first tx drains
/// the balance. The build loop hits a non-blocklist `InvalidTransaction`
/// (`LackOfFundForMaxFee`), which is skip-only — the tx must stay in the pool
/// across several builds and must NOT land in the invalid tx list.
#[tokio::test]
async fn test_temporarily_invalid_tx_not_evicted() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();

    // Wallet 0 starts with ~1e24 wei. Drain almost all of it in `drain`, so the
    // follow-up `stuck` tx (nonce+1) cannot cover its value + fees afterwards.
    let nearly_all =
        U256::from(10u64).pow(U256::from(24u64)) - U256::from(10u64).pow(U256::from(20u64));

    let drain_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(sender),
                to: Some(TxKind::Call(Address::random())),
                value: Some(nearly_all),
                ..Default::default()
            },
        )
        .await?;

    let stuck_hash = node
        .send_tx(
            signer,
            TransactionRequest {
                from: Some(sender),
                to: Some(TxKind::Call(Address::random())),
                value: Some(nearly_all),
                ..Default::default()
            },
        )
        .await?;

    // First block includes `drain`, leaving `stuck` unaffordable.
    node.produce_block().await?;
    let drain_receipt = node.get_receipt(drain_hash).await?;
    assert!(drain_receipt.status());

    // Produce several more blocks; `stuck` must remain in the pool throughout
    // (kept for retry, not added to the invalid tx list).
    for _ in 0..4 {
        node.produce_block().await?;
        if !node.node.inner.pool.contains(&stuck_hash) {
            return Err(eyre::eyre!(
                "temporarily-invalid tx {stuck_hash} was evicted from the pool; \
                 it should be kept for retry"
            ));
        }
    }

    Ok(())
}
