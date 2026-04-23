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

mod helpers;

use alloy_primitives::{address, U256};
use arc_execution_e2e::{
    actions::{
        AssertBalance, AssertNamedBalance, AssertTransferEvent, AssertTxIncluded, AssertTxLogs,
        AssertTxTrace, ProduceBlocks, SendTransaction, StoreDeployedAddress, TxStatus,
    },
    ArcSetup, ArcTestBuilder,
};
use helpers::{
    constants::{SYSTEM_ADDRESS, WALLET_FIRST_ADDRESS},
    contracts::right_pad_address,
};

// ===== CALL to EOA (#1-3) =====

/// Test #1: EOA sends nonzero USDC to another EOA — emits 1 EIP-7708 Transfer log.
#[tokio::test]
async fn test_call_eoa_with_value_emits_eip7708_log() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000bEEF");
    let value = U256::from(1_000_000); // 1 USDC (6 decimals)

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("transfer")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_call_eoa_with_value_emits_eip7708_log failed");
}

/// Test #2: EOA sends 0 value — no EIP-7708 log.
#[tokio::test]
async fn test_call_eoa_zero_value_no_log() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000bEEF");

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer")
                .with_to(recipient)
                .with_value(U256::ZERO),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("transfer").expect_no_logs())
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_call_eoa_zero_value_no_log failed");
}

/// Test #3: EOA sends value to self — no EIP-7708 log (self-transfer is suppressed).
#[tokio::test]
async fn test_call_eoa_self_transfer_no_log() {
    reth_tracing::init_test_tracing();

    let value = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer")
                .with_to(WALLET_FIRST_ADDRESS)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("transfer").expect_no_logs())
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_call_eoa_self_transfer_no_log failed");
}

// ===== CALL to Contract (#4-6) =====

/// Test #4: EOA sends value to a payable contract — emits exact EIP-7708 Transfer log.
///
/// Deploys a payable contract via CREATE, then sends value to it.
/// Asserts exact from (sender), to (deployed contract), and value using stored addresses.
#[tokio::test]
async fn test_call_contract_with_value_emits_eip7708_log() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::payable_contract_deploy_code();
    let transfer_value = U256::from(500_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Deploy payable contract
        .with_action(
            SendTransaction::new("deploy")
                .with_create()
                .with_data(deploy_code)
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy"))
        // Send value to the deployed contract
        .with_action(
            SendTransaction::new("value_call")
                .with_to_named("deploy_address")
                .with_value(transfer_value)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("value_call").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("value_call").expect_log_count(1))
        .with_action(AssertTransferEvent::new(
            "value_call",
            0,
            WALLET_FIRST_ADDRESS,
            AssertTransferEvent::named("deploy_address"),
            transfer_value,
        ))
        .with_action(AssertNamedBalance::of("deploy_address").equals(transfer_value))
        .with_action(AssertTxTrace::new("value_call"))
        .run()
        .await
        .expect("test_call_contract_with_value_emits_eip7708_log failed");
}

/// Test #5: EOA sends 0 value to a contract — no EIP-7708 log.
#[tokio::test]
async fn test_call_contract_zero_value_no_log() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::payable_contract_deploy_code();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("deploy")
                .with_create()
                .with_data(deploy_code)
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy"))
        .with_action(
            SendTransaction::new("zero_call")
                .with_to_named("deploy_address")
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("zero_call").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("zero_call").expect_no_logs())
        .with_action(AssertTxTrace::new("zero_call"))
        .run()
        .await
        .expect("test_call_contract_zero_value_no_log failed");
}

/// Test #6: EOA sends value to a reverting contract — tx reverts, no EIP-7708 log.
#[tokio::test]
async fn test_call_reverting_contract_with_value_no_log() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::reverting_contract_deploy_code();
    let value = U256::from(500_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("deploy")
                .with_create()
                .with_data(deploy_code)
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy"))
        .with_action(
            SendTransaction::new("revert_call")
                .with_to_named("deploy_address")
                .with_value(value)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("revert_call").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("revert_call").expect_no_logs())
        .with_action(AssertTxTrace::new("revert_call"))
        .run()
        .await
        .expect("test_call_reverting_contract_with_value_no_log failed");
}

// ===== CALL to Precompile (#7-8) =====

/// Test #7: CALL to precompile with value — reverts (unauthorized), logs rolled back.
#[tokio::test]
async fn test_call_precompile_with_value() {
    reth_tracing::init_test_tracing();

    let precompile = address!("0x1800000000000000000000000000000000000000");
    let value = U256::from(1_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("call_precompile")
                .with_to(precompile)
                .with_value(value)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("call_precompile").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("call_precompile").expect_no_logs())
        .with_action(AssertTxTrace::new("call_precompile"))
        .run()
        .await
        .expect("test_call_precompile_with_value failed");
}

/// Test #8: CALL to precompile with 0 value — no EIP-7708 log.
#[tokio::test]
async fn test_call_precompile_zero_value_no_log() {
    reth_tracing::init_test_tracing();

    let precompile = address!("0x1800000000000000000000000000000000000000");

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("call_precompile")
                .with_to(precompile)
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("call_precompile").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("call_precompile").expect_no_logs())
        .with_action(AssertTxTrace::new("call_precompile"))
        .run()
        .await
        .expect("test_call_precompile_zero_value_no_log failed");
}

// ===== CREATE (#9-10) =====

/// Test #9: CREATE with nonzero value — emits exact EIP-7708 Transfer log.
///
/// When deploying a contract with value (endowment), the value transfer from
/// the deployer to the new contract address emits an EIP-7708 Transfer log.
/// Asserts exact from (sender), to (deployed address from receipt), and value.
#[tokio::test]
async fn test_create_with_value_emits_eip7708_log() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::payable_contract_deploy_code();
    let endowment = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("create")
                .with_create()
                .with_data(deploy_code)
                .with_value(endowment)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("create").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("create"))
        .with_action(AssertTxLogs::new("create").expect_log_count(1))
        .with_action(AssertTransferEvent::new(
            "create",
            0,
            WALLET_FIRST_ADDRESS,
            AssertTransferEvent::named("create_address"),
            endowment,
        ))
        .with_action(AssertNamedBalance::of("create_address").equals(endowment))
        .with_action(AssertTxTrace::new("create"))
        .run()
        .await
        .expect("test_create_with_value_emits_eip7708_log failed");
}

/// Test #10: CREATE with zero value — no EIP-7708 Transfer log.
#[tokio::test]
async fn test_create_zero_value_no_log() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::payable_contract_deploy_code();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("create")
                .with_create()
                .with_data(deploy_code)
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("create").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("create").expect_no_logs())
        .with_action(AssertTxTrace::new("create"))
        .run()
        .await
        .expect("test_create_zero_value_no_log failed");
}

/// Test: CREATE with nonzero value where constructor reverts — tx reverts, no log, no balance leak.
///
/// Sends a CREATE tx with endowment but the constructor reverts.
/// The EIP-7708 Transfer log is rolled back with the frame.
/// The would-be contract address must not have any balance.
#[tokio::test]
async fn test_create_revert_with_endowment_no_log() {
    reth_tracing::init_test_tracing();

    let initcode = helpers::contracts::reverting_constructor_code();
    let endowment = U256::from(1_000_000);

    // Nonce-derived address is necessary here because the CREATE reverts —
    // StoreDeployedAddress cannot recover the address from a failed CREATE.
    // We compute it to verify no balance leaked to the would-be address.
    let would_be_addr = WALLET_FIRST_ADDRESS.create(0);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("create")
                .with_create()
                .with_data(initcode)
                .with_value(endowment)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        // Constructor reverts → tx reverts
        .with_action(AssertTxIncluded::new("create").expect(TxStatus::Reverted))
        // Transfer log rolled back
        .with_action(AssertTxLogs::new("create").expect_no_logs())
        // No balance leakage to the would-be contract address
        .with_action(AssertBalance::new(would_be_addr, U256::ZERO))
        .with_action(AssertTxTrace::new("create"))
        .run()
        .await
        .expect("test_create_revert_with_endowment_no_log failed");
}

// ===== SELFDESTRUCT (#11-18) =====

/// Test #11: SELFDESTRUCT sends balance to beneficiary — emits exact EIP-7708 Transfer log.
///
/// Asserts: from = contract address (stored), to = beneficiary, value = endowment.
#[tokio::test]
async fn test_selfdestruct_with_balance_emits_log() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::selfdestruct_contract_deploy_code();
    let endowment = U256::from(1_000_000);
    let beneficiary = address!("0x000000000000000000000000000000000000BEEF");

    let calldata = helpers::contracts::right_pad_address(beneficiary);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Deploy selfdestruct contract with endowment
        .with_action(
            SendTransaction::new("deploy")
                .with_create()
                .with_data(deploy_code)
                .with_value(endowment)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy"))
        // Trigger selfdestruct — sends balance to beneficiary
        .with_action(
            SendTransaction::new("selfdestruct")
                .with_to_named("deploy_address")
                .with_value(U256::ZERO)
                .with_data(calldata)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("selfdestruct").expect(TxStatus::Success))
        // Exact Transfer: from=stored contract addr, to=beneficiary, value=endowment
        .with_action(AssertTxLogs::new("selfdestruct").expect_log_count(1))
        .with_action(AssertTransferEvent::new(
            "selfdestruct",
            0,
            AssertTransferEvent::named("deploy_address"),
            beneficiary,
            endowment,
        ))
        .with_action(AssertNamedBalance::of("deploy_address").equals(U256::ZERO))
        .with_action(AssertTxTrace::new("selfdestruct"))
        .run()
        .await
        .expect("test_selfdestruct_with_balance_emits_log failed");
}

/// Test #12: SELFDESTRUCT with zero balance — no EIP-7708 Transfer log.
#[tokio::test]
async fn test_selfdestruct_zero_balance_no_log() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::selfdestruct_contract_deploy_code();
    let beneficiary = address!("0x000000000000000000000000000000000000BEEF");

    let calldata = helpers::contracts::right_pad_address(beneficiary);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Deploy with zero balance
        .with_action(
            SendTransaction::new("deploy")
                .with_create()
                .with_data(deploy_code)
                .with_value(U256::ZERO)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy"))
        // Trigger selfdestruct — zero balance to transfer
        .with_action(
            SendTransaction::new("selfdestruct")
                .with_to_named("deploy_address")
                .with_value(U256::ZERO)
                .with_data(calldata)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("selfdestruct").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("selfdestruct").expect_no_logs())
        .with_action(AssertTxTrace::new("selfdestruct"))
        .run()
        .await
        .expect("test_selfdestruct_zero_balance_no_log failed");
}

/// Test #13: SELFDESTRUCT to self — beneficiary == contract address.
///
/// The implementation explicitly rejects SELFDESTRUCT where source == target
/// with nonzero balance under Zero5 (see `check_selfdestruct_accounts` in opcode.rs).
/// The SELFDESTRUCT opcode halts with Revert, causing the tx to revert.
/// No log is emitted and the contract retains its balance.
#[tokio::test]
async fn test_selfdestruct_to_self_reverts() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::selfdestruct_contract_deploy_code();
    let endowment = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("deploy")
                .with_create()
                .with_data(deploy_code)
                .with_value(endowment)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy"))
        // Trigger selfdestruct to self — implementation rejects this
        .with_action(
            SendTransaction::new("selfdestruct")
                .with_to_named("deploy_address")
                .with_value(U256::ZERO)
                .with_data_fn(|env| {
                    let addr = env
                        .get_address("deploy_address")
                        .ok_or_else(|| eyre::eyre!("Named address 'deploy_address' not found"))?;
                    Ok(right_pad_address(*addr))
                })
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        // SELFDESTRUCT to self reverts the tx under Zero5
        .with_action(AssertTxIncluded::new("selfdestruct").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("selfdestruct").expect_no_logs())
        // Contract retains its balance — no state change
        .with_action(AssertNamedBalance::of("deploy_address").equals(endowment))
        .with_action(AssertTxTrace::new("selfdestruct"))
        .run()
        .await
        .expect("test_selfdestruct_to_self_reverts failed");
}

// ===== Nested/Forwarded Transfer (#19) =====

/// Test #19: Contract forwards received value to another address — both transfers emit exact logs.
///
/// Deploys a forwarder contract, then sends value to it with a target address.
/// The forwarder CALLs the target with the received value (CALLVALUE).
/// Expected: 2 EIP-7708 Transfer logs in order:
///   log[0]: Transfer(sender, forwarder, value)
///   log[1]: Transfer(forwarder, final_recipient, value)
#[tokio::test]
async fn test_nested_value_transfer_emits_multiple_logs() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::forwarder_contract_deploy_code();
    let final_recipient = address!("0x000000000000000000000000000000000000CAFE");
    let value = U256::from(500_000);

    let calldata = helpers::contracts::right_pad_address(final_recipient);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Deploy forwarder
        .with_action(
            SendTransaction::new("deploy")
                .with_create()
                .with_data(deploy_code)
                .with_value(U256::ZERO)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy"))
        // Send value to forwarder with target in calldata
        .with_action(
            SendTransaction::new("forward")
                .with_to_named("deploy_address")
                .with_value(value)
                .with_data(calldata)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("forward").expect(TxStatus::Success))
        // Exact 2 logs: sender→forwarder, forwarder→final_recipient
        .with_action(AssertTxLogs::new("forward").expect_log_count(2))
        .with_action(AssertTransferEvent::new(
            "forward",
            0,
            WALLET_FIRST_ADDRESS,
            AssertTransferEvent::named("deploy_address"),
            value,
        ))
        .with_action(AssertTransferEvent::new(
            "forward",
            1,
            AssertTransferEvent::named("deploy_address"),
            final_recipient,
            value,
        ))
        .with_action(AssertNamedBalance::of("deploy_address").equals(U256::ZERO))
        .with_action(AssertTxTrace::new("forward"))
        .run()
        .await
        .expect("test_nested_value_transfer_emits_multiple_logs failed");
}

// ===== Additional coverage =====

/// Test: large value transfer emits correct log.
#[tokio::test]
async fn test_large_value_transfer() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000001234");
    let value = U256::from(10_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("transfer")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_large_value_transfer failed");
}

/// Test: minimum value (1 wei) transfer emits correct log.
#[tokio::test]
async fn test_min_value_transfer() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000005678");
    let value = U256::from(1);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("transfer")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_min_value_transfer failed");
}

/// Test: multiple value transfers in one block each emit their own log.
#[tokio::test]
async fn test_multiple_transfers_in_block() {
    reth_tracing::init_test_tracing();

    let recipient_a = address!("0x000000000000000000000000000000000000aaaa");
    let recipient_b = address!("0x000000000000000000000000000000000000bbbb");
    let value_a = U256::from(100_000);
    let value_b = U256::from(200_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("tx_a")
                .with_to(recipient_a)
                .with_value(value_a),
        )
        .with_action(
            SendTransaction::new("tx_b")
                .with_to(recipient_b)
                .with_value(value_b),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("tx_a").expect(TxStatus::Success))
        .with_action(AssertTxIncluded::new("tx_b").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("tx_a")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient_a, value_a),
        )
        .with_action(
            AssertTxLogs::new("tx_b")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient_b, value_b),
        )
        .with_action(AssertTxTrace::new("tx_a"))
        .with_action(AssertTxTrace::new("tx_b"))
        .run()
        .await
        .expect("test_multiple_transfers_in_block failed");
}

/// Test: reverted value transfer to reverting contract does not leak balance.
///
/// Sends value to a reverting contract. The tx reverts, so no value is transferred.
/// Asserts the target contract's balance remains zero after the revert.
#[tokio::test]
async fn test_reverted_value_transfer_balance_unchanged() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::reverting_contract_deploy_code();
    let value = U256::from(500_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("deploy")
                .with_create()
                .with_data(deploy_code)
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy"))
        // Confirm contract starts with zero balance
        .with_action(AssertNamedBalance::of("deploy_address").equals(U256::ZERO))
        // Attempt value transfer — will revert
        .with_action(
            SendTransaction::new("revert_call")
                .with_to_named("deploy_address")
                .with_value(value)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("revert_call").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("revert_call").expect_no_logs())
        // Contract balance still zero — no value leaked through revert
        .with_action(AssertNamedBalance::of("deploy_address").equals(U256::ZERO))
        .run()
        .await
        .expect("test_reverted_value_transfer_balance_unchanged failed");
}
