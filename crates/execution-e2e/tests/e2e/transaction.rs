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

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

//! Transaction sending e2e tests for Arc Chain.

use super::helpers::utils::{fee, send_and_mine};
use alloy_primitives::{address, bytes, Address, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;

/// Recipient with zero balance in genesis.
const RECIPIENT: Address = address!("0x000000000000000000000000000000000000bEEF");

/// Test sending multiple transactions in a single block.
#[tokio::test]
async fn test_multiple_transactions() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;

    let before = node.balance(signer.address(), None).await?;

    let tx1 = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(RECIPIENT)),
                value: Some(U256::from(1u64)),
                ..Default::default()
            },
        )
        .await?;
    let tx2 = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(RECIPIENT)),
                value: Some(U256::from(1u64)),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;

    let tx3 = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(RECIPIENT)),
                value: Some(U256::from(1u64)),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;

    let receipt1 = node.get_receipt(tx1).await?;
    let receipt2 = node.get_receipt(tx2).await?;
    let receipt3 = node.get_receipt(tx3).await?;

    assert!(receipt1.status());
    assert!(receipt2.status());
    assert!(receipt3.status());
    assert_eq!(receipt1.block_number, Some(1));
    assert_eq!(receipt2.block_number, Some(1));
    assert_eq!(receipt3.block_number, Some(2));

    let total_fee = fee(&receipt1) + fee(&receipt2) + fee(&receipt3);
    let after = node.balance(signer.address(), None).await?;
    assert_eq!(after, before - (U256::from(3u64) + total_fee));

    Ok(())
}

/// Test that a contract call that reverts is detected.
#[tokio::test]
async fn test_reverted_transaction() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let before = node.balance(signer.address(), None).await?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(address!(
                "0x3600000000000000000000000000000000000000"
            ))),
            value: Some(U256::ZERO), // Value must be 0: FiatTokenProxy is pre-blocklisted in NativeCoinControl.
            input: TransactionInput::new(bytes!("0x1234abcd")),
            gas: Some(100_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(!receipt.status());
    let after = node.balance(signer.address(), None).await?;
    assert_eq!(after, before - fee(&receipt));
    Ok(())
}
