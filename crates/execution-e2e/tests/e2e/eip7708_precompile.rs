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

use super::helpers::{
    eip7708::{call_tracer_options, NATIVE_COIN_AUTHORITY_ADDRESS, TRANSFER_EVENT_SIGNATURE},
    utils::send_and_mine,
};
use alloy_primitives::{address, Address, Bytes, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use alloy_sol_types::{sol, SolCall};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;

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

fn native_units(usdc_amount: U256) -> U256 {
    usdc_amount
        .checked_mul(USDC_TO_NATIVE)
        .expect("usdc to native overflow")
}

// ===== Unauthorized paths (#30-32): Direct EOA calls to precompile =====

/// Test #30: Direct unauthorized call to NativeCoinAuthority mint — reverts, no EIP-7708 log.
#[tokio::test]
async fn test_unauthorized_mint_call_reverts_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let calldata = INativeCoinAuthority::mintCall {
        to: address!("0x000000000000000000000000000000000000bEEF"),
        amount: U256::from(1_000_000),
    }
    .abi_encode();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(NATIVE_COIN_AUTHORITY_ADDRESS)),
            value: Some(U256::ZERO),
            input: TransactionInput::new(Bytes::from(calldata)),
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

/// Test #31: Direct unauthorized call to NativeCoinAuthority burn — reverts, no EIP-7708 log.
#[tokio::test]
async fn test_unauthorized_burn_call_reverts_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let calldata = INativeCoinAuthority::burnCall {
        from: address!("0x000000000000000000000000000000000000bEEF"),
        amount: U256::from(1_000),
    }
    .abi_encode();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(NATIVE_COIN_AUTHORITY_ADDRESS)),
            value: Some(U256::ZERO),
            input: TransactionInput::new(Bytes::from(calldata)),
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

/// Test #32: Direct unauthorized call to NativeCoinAuthority transfer — reverts, no EIP-7708 log.
#[tokio::test]
async fn test_unauthorized_transfer_call_reverts_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let calldata = INativeCoinAuthority::transferCall {
        from: address!("0x000000000000000000000000000000000000bEEF"),
        to: address!("0x000000000000000000000000000000000000CAFE"),
        amount: U256::from(1_000),
    }
    .abi_encode();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(NATIVE_COIN_AUTHORITY_ADDRESS)),
            value: Some(U256::ZERO),
            input: TransactionInput::new(Bytes::from(calldata)),
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

// ===== Value to precompile addresses (#33-34) =====

/// Test #33: Value transfer to NativeCoinAuthority — reverts, no log.
#[tokio::test]
async fn test_value_to_native_coin_authority() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(NATIVE_COIN_AUTHORITY_ADDRESS)),
            value: Some(U256::from(1_000)),
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

/// Test #34: Value transfer to NativeCoinControl — reverts, no log.
#[tokio::test]
async fn test_value_to_native_coin_control() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(NATIVE_COIN_CONTROL_ADDRESS)),
            value: Some(U256::from(1_000)),
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

/// Test #35: Zero-value call to NativeFiatToken — no EIP-7708 log.
#[tokio::test]
async fn test_zero_value_call_to_native_fiat_token() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS)),
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

/// Test #36: Direct totalSupply read — succeeds without log.
#[tokio::test]
async fn test_total_supply_read_no_log() -> Result<()> {
    reth_tracing::init_test_tracing();

    let calldata = Bytes::from(INativeCoinAuthority::totalSupplyCall {}.abi_encode());

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(NATIVE_COIN_AUTHORITY_ADDRESS)),
            value: Some(U256::ZERO),
            input: TransactionInput::new(calldata),
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

// ===== Authorized paths: NativeFiatToken mint/burn =====

/// Test: Authorized mint via NativeFiatToken — emits EIP-7708 Transfer log + Mint event.
///
/// The operator (wallet index 7) calls NativeFiatToken.mint(to, amount).
/// NativeFiatToken delegates to NativeCoinAuthority precompile.
/// Under Zero5, the precompile emits an EIP-7708 Transfer log from SYSTEM_ADDRESS
/// for the minted amount, plus the Solidity-level Mint and Transfer events from
/// the NativeFiatToken contract.
#[tokio::test]
async fn test_authorized_mint_via_native_fiat_token() -> Result<()> {
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

    let native_amount = native_units(mint_amount_usdc);
    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let operator = node.wallet_signer(WALLET_OPERATOR_INDEX)?;
    let receipt = send_and_mine(
        &mut node,
        operator.clone(),
        TransactionRequest {
            from: Some(operator.address()),
            to: Some(TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS)),
            value: Some(U256::ZERO),
            input: TransactionInput::new(Bytes::from(calldata)),
            gas: Some(500_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());
    let log = &receipt.logs()[0];
    let topics = log.topics();
    assert_eq!(topics.len(), 3);
    assert_eq!(topics[0], TRANSFER_EVENT_SIGNATURE);
    assert_eq!(topics[1], Address::ZERO.into_word());
    assert_eq!(topics[2], mint_recipient.into_word());
    assert_eq!(
        log.data().data.as_ref(),
        native_amount.to_be_bytes::<32>().as_slice()
    );
    node.trace_transaction(receipt.transaction_hash, call_tracer_options())
        .await?;
    let balance = node.balance(mint_recipient, None).await?;
    assert_eq!(balance, native_amount);
    Ok(())
}

/// Test: Authorized burn via NativeFiatToken — emits EIP-7708 Transfer log + Burn event.
///
/// Burns tokens from the operator's own balance. Requires the operator to have
/// balance, so we first mint to the operator, then burn.
#[tokio::test]
async fn test_authorized_burn_via_native_fiat_token() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mint_amount = U256::from(2_000_000u64); // 2 USDC
    let burn_amount = U256::from(1_000_000u64); // 1 USDC

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let operator = node.wallet_signer(WALLET_OPERATOR_INDEX)?;
    // Mint to operator first
    let mint_calldata = INativeFiatToken::mintCall {
        to: operator.address(),
        amount: mint_amount,
    }
    .abi_encode();

    let burn_calldata = INativeFiatToken::burnCall {
        amount: burn_amount,
    }
    .abi_encode();

    let mint_receipt = send_and_mine(
        &mut node,
        operator.clone(),
        TransactionRequest {
            from: Some(operator.address()),
            to: Some(TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS)),
            value: Some(U256::ZERO),
            input: TransactionInput::new(Bytes::from(mint_calldata)),
            gas: Some(500_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(mint_receipt.status());

    let burn_receipt = send_and_mine(
        &mut node,
        operator.clone(),
        TransactionRequest {
            from: Some(operator.address()),
            to: Some(TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS)),
            value: Some(U256::ZERO),
            input: TransactionInput::new(Bytes::from(burn_calldata)),
            gas: Some(500_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(burn_receipt.status());
    let log = &burn_receipt.logs()[0];
    let topics = log.topics();
    assert_eq!(topics.len(), 3);
    assert_eq!(topics[0], TRANSFER_EVENT_SIGNATURE);
    assert_eq!(topics[1], operator.address().into_word());
    assert_eq!(topics[2], Address::ZERO.into_word());
    assert_eq!(
        log.data().data.as_ref(),
        native_units(burn_amount).to_be_bytes::<32>().as_slice()
    );
    node.trace_transaction(burn_receipt.transaction_hash, call_tracer_options())
        .await?;
    Ok(())
}

/// Test: Authorized transfer via NativeFiatToken — emits exact EIP-7708 Transfer log.
///
/// Mints to the operator, then the operator calls NativeFiatToken.transfer(to, amount).
/// NativeFiatToken delegates to NativeCoinAuthority.transfer(from, to, amount).
/// Under Zero5, the precompile emits Transfer(from, to, amount) from SYSTEM_ADDRESS.
/// Verifies exact log fields and balance side effects.
#[tokio::test]
async fn test_authorized_transfer_via_native_fiat_token() -> Result<()> {
    reth_tracing::init_test_tracing();

    let transfer_recipient = address!("0x000000000000000000000000000000000000D00D");
    let mint_amount = U256::from(2_000_000u64); // 2 USDC
    let transfer_amount = U256::from(1_000_000u64); // 1 USDC

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let operator = node.wallet_signer(WALLET_OPERATOR_INDEX)?;
    let mint_calldata = INativeFiatToken::mintCall {
        to: operator.address(),
        amount: mint_amount,
    }
    .abi_encode();

    let transfer_calldata = INativeFiatToken::transferCall {
        to: transfer_recipient,
        amount: transfer_amount,
    }
    .abi_encode();

    let mint_receipt = send_and_mine(
        &mut node,
        operator.clone(),
        TransactionRequest {
            from: Some(operator.address()),
            to: Some(TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS)),
            value: Some(U256::ZERO),
            input: TransactionInput::new(Bytes::from(mint_calldata)),
            gas: Some(500_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(mint_receipt.status());

    let transfer_receipt = send_and_mine(
        &mut node,
        operator.clone(),
        TransactionRequest {
            from: Some(operator.address()),
            to: Some(TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS)),
            value: Some(U256::ZERO),
            input: TransactionInput::new(Bytes::from(transfer_calldata)),
            gas: Some(500_000),
            ..Default::default()
        },
    )
    .await?;
    assert!(transfer_receipt.status());
    let native_transfer_amount = native_units(transfer_amount);
    let log = &transfer_receipt.logs()[0];
    let topics = log.topics();
    assert_eq!(topics.len(), 3);
    assert_eq!(topics[0], TRANSFER_EVENT_SIGNATURE);
    assert_eq!(topics[1], operator.address().into_word());
    assert_eq!(topics[2], transfer_recipient.into_word());
    assert_eq!(
        log.data().data.as_ref(),
        native_transfer_amount.to_be_bytes::<32>().as_slice()
    );
    node.trace_transaction(transfer_receipt.transaction_hash, call_tracer_options())
        .await?;
    let balance = node.balance(transfer_recipient, None).await?;
    assert_eq!(balance, native_transfer_amount);
    Ok(())
}
