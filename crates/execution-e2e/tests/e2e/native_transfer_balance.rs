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

//! E2E tests verifying native value transfer balance changes.

use super::helpers::utils::{fee, send_and_mine};
use alloy_primitives::{address, Address, U256};
use alloy_rpc_types_eth::TransactionRequest;
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;

/// Recipient with zero balance in genesis.
const RECIPIENT: Address = address!("0x000000000000000000000000000000000000bEEF");

/// Transfer value used in tests: 100 USDC (100e18 wei).
fn transfer_value() -> U256 {
    U256::from(100u64) * U256::from(10u64).pow(U256::from(18u64))
}

/// Recipient balance goes from 0 to the transferred value.
#[tokio::test]
async fn test_value_transfer_credits_recipient() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = transfer_value();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender_before = node.balance(signer.address(), None).await?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(RECIPIENT)),
            value: Some(value),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    let balance = node.balance(RECIPIENT, None).await?;
    assert_eq!(balance, value);
    let sender_after = node.balance(signer.address(), None).await?;
    assert_eq!(sender_after, sender_before - value - fee(&receipt));
    Ok(())
}

/// Sender balance decreases by at least the transferred value (plus gas).
#[tokio::test]
async fn test_value_transfer_debits_sender() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = transfer_value();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let before = node.balance(signer.address(), None).await?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(RECIPIENT)),
            value: Some(value),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    let after = node.balance(signer.address(), None).await?;
    assert_eq!(after, before - value - fee(&receipt));
    Ok(())
}

/// Zero-value transfer leaves recipient balance unchanged.
#[tokio::test]
async fn test_zero_value_transfer_no_balance_change() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let before = node.balance(RECIPIENT, None).await?;
    let sender_before = node.balance(signer.address(), None).await?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(RECIPIENT)),
            value: Some(U256::ZERO),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    let after = node.balance(RECIPIENT, None).await?;
    assert_eq!(after, before);
    let sender_after = node.balance(signer.address(), None).await?;
    assert_eq!(sender_after, sender_before - fee(&receipt));
    Ok(())
}

/// Two transfers to the same recipient accumulate.
#[tokio::test]
async fn test_multiple_transfers_accumulate() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = transfer_value();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender_before = node.balance(signer.address(), None).await?;
    let tx1 = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(RECIPIENT)),
                value: Some(value),
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
                value: Some(value),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let receipt1 = node.get_receipt(tx1).await?;
    let receipt2 = node.get_receipt(tx2).await?;
    assert!(receipt1.status());
    assert!(receipt2.status());
    let balance = node.balance(RECIPIENT, None).await?;
    assert_eq!(balance, value + value);
    let sender_after = node.balance(signer.address(), None).await?;
    let total_fee = fee(&receipt1) + fee(&receipt2);
    assert_eq!(sender_after, sender_before - (value + value + total_fee));
    Ok(())
}
