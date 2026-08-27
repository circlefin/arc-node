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

//! EIP-7708 zero address e2e tests.
//!
//! Arc custom behavior: value transfers to Address::ZERO are rejected under Zero5.

use super::helpers::{eip7708::call_tracer_options, utils::send_and_mine};
use alloy_primitives::{Address, U256};
use alloy_rpc_types_eth::TransactionRequest;
use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_e2e::{chainspec::localdev_with_hardforks, ArcSetup, ArcTestNode, TxKind};
use eyre::Result;
use reth_chainspec::ForkCondition;

/// Test #24: Send value to Address::ZERO under Zero5 — tx reverts.
#[tokio::test]
async fn test_zero_address_value_transfer_reverts() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(Address::ZERO)),
            value: Some(value),
            gas: Some(100_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(!receipt.status());
    assert_eq!(receipt.logs().len(), 0);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #25: Send zero value to Address::ZERO — should succeed (no transfer, no log).
#[tokio::test]
async fn test_zero_address_zero_value_succeeds() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(Address::ZERO)),
            value: Some(U256::ZERO),
            gas: Some(100_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 0);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #26: Value transfer to Address::ZERO is rejected before Zero5 metadata activation.
#[tokio::test]
async fn test_zero_address_rejected_before_zero5_metadata_activation() -> Result<()> {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, ForkCondition::Block(0)),
        (ArcHardfork::Zero4, ForkCondition::Block(0)),
        (ArcHardfork::Zero5, ForkCondition::Block(100)),
        (ArcHardfork::Zero6, ForkCondition::Block(100)),
    ]);

    let value = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new().with_chain_spec(chain_spec)).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(Address::ZERO)),
            value: Some(value),
            gas: Some(100_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(!receipt.status());
    assert_eq!(receipt.logs().len(), 0);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}
