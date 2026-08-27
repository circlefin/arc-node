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

//! EIP-7708 native transfer e2e tests.
//!
//! Tests that native value transfers emit ERC-20 Transfer logs from SYSTEM_ADDRESS
//! under the Zero5 hardfork via CALL to EOA, contract, and precompile recipients,
//! as well as CREATE, SELFDESTRUCT, and nested value transfer scenarios.

use super::helpers;

use super::helpers::{
    contracts::right_pad_address,
    eip7708::{assert_transfer_log, call_tracer_options},
    utils::{deploy_and_mine, send_and_mine},
};
use alloy_primitives::{address, TxHash, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_rpc_types_trace::geth::{GethDebugTracingOptions, GethDefaultTracingOptions, GethTrace};
use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_e2e::{chainspec::localdev_with_hardforks, ArcSetup, ArcTestNode, TxKind};
use eyre::Result;
use reth_chainspec::ForkCondition;

fn opcode_gas_trace_options() -> GethDebugTracingOptions {
    GethDebugTracingOptions {
        config: GethDefaultTracingOptions::default()
            .with_enable_memory(false)
            .disable_stack()
            .disable_storage(),
        ..Default::default()
    }
}

async fn assert_last_opcode_gas_cost(
    node: &ArcTestNode,
    hash: TxHash,
    opcode: &str,
    expected: u64,
) -> Result<()> {
    let trace = node
        .trace_transaction(hash, opcode_gas_trace_options())
        .await?;
    let GethTrace::Default(frame) = trace else {
        return Err(eyre::eyre!(
            "expected default struct-log trace for transaction {hash}"
        ));
    };
    let log = frame
        .struct_logs
        .iter()
        .rev()
        .find(|log| log.opcode() == opcode)
        .ok_or_else(|| eyre::eyre!("opcode {opcode} not found in trace for transaction {hash}"))?;

    assert_eq!(log.gas_cost, expected);
    Ok(())
}

// ===== CALL to EOA (#1-3) =====

/// Test #1: EOA sends nonzero USDC to another EOA — emits 1 EIP-7708 Transfer log.
#[tokio::test]
async fn test_call_eoa_with_value_emits_eip7708_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000bEEF");
    let value = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
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
    assert!(receipt.status());
    assert_eq!(receipt.gas_used, 21_000);
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, recipient, value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #2: EOA sends 0 value — no EIP-7708 log.
#[tokio::test]
async fn test_call_eoa_zero_value_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000bEEF");

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(recipient)),
            value: Some(U256::ZERO),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.gas_used, 21_000);
    assert_eq!(receipt.logs().len(), 0);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #3: EOA sends value to self — no EIP-7708 log (self-transfer is suppressed).
#[tokio::test]
async fn test_call_eoa_self_transfer_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(sender)),
            value: Some(value),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.gas_used, 21_000);
    assert_eq!(receipt.logs().len(), 0);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

// ===== CALL to Contract (#4-6) =====

/// Test #4: EOA sends value to a value-accepting contract — emits exact EIP-7708 Transfer log.
#[tokio::test]
async fn test_call_contract_with_value_emits_eip7708_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let transfer_value = U256::from(500_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::payable_contract_deploy_code(),
        U256::ZERO,
        100_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(contract)),
            value: Some(transfer_value),
            gas: Some(100_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, contract, transfer_value);
    let balance = node.balance(contract, None).await?;
    assert_eq!(balance, transfer_value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #5: EOA sends 0 value to a contract — no EIP-7708 log.
#[tokio::test]
async fn test_call_contract_zero_value_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::payable_contract_deploy_code(),
        U256::ZERO,
        100_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(contract)),
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

/// Test #6: EOA sends value to a reverting contract — tx reverts, no EIP-7708 log.
#[tokio::test]
async fn test_call_reverting_contract_with_value_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = U256::from(500_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
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
        signer,
        TransactionRequest {
            from: Some(sender),
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

// ===== CALL to Precompile (#7-8) =====

/// Test #7: CALL to precompile with value — reverts (unauthorized), logs rolled back.
#[tokio::test]
async fn test_call_precompile_with_value() -> Result<()> {
    reth_tracing::init_test_tracing();

    let precompile = address!("0x1800000000000000000000000000000000000000");
    let value = U256::from(1_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(precompile)),
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

/// Test #8: CALL to precompile with 0 value — no EIP-7708 log.
#[tokio::test]
async fn test_call_precompile_zero_value_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let precompile = address!("0x1800000000000000000000000000000000000000");

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(precompile)),
            value: Some(U256::ZERO),
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

// ===== CREATE (#9-10) =====

/// Test #9: CREATE with nonzero value — emits exact EIP-7708 Transfer log.
#[tokio::test]
async fn test_create_with_value_emits_eip7708_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let endowment = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (contract, receipt) = deploy_and_mine(
        &mut node,
        signer,
        helpers::contracts::payable_contract_deploy_code(),
        endowment,
        100_000,
    )
    .await?;
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, contract, endowment);
    let balance = node.balance(contract, None).await?;
    assert_eq!(balance, endowment);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #10: CREATE with zero value — no EIP-7708 Transfer log.
#[tokio::test]
async fn test_create_zero_value_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let (_, receipt) = deploy_and_mine(
        &mut node,
        signer,
        helpers::contracts::payable_contract_deploy_code(),
        U256::ZERO,
        100_000,
    )
    .await?;
    assert_eq!(receipt.logs().len(), 0);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test: CREATE with nonzero value where constructor reverts — tx reverts, no log, no balance leak.
#[tokio::test]
async fn test_create_revert_with_endowment_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let initcode = helpers::contracts::reverting_constructor_code();
    let endowment = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let would_be_addr = sender.create(0);
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Create),
            value: Some(endowment),
            gas: Some(100_000),
            input: TransactionInput::new(initcode),
            ..Default::default()
        },
    )
    .await?;
    assert!(!receipt.status());
    assert_eq!(receipt.logs().len(), 0);
    let balance = node.balance(would_be_addr, None).await?;
    assert_eq!(balance, U256::ZERO);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test: successful CREATE2 with value leaves the created address warm.
#[tokio::test]
async fn test_create2_with_value_balance_probe_is_warm_after_successful_create() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (probe, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::create2_with_balance_probe(),
        U256::from(1),
        200_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(probe)),
            gas: Some(200_000),
            input: TransactionInput::new(helpers::contracts::create2_balance_probe_calldata(probe)),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_last_opcode_gas_cost(&node, receipt.transaction_hash, "BALANCE", 100).await?;
    Ok(())
}

/// Test: out-of-funds CREATE2 with value does not warm the would-be created address.
#[tokio::test]
async fn test_create2_out_of_funds_keeps_balance_probe_cold() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (probe, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::create2_with_balance_probe(),
        U256::ZERO,
        200_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(probe)),
            gas: Some(200_000),
            input: TransactionInput::new(helpers::contracts::create2_balance_probe_calldata(probe)),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_last_opcode_gas_cost(&node, receipt.transaction_hash, "BALANCE", 2600).await?;
    Ok(())
}

// ===== SELFDESTRUCT (#11-18) =====

/// Test #11: SELFDESTRUCT sends balance to beneficiary — emits exact EIP-7708 Transfer log.
#[tokio::test]
async fn test_selfdestruct_with_balance_emits_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let endowment = U256::from(1_000_000);
    let beneficiary = address!("0x000000000000000000000000000000000000BEEF");

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::selfdestruct_contract_deploy_code(),
        endowment,
        200_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(contract)),
            gas: Some(200_000),
            input: TransactionInput::new(right_pad_address(beneficiary)),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, contract, beneficiary, endowment);
    let balance = node.balance(contract, None).await?;
    assert_eq!(balance, U256::ZERO);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test #12: SELFDESTRUCT with zero balance — no EIP-7708 Transfer log.
#[tokio::test]
async fn test_selfdestruct_zero_balance_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let beneficiary = address!("0x000000000000000000000000000000000000BEEF");

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::selfdestruct_contract_deploy_code(),
        U256::ZERO,
        200_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(contract)),
            gas: Some(200_000),
            input: TransactionInput::new(right_pad_address(beneficiary)),
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

/// Test #13: SELFDESTRUCT to self — beneficiary == contract address.
#[tokio::test]
async fn test_selfdestruct_to_self_reverts() -> Result<()> {
    reth_tracing::init_test_tracing();

    let endowment = U256::from(1_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::selfdestruct_contract_deploy_code(),
        endowment,
        200_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(contract)),
            gas: Some(200_000),
            input: TransactionInput::new(right_pad_address(contract)),
            ..Default::default()
        },
    )
    .await?;
    assert!(!receipt.status());
    assert_eq!(receipt.logs().len(), 0);
    let balance = node.balance(contract, None).await?;
    assert_eq!(balance, endowment);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test: before Zero8, SELFDESTRUCT target warmth only honors the legacy warm-address set.
#[tokio::test]
async fn test_selfdestruct_to_transaction_warm_target_before_zero8_reverts() -> Result<()> {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, ForkCondition::Block(0)),
        (ArcHardfork::Zero4, ForkCondition::Block(0)),
        (ArcHardfork::Zero5, ForkCondition::Block(0)),
        (ArcHardfork::Zero6, ForkCondition::Block(0)),
        (ArcHardfork::Zero7, ForkCondition::Block(0)),
        (ArcHardfork::Zero8, ForkCondition::Block(100)),
    ]);
    let endowment = U256::from(1);
    let selfdestruct_target = address!("0x000000000000000000000000000000000000BEEF");

    let mut node = ArcTestNode::start(ArcSetup::new().with_chain_spec(chain_spec)).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let fund_receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(selfdestruct_target)),
            value: Some(U256::from(1)),
            ..Default::default()
        },
    )
    .await?;
    assert!(fund_receipt.status());

    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::balance_warming_selfdestruct_contract_deploy_code(),
        endowment,
        200_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(contract)),
            value: Some(U256::ZERO),
            gas: Some(28_869),
            input: TransactionInput::new(right_pad_address(selfdestruct_target)),
            ..Default::default()
        },
    )
    .await?;

    assert!(!receipt.status());
    assert_eq!(receipt.logs().len(), 0);
    assert_eq!(
        node.balance(selfdestruct_target, None).await?,
        U256::from(1)
    );
    assert_eq!(node.balance(contract, None).await?, endowment);
    Ok(())
}

/// Test: SELFDESTRUCT to a target warmed earlier in the same transaction succeeds
/// even when the remaining gas after SELFDESTRUCT's static cost is below the cold
/// account load cost.
#[tokio::test]
async fn test_selfdestruct_to_transaction_warm_target_with_low_gas_succeeds() -> Result<()> {
    reth_tracing::init_test_tracing();

    let endowment = U256::from(1);
    let selfdestruct_target = address!("0x000000000000000000000000000000000000BEEF");

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let fund_receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(selfdestruct_target)),
            value: Some(U256::from(1)),
            ..Default::default()
        },
    )
    .await?;
    assert!(fund_receipt.status());

    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::balance_warming_selfdestruct_contract_deploy_code(),
        endowment,
        200_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(contract)),
            value: Some(U256::ZERO),
            gas: Some(28_869),
            input: TransactionInput::new(right_pad_address(selfdestruct_target)),
            ..Default::default()
        },
    )
    .await?;

    assert!(receipt.status());
    assert_last_opcode_gas_cost(&node, receipt.transaction_hash, "SELFDESTRUCT", 5000).await?;
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, contract, selfdestruct_target, endowment);
    assert_eq!(
        node.balance(selfdestruct_target, None).await?,
        U256::from(2)
    );
    assert_eq!(node.balance(contract, None).await?, U256::ZERO);
    Ok(())
}

// ===== Nested/Forwarded Transfer (#19) =====

/// Test #19: Contract forwards received value to another address — both transfers emit exact logs.
#[tokio::test]
async fn test_nested_value_transfer_emits_multiple_logs() -> Result<()> {
    reth_tracing::init_test_tracing();

    let final_recipient = address!("0x000000000000000000000000000000000000CAFE");
    let value = U256::from(500_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (forwarder, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::forwarder_contract_deploy_code(),
        U256::ZERO,
        200_000,
    )
    .await?;
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(forwarder)),
            value: Some(value),
            gas: Some(200_000),
            input: TransactionInput::new(right_pad_address(final_recipient)),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 2);
    assert_transfer_log(&receipt, 0, sender, forwarder, value);
    assert_transfer_log(&receipt, 1, forwarder, final_recipient, value);
    let balance = node.balance(forwarder, None).await?;
    assert_eq!(balance, U256::ZERO);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

// ===== Additional coverage =====

/// Test: large value transfer emits correct log.
#[tokio::test]
async fn test_large_value_transfer() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000001234");
    let value = U256::from(10_000_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
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
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, recipient, value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test: minimum value (1 wei) transfer emits correct log.
#[tokio::test]
async fn test_min_value_transfer() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000005678");
    let value = U256::from(1);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let receipt = send_and_mine(
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
    assert!(receipt.status());
    assert_eq!(receipt.logs().len(), 1);
    assert_transfer_log(&receipt, 0, sender, recipient, value);
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test: multiple value transfers in one block each emit their own log.
#[tokio::test]
async fn test_multiple_transfers_in_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    let recipient_a = address!("0x000000000000000000000000000000000000aaaa");
    let recipient_b = address!("0x000000000000000000000000000000000000bbbb");
    let value_a = U256::from(100_000);
    let value_b = U256::from(200_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let tx1 = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(recipient_a)),
                value: Some(value_a),
                ..Default::default()
            },
        )
        .await?;
    let tx2 = node
        .send_tx(
            signer,
            TransactionRequest {
                from: Some(sender),
                to: Some(TxKind::Call(recipient_b)),
                value: Some(value_b),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;

    let receipt1 = node.get_receipt(tx1).await?;
    assert!(receipt1.status());
    assert_eq!(receipt1.gas_used, 21_000);
    assert_eq!(receipt1.logs().len(), 1);
    assert_transfer_log(&receipt1, 0, sender, recipient_a, value_a);

    let receipt2 = node.get_receipt(tx2).await?;
    assert!(receipt2.status());
    assert_eq!(receipt2.gas_used, 21_000);
    assert_eq!(receipt2.logs().len(), 1);
    assert_transfer_log(&receipt2, 0, sender, recipient_b, value_b);
    node.trace_transaction(tx1, call_tracer_options()).await?;
    node.trace_transaction(tx2, call_tracer_options()).await?;
    Ok(())
}

/// Test: reverted value transfer to reverting contract does not leak balance.
#[tokio::test]
async fn test_reverted_value_transfer_balance_unchanged() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = U256::from(500_000);

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let sender = signer.address();
    let (contract, _) = deploy_and_mine(
        &mut node,
        signer.clone(),
        helpers::contracts::reverting_contract_deploy_code(),
        U256::ZERO,
        100_000,
    )
    .await?;
    let starting_balance = node.balance(contract, None).await?;
    assert_eq!(starting_balance, U256::ZERO);
    let receipt = send_and_mine(
        &mut node,
        signer,
        TransactionRequest {
            from: Some(sender),
            to: Some(TxKind::Call(contract)),
            value: Some(value),
            gas: Some(100_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(!receipt.status());
    assert_eq!(receipt.logs().len(), 0);
    let ending_balance = node.balance(contract, None).await?;
    assert_eq!(ending_balance, U256::ZERO);
    Ok(())
}
