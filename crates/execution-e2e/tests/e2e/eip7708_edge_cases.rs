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

//! EIP-7708 edge case e2e tests.
//!
//! Tests revert rollback, multi-log composition, inner-call semantics,
//! and unusual transfer patterns.

use super::helpers;

use super::helpers::{
    contracts::right_pad_address,
    eip7708::{assert_transfer_log, call_tracer_options, NATIVE_COIN_AUTHORITY_ADDRESS},
    utils::{deploy_and_mine, send_and_mine},
};
use alloy_primitives::{address, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;
use rstest::rstest;

/// Test #48: Send value to a reverting contract — tx reverts, no EIP-7708 log.
///
/// When the entire CALL frame reverts, the EIP-7708 log is rolled back.
/// Deploys an actual reverting contract rather than using an existing address.
#[tokio::test]
async fn test_reverted_call_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let value = U256::from(1_000_000);
    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::reverting_contract_deploy_code(),
        U256::ZERO,
        100_000,
    )
    .await?;

    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(contract)),
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

/// Tests #49/#50: Inner CALL reverts but outer succeeds — outer log emitted, inner log rolled back.
///
/// Deploys a reverting contract and an outer contract that forwards value to it.
/// The outer contract accepts value (emitting sender→outer log), then makes an
/// inner CALL with value to the reverting contract. The inner frame reverts, so
/// the inner value transfer log (outer→reverting) is rolled back. Only the
/// outer log remains. Parameterized over transfer amount to verify consistency.
#[rstest]
#[case::standard_value(U256::from(1_000_000))]
#[case::smaller_value(U256::from(500_000))]
#[tokio::test]
async fn test_inner_call_reverts_outer_succeeds(#[case] value: U256) -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (reverting_contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::reverting_contract_deploy_code(),
        U256::ZERO,
        200_000,
    )
    .await?;
    let (outer_contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::call_target_with_value_contract_deploy_code(),
        U256::ZERO,
        200_000,
    )
    .await?;

    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(outer_contract)),
            value: Some(value),
            gas: Some(200_000),
            input: TransactionInput::new(right_pad_address(reverting_contract)),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, outer_contract, value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #51: Multiple sequential value transfers in separate blocks.
///
/// Each block contains a value transfer, verifying logs are emitted consistently
/// across blocks and don't leak between transactions.
#[tokio::test]
async fn test_sequential_blocks_each_emit_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient_1 = address!("0x000000000000000000000000000000000000AAA1");
    let recipient_2 = address!("0x000000000000000000000000000000000000AAA2");
    let value_1 = U256::from(100_000);
    let value_2 = U256::from(200_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt_1 = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(recipient_1)),
            value: Some(value_1),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt_1.status());
    assert_eq!(receipt_1.logs().len(), 1);
    assert_transfer_log(&receipt_1, 0, sender, recipient_1, value_1);
    node.trace_transaction(receipt_1.transaction_hash, call_tracer_options())
        .await?;

    let receipt_2 = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(recipient_2)),
            value: Some(value_2),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt_2.status());
    assert_eq!(receipt_2.logs().len(), 1);
    assert_transfer_log(&receipt_2, 0, sender, recipient_2, value_2);
    node.trace_transaction(receipt_2.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #52: Contract calls NativeCoinAuthority precompile with value.
///
/// Deploys a contract that forwards a CALL with value to the NativeCoinAuthority
/// precompile address. The precompile will revert (unauthorized caller), but
/// the outer frame succeeds. The outer value transfer log (sender→contract)
/// is preserved; the inner log (contract→precompile) is rolled back because
/// the precompile rejects the call.
#[tokio::test]
async fn test_contract_calls_precompile_with_value() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let value = U256::from(500_000);
    let calldata = right_pad_address(NATIVE_COIN_AUTHORITY_ADDRESS);
    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::call_target_with_value_contract_deploy_code(),
        U256::ZERO,
        200_000,
    )
    .await?;

    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(contract)),
            value: Some(value),
            gas: Some(200_000),
            input: TransactionInput::new(calldata),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, contract, value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #53: Value transfer after producing multiple empty blocks.
///
/// Verifies that EIP-7708 log emission works correctly even when
/// there are empty blocks between genesis and the transfer.
#[tokio::test]
async fn test_log_after_empty_blocks() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000CC0001");
    let value = U256::from(500_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    node.produce_blocks(5).await?;
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

/// Test: Transfer to a contract that exists but has no code (EOA-like).
#[tokio::test]
async fn test_transfer_to_codeless_address() -> Result<()> {
    reth_tracing::init_test_tracing();

    let target = address!("0x000000000000000000000000000000000000DEAD");
    let value = U256::from(1_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(target)),
            value: Some(value),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, target, value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test: Value transfer and zero-value transfer in same block.
///
/// Only the value transfer should emit a log; the zero-value transfer should not.
#[tokio::test]
async fn test_mixed_value_and_zero_value_in_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000F00D");
    let value = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let with_value = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(recipient)),
                value: Some(value),
                ..Default::default()
            },
        )
        .await?;
    let zero_value = node
        .send_tx(
            signer,
            TransactionRequest {
                from: Some(sender),
                to: Some(TxKind::Call(recipient)),
                value: Some(U256::ZERO),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;

    let with_value_receipt = node.get_receipt(with_value).await?;
    assert!(with_value_receipt.status());
    assert_eq!(with_value_receipt.logs().len(), 1);
    assert_transfer_log(&with_value_receipt, 0, sender, recipient, value);

    let zero_value_receipt = node.get_receipt(zero_value).await?;
    assert!(zero_value_receipt.status());
    assert_eq!(zero_value_receipt.logs().len(), 0);
    Ok(())
}
