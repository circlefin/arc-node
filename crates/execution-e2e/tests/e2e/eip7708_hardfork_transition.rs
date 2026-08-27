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

//! EIP-7708 baseline e2e tests.
//!
//! Verifies that EIP-7708 Transfer logs are emitted even when chain metadata
//! schedules Zero5 later.

use super::helpers::{
    eip7708::{assert_transfer_log, call_tracer_options},
    utils::send_and_mine,
};
use alloy_primitives::{address, U256};
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionRequest};
use arc_execution_config::hardforks::{is_arc_fork_active, ArcHardfork};
use arc_execution_e2e::{chainspec::localdev_with_hardforks, ArcSetup, ArcTestNode, TxKind};
use eyre::Result;
use reth_chainspec::{ChainSpecProvider, ForkCondition};

/// Test #20: Value transfer emits EIP-7708 Transfer before Zero5 metadata activation.
#[tokio::test]
async fn test_baseline_emits_eip7708_before_zero5_metadata_activation() -> Result<()> {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, ForkCondition::Block(0)),
        (ArcHardfork::Zero4, ForkCondition::Block(0)),
        (ArcHardfork::Zero5, ForkCondition::Block(100)), // far in the future
        (ArcHardfork::Zero6, ForkCondition::Block(100)),
    ]);

    let recipient = address!("0x000000000000000000000000000000000000bEEF");
    let value = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new().with_chain_spec(chain_spec)).await?;
    let chain_spec = node.node.inner.provider().chain_spec();
    let current = node.get_block(BlockNumberOrTag::Latest).await?;
    assert!(!is_arc_fork_active(
        chain_spec.as_ref(),
        ArcHardfork::Zero5,
        current.header.number,
        current.header.timestamp
    ));
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(recipient)),
            value: Some(value),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, recipient, value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #21: Zero5 activation metadata does not change the EIP-7708 baseline.
#[tokio::test]
async fn test_zero5_activation_metadata_keeps_eip7708_baseline() -> Result<()> {
    reth_tracing::init_test_tracing();

    // Zero5 activates at block 3
    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, ForkCondition::Block(0)),
        (ArcHardfork::Zero4, ForkCondition::Block(0)),
        (ArcHardfork::Zero5, ForkCondition::Block(3)),
        (ArcHardfork::Zero6, ForkCondition::Block(100)),
    ]);

    let recipient = address!("0x000000000000000000000000000000000000bEEF");
    let value = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new().with_chain_spec(chain_spec)).await?;
    let chain_spec = node.node.inner.provider().chain_spec();
    let current = node.get_block(BlockNumberOrTag::Latest).await?;
    assert!(!is_arc_fork_active(
        chain_spec.as_ref(),
        ArcHardfork::Zero5,
        current.header.number,
        current.header.timestamp
    ));

    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let pre_zero5_receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(recipient)),
            value: Some(value),
            ..Default::default()
        },
    )
    .await?;
    let current = node.get_block(BlockNumberOrTag::Latest).await?;
    assert_eq!(current.header.number, 1);
    assert!(pre_zero5_receipt.status());
    assert_eq!(pre_zero5_receipt.logs().len(), 1);
    assert_transfer_log(&pre_zero5_receipt, 0, sender, recipient, value);

    node.produce_blocks(2).await?;
    let current = node.get_block(BlockNumberOrTag::Latest).await?;
    assert_eq!(current.header.number, 3);
    assert!(is_arc_fork_active(
        chain_spec.as_ref(),
        ArcHardfork::Zero5,
        current.header.number,
        current.header.timestamp
    ));

    let post_zero5_receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(recipient)),
            value: Some(value),
            ..Default::default()
        },
    )
    .await?;
    assert!(post_zero5_receipt.status());
    assert_eq!(post_zero5_receipt.logs().len(), 1);
    assert_transfer_log(&post_zero5_receipt, 0, sender, recipient, value);
    node.trace_transaction(pre_zero5_receipt.transaction_hash, call_tracer_options())
        .await?;
    node.trace_transaction(post_zero5_receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #22: Post-Zero5 value transfer emits Transfer from SYSTEM_ADDRESS.
#[tokio::test]
async fn test_post_zero5_emits_eip7708_transfer() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000bEEF");
    let value = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let chain_spec = node.node.inner.provider().chain_spec();
    let current = node.get_block(BlockNumberOrTag::Latest).await?;
    assert!(is_arc_fork_active(
        chain_spec.as_ref(),
        ArcHardfork::Zero5,
        current.header.number,
        current.header.timestamp
    ));
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(recipient)),
            value: Some(value),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, recipient, value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #23: Verify Zero5 hardfork is active at genesis on default localdev.
#[tokio::test]
async fn test_zero5_active_at_genesis() -> Result<()> {
    reth_tracing::init_test_tracing();

    let node = ArcTestNode::start(ArcSetup::new()).await?;
    let chain_spec = node.node.inner.provider().chain_spec();
    let current = node.get_block(BlockNumberOrTag::Latest).await?;
    assert!(is_arc_fork_active(
        chain_spec.as_ref(),
        ArcHardfork::Zero5,
        current.header.number,
        current.header.timestamp
    ));
    Ok(())
}
