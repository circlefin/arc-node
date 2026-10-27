// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Pending RPC lookups.
//!
//! Default (filter on): sender+nonce hides pool-only answers. By-hash still
//! returns the pending object, then the mined tx after inclusion.
//! `--arc.expose-pending-txs` (filter off): sender+nonce returns the pool tx.

use alloy_primitives::{address, Address, TxHash, U256};
use alloy_rpc_types_eth::TransactionRequest;
use alloy_signer_local::PrivateKeySigner;
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;

/// Recipient with zero balance in genesis.
const RECIPIENT: Address = address!("0x000000000000000000000000000000000000bEEF");

async fn submit_pending_transfer(
    node: &mut ArcTestNode,
    signer: PrivateKeySigner,
) -> Result<(Address, u64, TxHash)> {
    let sender = signer.address();
    let nonce = node.nonce(sender, None).await?;
    let hash = node
        .send_tx(
            signer,
            TransactionRequest {
                from: Some(sender),
                to: Some(TxKind::Call(RECIPIENT)),
                value: Some(U256::from(1u64)),
                ..Default::default()
            },
        )
        .await?;
    Ok((sender, nonce, hash))
}

#[tokio::test]
async fn test_pending_tx_lookups() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let (sender, nonce, hash) = submit_pending_transfer(&mut node, signer).await?;

    let pending_by_nonce = node.transaction_by_sender_and_nonce(sender, nonce).await?;
    assert!(
        pending_by_nonce.is_none(),
        "pool-only sender+nonce lookup must return null on a pending-filtered node"
    );

    let pending_by_hash = node.transaction_by_hash(hash).await?.ok_or_else(|| {
        eyre::eyre!("eth_getTransactionByHash returned null for pending tx {hash}")
    })?;
    assert_eq!(
        *pending_by_hash.inner.tx_hash(),
        hash,
        "getTransactionByHash must return the pending transaction"
    );
    assert!(
        pending_by_hash.block_number.is_none(),
        "pending getTransactionByHash must have null blockNumber"
    );
    assert!(
        pending_by_hash.block_hash.is_none(),
        "pending getTransactionByHash must have null blockHash"
    );

    node.produce_block().await?;

    let mined_by_nonce = node
        .transaction_by_sender_and_nonce(sender, nonce)
        .await?
        .ok_or_else(|| eyre::eyre!("mined sender+nonce lookup returned null for nonce {nonce}"))?;
    let mined_by_hash = node
        .transaction_by_hash(hash)
        .await?
        .ok_or_else(|| eyre::eyre!("eth_getTransactionByHash returned null for mined tx {hash}"))?;

    assert_eq!(
        *mined_by_nonce.inner.tx_hash(),
        hash,
        "sender+nonce lookup must return the mined transaction"
    );
    assert_eq!(
        *mined_by_hash.inner.tx_hash(),
        hash,
        "getTransactionByHash must return the same mined transaction"
    );
    assert!(
        mined_by_nonce.block_number.is_some(),
        "mined sender+nonce result must include a block number"
    );
    assert!(
        mined_by_hash.block_number.is_some(),
        "mined getTransactionByHash result must include a block number"
    );
    assert_eq!(mined_by_nonce.block_number, mined_by_hash.block_number);
    assert_eq!(mined_by_nonce.block_hash, mined_by_hash.block_hash);

    Ok(())
}

#[tokio::test]
async fn test_sender_nonce_lookup_with_expose_pending_txs() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new().with_expose_pending_txs()).await?;
    let signer = node.wallet_signer(0)?;
    let (sender, nonce, hash) = submit_pending_transfer(&mut node, signer).await?;

    let pending_by_nonce = node
        .transaction_by_sender_and_nonce(sender, nonce)
        .await?
        .ok_or_else(|| {
            eyre::eyre!("pool-only sender+nonce lookup returned null with --arc.expose-pending-txs")
        })?;
    assert_eq!(
        *pending_by_nonce.inner.tx_hash(),
        hash,
        "exposed sender+nonce lookup must return the pool transaction"
    );
    assert!(
        pending_by_nonce.block_number.is_none(),
        "pool-only sender+nonce result must have null blockNumber"
    );
    assert!(
        pending_by_nonce.block_hash.is_none(),
        "pool-only sender+nonce result must have null blockHash"
    );

    Ok(())
}
