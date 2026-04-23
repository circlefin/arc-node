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

mod helpers;

use alloy_primitives::{address, U256};
use arc_execution_e2e::{
    actions::{
        AssertTransferEvent, AssertTxIncluded, AssertTxLogs, AssertTxTrace, ProduceBlocks,
        SendTransaction, StoreDeployedAddress, TxStatus,
    },
    ArcSetup, ArcTestBuilder,
};
use helpers::{
    constants::{NATIVE_COIN_AUTHORITY_ADDRESS, SYSTEM_ADDRESS, WALLET_FIRST_ADDRESS},
    contracts::right_pad_address,
};
use rstest::rstest;

/// Test #48: Send value to a reverting contract — tx reverts, no EIP-7708 log.
///
/// When the entire CALL frame reverts, the EIP-7708 log is rolled back.
/// Deploys an actual reverting contract rather than using an existing address.
#[tokio::test]
async fn test_reverted_call_no_log() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::reverting_contract_deploy_code();
    let value = U256::from(1_000_000);

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
        // Send value to the reverting contract
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
        .expect("test_reverted_call_no_log failed");
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
async fn test_inner_call_reverts_outer_succeeds(#[case] value: U256) {
    reth_tracing::init_test_tracing();

    let reverting_deploy = helpers::contracts::reverting_contract_deploy_code();
    let outer_deploy = helpers::contracts::call_target_with_value_contract_deploy_code();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Deploy the reverting contract (inner target)
        .with_action(
            SendTransaction::new("deploy_reverting")
                .with_create()
                .with_data(reverting_deploy)
                .with_value(U256::ZERO)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy_reverting").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy_reverting"))
        // Deploy the outer contract (calls target from calldata with value)
        .with_action(
            SendTransaction::new("deploy_outer")
                .with_create()
                .with_data(outer_deploy)
                .with_value(U256::ZERO)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("deploy_outer").expect(TxStatus::Success))
        .with_action(StoreDeployedAddress::new("deploy_outer"))
        // Call the outer contract with value, passing the reverting contract's address
        // as calldata. Uses stored address instead of nonce-derived guess.
        .with_action(
            SendTransaction::new("call")
                .with_to_named("deploy_outer_address")
                .with_value(value)
                .with_data_fn(|env| {
                    let addr = env.get_address("deploy_reverting_address").ok_or_else(|| {
                        eyre::eyre!("Named address 'deploy_reverting_address' not found")
                    })?;
                    Ok(right_pad_address(*addr))
                })
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("call").expect(TxStatus::Success))
        // Outer frame succeeds → sender→outer_contract log preserved.
        // Inner call reverts → inner log rolled back.
        .with_action(AssertTxLogs::new("call").expect_log_count(1))
        .with_action(AssertTransferEvent::new(
            "call",
            0,
            WALLET_FIRST_ADDRESS,
            AssertTransferEvent::named("deploy_outer_address"),
            value,
        ))
        .with_action(AssertTxTrace::new("call"))
        .run()
        .await
        .expect("test_inner_call_reverts_outer_succeeds failed");
}

/// Test #51: Multiple sequential value transfers in separate blocks.
///
/// Each block contains a value transfer, verifying logs are emitted consistently
/// across blocks and don't leak between transactions.
#[tokio::test]
async fn test_sequential_blocks_each_emit_log() {
    reth_tracing::init_test_tracing();

    let recipient_1 = address!("0x000000000000000000000000000000000000AAA1");
    let recipient_2 = address!("0x000000000000000000000000000000000000AAA2");
    let value_1 = U256::from(100_000);
    let value_2 = U256::from(200_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Block 1
        .with_action(
            SendTransaction::new("tx1")
                .with_to(recipient_1)
                .with_value(value_1),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("tx1").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("tx1")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient_1, value_1),
        )
        // Block 2
        .with_action(
            SendTransaction::new("tx2")
                .with_to(recipient_2)
                .with_value(value_2),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("tx2").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("tx2")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient_2, value_2),
        )
        .with_action(AssertTxTrace::new("tx1"))
        .with_action(AssertTxTrace::new("tx2"))
        .run()
        .await
        .expect("test_sequential_blocks_each_emit_log failed");
}

/// Test #52: Contract calls NativeCoinAuthority precompile with value.
///
/// Deploys a contract that forwards a CALL with value to the NativeCoinAuthority
/// precompile address. The precompile will revert (unauthorized caller), but
/// the outer frame succeeds. The outer value transfer log (sender→contract)
/// is preserved; the inner log (contract→precompile) is rolled back because
/// the precompile rejects the call.
#[tokio::test]
async fn test_contract_calls_precompile_with_value() {
    reth_tracing::init_test_tracing();

    let deploy_code = helpers::contracts::call_target_with_value_contract_deploy_code();
    let value = U256::from(500_000);

    // Encode NativeCoinAuthority address as calldata target.
    // The bytecode reads target via CALLDATALOAD(0) + SHR(96), extracting
    // the top 20 bytes. So address must be right-padded (address at left).
    let calldata = helpers::contracts::right_pad_address(NATIVE_COIN_AUTHORITY_ADDRESS);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
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
        // Call the contract with value + precompile target in calldata
        .with_action(
            SendTransaction::new("call")
                .with_to_named("deploy_address")
                .with_value(value)
                .with_data(calldata)
                .with_gas_limit(200_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("call").expect(TxStatus::Success))
        // Outer transfer succeeds (sender→contract), inner reverts (contract→precompile)
        // Only the outer log with exact from/to/value
        .with_action(AssertTxLogs::new("call").expect_log_count(1))
        .with_action(AssertTransferEvent::new(
            "call",
            0,
            WALLET_FIRST_ADDRESS,
            AssertTransferEvent::named("deploy_address"),
            value,
        ))
        .with_action(AssertTxTrace::new("call"))
        .run()
        .await
        .expect("test_contract_calls_precompile_with_value failed");
}

/// Test #53: Value transfer after producing multiple empty blocks.
///
/// Verifies that EIP-7708 log emission works correctly even when
/// there are empty blocks between genesis and the transfer.
#[tokio::test]
async fn test_log_after_empty_blocks() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000CC0001");
    let value = U256::from(500_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Produce several empty blocks first
        .with_action(ProduceBlocks::new(5))
        // Now send a value transfer
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
        .expect("test_log_after_empty_blocks failed");
}

/// Test: Transfer to a contract that exists but has no code (EOA-like).
#[tokio::test]
async fn test_transfer_to_codeless_address() {
    reth_tracing::init_test_tracing();

    let target = address!("0x000000000000000000000000000000000000DEAD");
    let value = U256::from(1_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer")
                .with_to(target)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("transfer")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, target, value),
        )
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_transfer_to_codeless_address failed");
}

/// Test: Value transfer and zero-value transfer in same block.
///
/// Only the value transfer should emit a log; the zero-value transfer should not.
#[tokio::test]
async fn test_mixed_value_and_zero_value_in_block() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000F00D");
    let value = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("with_value")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(
            SendTransaction::new("zero_value")
                .with_to(recipient)
                .with_value(U256::ZERO),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("with_value").expect(TxStatus::Success))
        .with_action(AssertTxIncluded::new("zero_value").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("with_value")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .with_action(AssertTxLogs::new("zero_value").expect_no_logs())
        .run()
        .await
        .expect("test_mixed_value_and_zero_value_in_block failed");
}
