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

//! EIP-7708 precompile interaction e2e tests.
//!
//! Tests cover both unauthorized (revert) and authorized (success) paths for
//! NativeCoinAuthority precompile operations (mint, burn, transfer).
//!
//! Under Zero5, the NativeCoinAuthority precompile only accepts calls from
//! `NATIVE_FIAT_TOKEN_ADDRESS` (0x3600..0000). Direct EOA calls are rejected.
//! Authorized calls go through the NativeFiatToken contract, which delegates
//! to the precompile. The operator wallet (index 7 in localdev genesis) has
//! the minter role.

mod helpers;

use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use arc_execution_e2e::{
    actions::{
        AssertBalance, AssertTxIncluded, AssertTxLogs, AssertTxTrace, ProduceBlocks,
        SendTransaction, TxStatus,
    },
    ArcSetup, ArcTestBuilder,
};
use helpers::constants::NATIVE_COIN_AUTHORITY_ADDRESS;

/// NativeFiatToken proxy contract address — the only caller authorized to invoke
/// NativeCoinAuthority under Zero5.
const NATIVE_FIAT_TOKEN_ADDRESS: Address = address!("0x3600000000000000000000000000000000000000");

/// NativeCoinControl precompile address.
const NATIVE_COIN_CONTROL_ADDRESS: Address = address!("0x1800000000000000000000000000000000000001");

/// Operator wallet index in localdev genesis (has minter role on NativeFiatToken).
const WALLET_OPERATOR_INDEX: usize = 7;

/// NativeFiatToken uses 6 decimals; the precompile operates in 18-decimal native units.
/// NativeFiatToken converts by multiplying by 10^12 before calling the precompile.
/// So 1 USDC (1_000_000 in 6-dec) becomes 10^18 in the precompile's event and balance.
const USDC_TO_NATIVE: U256 = U256::from_limbs([1_000_000_000_000u64, 0, 0, 0]); // 10^12

sol! {
    /// NativeFiatToken contract ABI (authorized path — operator calls these).
    interface INativeFiatToken {
        function mint(address to, uint256 amount) public;
        function burn(uint256 amount) public;
        function transfer(address to, uint256 amount) public returns (bool);
    }

    /// NativeCoinAuthority precompile ABI (unauthorized path — direct calls).
    interface INativeCoinAuthority {
        function mint(address to, uint256 amount) external returns (bool);
        function burn(address from, uint256 amount) external returns (bool);
        function transfer(address from, address to, uint256 amount) external returns (bool);
        function totalSupply() external view returns (uint256 supply);
    }
}

// ===== Unauthorized paths (#30-32): Direct EOA calls to precompile =====

/// Test #30: Direct unauthorized call to NativeCoinAuthority mint — reverts, no EIP-7708 log.
#[tokio::test]
async fn test_unauthorized_mint_call_reverts_no_log() {
    reth_tracing::init_test_tracing();

    let calldata = INativeCoinAuthority::mintCall {
        to: address!("0x000000000000000000000000000000000000bEEF"),
        amount: U256::from(1_000_000),
    }
    .abi_encode();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("mint_call")
                .with_to(NATIVE_COIN_AUTHORITY_ADDRESS)
                .with_value(U256::ZERO)
                .with_data(Bytes::from(calldata))
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("mint_call").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("mint_call").expect_no_logs())
        .with_action(AssertTxTrace::new("mint_call"))
        .run()
        .await
        .expect("test_unauthorized_mint_call_reverts_no_log failed");
}

/// Test #31: Direct unauthorized call to NativeCoinAuthority burn — reverts, no EIP-7708 log.
#[tokio::test]
async fn test_unauthorized_burn_call_reverts_no_log() {
    reth_tracing::init_test_tracing();

    let calldata = INativeCoinAuthority::burnCall {
        from: address!("0x000000000000000000000000000000000000bEEF"),
        amount: U256::from(1_000),
    }
    .abi_encode();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("burn_call")
                .with_to(NATIVE_COIN_AUTHORITY_ADDRESS)
                .with_value(U256::ZERO)
                .with_data(Bytes::from(calldata))
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("burn_call").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("burn_call").expect_no_logs())
        .with_action(AssertTxTrace::new("burn_call"))
        .run()
        .await
        .expect("test_unauthorized_burn_call_reverts_no_log failed");
}

/// Test #32: Direct unauthorized call to NativeCoinAuthority transfer — reverts, no EIP-7708 log.
#[tokio::test]
async fn test_unauthorized_transfer_call_reverts_no_log() {
    reth_tracing::init_test_tracing();

    let calldata = INativeCoinAuthority::transferCall {
        from: address!("0x000000000000000000000000000000000000bEEF"),
        to: address!("0x000000000000000000000000000000000000CAFE"),
        amount: U256::from(1_000),
    }
    .abi_encode();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer_call")
                .with_to(NATIVE_COIN_AUTHORITY_ADDRESS)
                .with_value(U256::ZERO)
                .with_data(Bytes::from(calldata))
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer_call").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("transfer_call").expect_no_logs())
        .with_action(AssertTxTrace::new("transfer_call"))
        .run()
        .await
        .expect("test_unauthorized_transfer_call_reverts_no_log failed");
}

// ===== Value to precompile addresses (#33-34) =====

/// Test #33: Value transfer to NativeCoinAuthority — reverts, no log.
#[tokio::test]
async fn test_value_to_native_coin_authority() {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("value_call")
                .with_to(NATIVE_COIN_AUTHORITY_ADDRESS)
                .with_value(U256::from(1_000))
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("value_call").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("value_call").expect_no_logs())
        .with_action(AssertTxTrace::new("value_call"))
        .run()
        .await
        .expect("test_value_to_native_coin_authority failed");
}

/// Test #34: Value transfer to NativeCoinControl — reverts, no log.
#[tokio::test]
async fn test_value_to_native_coin_control() {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("value_call")
                .with_to(NATIVE_COIN_CONTROL_ADDRESS)
                .with_value(U256::from(1_000))
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("value_call").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("value_call").expect_no_logs())
        .with_action(AssertTxTrace::new("value_call"))
        .run()
        .await
        .expect("test_value_to_native_coin_control failed");
}

/// Test #35: Zero-value call to NativeFiatToken — no EIP-7708 log.
#[tokio::test]
async fn test_zero_value_call_to_native_fiat_token() {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("call")
                .with_to(NATIVE_FIAT_TOKEN_ADDRESS)
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("call").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("call").expect_no_logs())
        .with_action(AssertTxTrace::new("call"))
        .run()
        .await
        .expect("test_zero_value_call_to_native_fiat_token failed");
}

/// Test #36: Direct totalSupply read — succeeds without log.
#[tokio::test]
async fn test_total_supply_read_no_log() {
    reth_tracing::init_test_tracing();

    let calldata = Bytes::from(INativeCoinAuthority::totalSupplyCall {}.abi_encode());

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("total_supply")
                .with_to(NATIVE_COIN_AUTHORITY_ADDRESS)
                .with_value(U256::ZERO)
                .with_data(calldata)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("total_supply").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("total_supply").expect_no_logs())
        .with_action(AssertTxTrace::new("total_supply"))
        .run()
        .await
        .expect("test_total_supply_read_no_log failed");
}

// ===== Authorized paths: NativeFiatToken mint/burn =====

/// Test: Authorized mint via NativeFiatToken — emits EIP-7708 Transfer log + Mint event.
///
/// The operator (wallet index 7) calls NativeFiatToken.mint(to, amount).
/// NativeFiatToken delegates to NativeCoinAuthority precompile.
/// Under Zero5, the precompile emits an EIP-7708 Transfer log from SYSTEM_ADDRESS
/// for the minted amount, plus the Solidity-level Mint and Transfer events from
/// the NativeFiatToken contract.
#[tokio::test]
async fn test_authorized_mint_via_native_fiat_token() {
    reth_tracing::init_test_tracing();

    let mint_recipient = address!("0x000000000000000000000000000000000000CAFE");
    // NativeFiatToken uses 6 decimals. Mint 1 USDC = 1_000_000 (6 decimals).
    // The precompile converts this to 18-decimal native units internally.
    let mint_amount_usdc = U256::from(1_000_000u64);

    let calldata = INativeFiatToken::mintCall {
        to: mint_recipient,
        amount: mint_amount_usdc,
    }
    .abi_encode();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("mint")
                .with_to(NATIVE_FIAT_TOKEN_ADDRESS)
                .with_value(U256::ZERO)
                .with_data(Bytes::from(calldata))
                .with_gas_limit(500_000)
                .with_wallet_index(WALLET_OPERATOR_INDEX),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("mint").expect(TxStatus::Success))
        // The precompile emits Transfer(0x0, to, amount) from SYSTEM_ADDRESS under Zero5.
        // NativeFiatToken converts 6-dec USDC to 18-dec native before calling the precompile,
        // so the event amount is in 18-decimal native units.
        .with_action(
            AssertTxLogs::new("mint").expect_transfer_event(
                0,
                Address::ZERO,
                mint_recipient,
                mint_amount_usdc
                    .checked_mul(USDC_TO_NATIVE)
                    .expect("usdc to native overflow"),
            ),
        )
        .with_action(AssertTxTrace::new("mint"))
        // Verify recipient balance in 18-decimal native units
        .with_action(AssertBalance::new(
            mint_recipient,
            mint_amount_usdc
                .checked_mul(USDC_TO_NATIVE)
                .expect("usdc to native overflow"),
        ))
        .run()
        .await
        .expect("test_authorized_mint_via_native_fiat_token failed");
}

/// Test: Authorized burn via NativeFiatToken — emits EIP-7708 Transfer log + Burn event.
///
/// Burns tokens from the operator's own balance. Requires the operator to have
/// balance, so we first mint to the operator, then burn.
#[tokio::test]
async fn test_authorized_burn_via_native_fiat_token() {
    reth_tracing::init_test_tracing();

    // Operator address (wallet index 7)
    let operator = {
        let wallet = reth_e2e_test_utils::wallet::Wallet::new(10).with_chain_id(1337);
        wallet.wallet_gen()[WALLET_OPERATOR_INDEX].address()
    };

    let mint_amount = U256::from(2_000_000u64); // 2 USDC
    let burn_amount = U256::from(1_000_000u64); // 1 USDC

    // Mint to operator first
    let mint_calldata = INativeFiatToken::mintCall {
        to: operator,
        amount: mint_amount,
    }
    .abi_encode();

    let burn_calldata = INativeFiatToken::burnCall {
        amount: burn_amount,
    }
    .abi_encode();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Step 1: Mint to operator
        .with_action(
            SendTransaction::new("mint")
                .with_to(NATIVE_FIAT_TOKEN_ADDRESS)
                .with_value(U256::ZERO)
                .with_data(Bytes::from(mint_calldata))
                .with_gas_limit(500_000)
                .with_wallet_index(WALLET_OPERATOR_INDEX),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("mint").expect(TxStatus::Success))
        // Step 2: Burn from operator
        .with_action(
            SendTransaction::new("burn")
                .with_to(NATIVE_FIAT_TOKEN_ADDRESS)
                .with_value(U256::ZERO)
                .with_data(Bytes::from(burn_calldata))
                .with_gas_limit(500_000)
                .with_wallet_index(WALLET_OPERATOR_INDEX),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("burn").expect(TxStatus::Success))
        // Burn emits Transfer(operator, 0x0, amount) from SYSTEM_ADDRESS under Zero5.
        // Amount is in 18-decimal native units (NativeFiatToken converts before calling precompile).
        .with_action(
            AssertTxLogs::new("burn").expect_transfer_event(
                0,
                operator,
                Address::ZERO,
                burn_amount
                    .checked_mul(USDC_TO_NATIVE)
                    .expect("usdc to native overflow"),
            ),
        )
        .with_action(AssertTxTrace::new("burn"))
        // Note: operator balance assertion omitted because the operator is funded in genesis
        // and pays gas in USDC, making the exact post-burn balance variable.
        // The Transfer log assertion above verifies the burn semantics.
        .run()
        .await
        .expect("test_authorized_burn_via_native_fiat_token failed");
}

/// Test: Authorized transfer via NativeFiatToken — emits exact EIP-7708 Transfer log.
///
/// Mints to the operator, then the operator calls NativeFiatToken.transfer(to, amount).
/// NativeFiatToken delegates to NativeCoinAuthority.transfer(from, to, amount).
/// Under Zero5, the precompile emits Transfer(from, to, amount) from SYSTEM_ADDRESS.
/// Verifies exact log fields and balance side effects.
#[tokio::test]
async fn test_authorized_transfer_via_native_fiat_token() {
    reth_tracing::init_test_tracing();

    let operator = {
        let wallet = reth_e2e_test_utils::wallet::Wallet::new(10).with_chain_id(1337);
        wallet.wallet_gen()[WALLET_OPERATOR_INDEX].address()
    };

    let transfer_recipient = address!("0x000000000000000000000000000000000000D00D");
    let mint_amount = U256::from(2_000_000u64); // 2 USDC
    let transfer_amount = U256::from(1_000_000u64); // 1 USDC

    let mint_calldata = INativeFiatToken::mintCall {
        to: operator,
        amount: mint_amount,
    }
    .abi_encode();

    let transfer_calldata = INativeFiatToken::transferCall {
        to: transfer_recipient,
        amount: transfer_amount,
    }
    .abi_encode();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Mint to operator first
        .with_action(
            SendTransaction::new("mint")
                .with_to(NATIVE_FIAT_TOKEN_ADDRESS)
                .with_value(U256::ZERO)
                .with_data(Bytes::from(mint_calldata))
                .with_gas_limit(500_000)
                .with_wallet_index(WALLET_OPERATOR_INDEX),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("mint").expect(TxStatus::Success))
        // Transfer from operator to recipient
        .with_action(
            SendTransaction::new("transfer")
                .with_to(NATIVE_FIAT_TOKEN_ADDRESS)
                .with_value(U256::ZERO)
                .with_data(Bytes::from(transfer_calldata))
                .with_gas_limit(500_000)
                .with_wallet_index(WALLET_OPERATOR_INDEX),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        // Transfer emits Transfer(operator, recipient, amount) from SYSTEM_ADDRESS.
        // Amount is in 18-decimal native units.
        .with_action(
            AssertTxLogs::new("transfer").expect_transfer_event(
                0,
                operator,
                transfer_recipient,
                transfer_amount
                    .checked_mul(USDC_TO_NATIVE)
                    .expect("usdc to native overflow"),
            ),
        )
        .with_action(AssertTxTrace::new("transfer"))
        // Operator balance omitted (genesis-funded + gas costs make exact value variable).
        // Recipient starts at zero and doesn't pay gas, so exact balance is deterministic.
        .with_action(AssertBalance::new(
            transfer_recipient,
            transfer_amount
                .checked_mul(USDC_TO_NATIVE)
                .expect("usdc to native overflow"),
        ))
        .run()
        .await
        .expect("test_authorized_transfer_via_native_fiat_token failed");
}
