// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

mod common;
use common::{setup_evm, NativeFiatTokenV2_2, WALLET_OPERATOR_INDEX};

use alloy_sol_types::{SolCall, SolEvent};
use arc_precompiles::helpers::NATIVE_FIAT_TOKEN_ADDRESS;
use reth_chainspec::{EthChainSpec, DEV};
use reth_evm::Evm;
use revm::context::TxEnv;
use revm::handler::SYSTEM_ADDRESS;
use revm_primitives::{Address, TxKind, U256};

const EIP7708_LOG_ADDRESS: Address = SYSTEM_ADDRESS;

#[test]
fn evm_usdc_mint() {
    let (mut evm, wallet) = setup_evm();
    let signer = wallet.wallet_gen()[WALLET_OPERATOR_INDEX].clone();

    let amount = U256::from(10_u128.pow(6) * 3);
    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: signer.address(),
        kind: TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS),
        gas_limit: 100_000,
        gas_price: 0,
        data: NativeFiatTokenV2_2::mintCall {
            to: signer.address(),
            amount,
        }
        .abi_encode()
        .into(),
        ..Default::default()
    };

    let exec = evm.transact_raw(tx).expect("Tx should be accepted");
    assert!(exec.result.is_success(), "execution failed, {exec:?}");
    // The precompile operates in 18-decimal native units (scaled from 6-decimal USDC by the
    // NativeFiatToken Solidity contract), so the EIP-7708 Transfer value is in wei.
    let native_amount = amount * U256::from(10_u128.pow(12));
    // Zero5 (active on localdev): EIP-7708 Transfer(0x0, to) replaces NativeCoinMinted.
    // Logs: EIP-7708 Transfer + Mint + Solidity Transfer from the NativeFiatToken contract.
    assert_eq!(exec.result.logs().len(), 3);
    // Log 0: EIP-7708 Transfer (mint) from system address — 18-decimal native amount
    assert_eq!(exec.result.logs()[0].address, EIP7708_LOG_ADDRESS);
    assert_eq!(
        NativeFiatTokenV2_2::Transfer::decode_log_data(&exec.result.logs()[0].data).unwrap(),
        NativeFiatTokenV2_2::Transfer {
            from: Address::ZERO,
            to: signer.address(),
            value: native_amount,
        }
    );
    // Log 1: Solidity Mint event from NativeFiatToken contract
    assert_eq!(exec.result.logs()[1].address, NATIVE_FIAT_TOKEN_ADDRESS);
    assert_eq!(
        NativeFiatTokenV2_2::Mint::decode_log_data(&exec.result.logs()[1].data).unwrap(),
        NativeFiatTokenV2_2::Mint {
            minter: signer.address(),
            recipient: signer.address(),
            value: amount,
        }
    );
    // Log 2: Solidity Transfer event from NativeFiatToken contract
    assert_eq!(exec.result.logs()[2].address, NATIVE_FIAT_TOKEN_ADDRESS);
    assert_eq!(
        NativeFiatTokenV2_2::Transfer::decode_log_data(&exec.result.logs()[2].data).unwrap(),
        NativeFiatTokenV2_2::Transfer {
            from: Address::ZERO,
            to: signer.address(),
            value: amount,
        }
    );

    let balance_before = evm
        .db_mut()
        .load_account(signer.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);

    let balance_after = exec
        .state
        .get(&signer.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    assert_eq!(
        balance_after,
        balance_before + amount * U256::from(10_u128.pow(12))
    );
}

#[test]
fn evm_usdc_burn() {
    let (mut evm, wallet) = setup_evm();
    let signer = wallet.wallet_gen()[WALLET_OPERATOR_INDEX].clone();

    let amount = U256::from(10_u128.pow(6) * 3);
    let tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: signer.address(),
        kind: TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS),
        value: U256::from(0),
        gas_limit: 100_000,
        gas_price: 0,
        data: NativeFiatTokenV2_2::burnCall { amount }.abi_encode().into(),
        ..Default::default()
    };

    let exec = evm.transact_raw(tx).expect("Tx should be accepted");
    assert!(exec.result.is_success(), "execution failed, {exec:?}",);
    // The precompile operates in 18-decimal native units — see mint test for explanation.
    let native_amount = amount * U256::from(10_u128.pow(12));
    // Zero5 (active on localdev): EIP-7708 Transfer(from, 0x0) replaces NativeCoinBurned.
    // Logs: EIP-7708 Transfer + Burn + Solidity Transfer from the NativeFiatToken contract.
    assert_eq!(exec.result.logs().len(), 3);
    // Log 0: EIP-7708 Transfer (burn) from system address — 18-decimal native amount
    assert_eq!(exec.result.logs()[0].address, EIP7708_LOG_ADDRESS);
    assert_eq!(
        NativeFiatTokenV2_2::Transfer::decode_log_data(&exec.result.logs()[0].data).unwrap(),
        NativeFiatTokenV2_2::Transfer {
            from: signer.address(),
            to: Address::ZERO,
            value: native_amount,
        }
    );
    // Log 1: Solidity Burn event from NativeFiatToken contract
    assert_eq!(exec.result.logs()[1].address, NATIVE_FIAT_TOKEN_ADDRESS);
    assert_eq!(
        NativeFiatTokenV2_2::Burn::decode_log_data(&exec.result.logs()[1].data).unwrap(),
        NativeFiatTokenV2_2::Burn {
            burner: signer.address(),
            value: amount,
        }
    );
    // Log 2: Solidity Transfer event from NativeFiatToken contract
    assert_eq!(exec.result.logs()[2].address, NATIVE_FIAT_TOKEN_ADDRESS);
    assert_eq!(
        NativeFiatTokenV2_2::Transfer::decode_log_data(&exec.result.logs()[2].data).unwrap(),
        NativeFiatTokenV2_2::Transfer {
            from: signer.address(),
            to: Address::ZERO,
            value: amount,
        }
    );

    let balance_before = evm
        .db_mut()
        .load_account(signer.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);

    let balance_after = exec
        .state
        .get(&signer.address())
        .map(|a| a.info.balance)
        .unwrap_or(U256::ZERO);
    assert_eq!(
        balance_after + amount * U256::from(10_u128.pow(12)),
        balance_before
    );
}

/// Tests that the blacklister cannot blacklist the USDC owner.
/// This is a security guard to prevent a compromised blacklister from
/// disabling the owner's ability to rotate roles.
#[test]
fn evm_usdc_cannot_blacklist_owner() {
    let (mut evm, wallet) = setup_evm();
    // Wallet index 7 is the operator/blacklister in localdev genesis
    let blacklister = wallet.wallet_gen()[WALLET_OPERATOR_INDEX].clone();

    // First, get the owner address by calling owner()
    let owner_call_tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: blacklister.address(),
        kind: TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS),
        gas_limit: 100_000,
        gas_price: 0,
        data: NativeFiatTokenV2_2::ownerCall {}.abi_encode().into(),
        ..Default::default()
    };

    let owner_result = evm
        .transact_raw(owner_call_tx)
        .expect("Owner call should succeed");
    assert!(owner_result.result.is_success(), "owner() call failed");

    let owner_address = Address::from_slice(&owner_result.result.output().unwrap()[12..32]);

    // Now attempt to blacklist the owner - this should fail
    let blacklist_tx = TxEnv {
        chain_id: Some(DEV.chain_id()),
        caller: blacklister.address(),
        kind: TxKind::Call(NATIVE_FIAT_TOKEN_ADDRESS),
        gas_limit: 100_000,
        gas_price: 0,
        data: NativeFiatTokenV2_2::blacklistCall {
            account: owner_address,
        }
        .abi_encode()
        .into(),
        ..Default::default()
    };

    let exec = evm
        .transact_raw(blacklist_tx)
        .expect("Tx should be accepted");

    // The transaction should revert
    assert!(
        !exec.result.is_success(),
        "Blacklisting owner should have reverted, but succeeded"
    );
}
