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

use alloy_consensus::TxEip1559;
use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use color_eyre::eyre::Result;

use crate::config::Erc20Function;
use crate::generator::{TxGenerator, TESTNET_CHAIN_ID};
use crate::ws::WsClient;

/// TestToken ERC-20 contract address (deterministic deployment in genesis).
pub(crate) const TEST_TOKEN_ADDRESS: Address = address!("c35bA063F507CCf914FeEb69c8651ec695872587");

fn encode_transfer(to: Address, amount: U256) -> Bytes {
    sol! {
        function transfer(address to, uint256 amount) returns (bool);
    }
    transferCall { to, amount }.abi_encode().into()
}

pub(crate) fn encode_approve(spender: Address, amount: U256) -> Bytes {
    sol! {
        function approve(address spender, uint256 amount) returns (bool);
    }
    approveCall { spender, amount }.abi_encode().into()
}

pub(crate) fn encode_transfer_from(from: Address, to: Address, amount: U256) -> Bytes {
    sol! {
        function transferFrom(address from, address to, uint256 amount) returns (bool);
    }
    transferFromCall { from, to, amount }.abi_encode().into()
}

/// Prepare an ERC-20 tx with the given function: encode calldata, estimate gas, and build the tx.
pub(crate) async fn prepare_erc20_tx(
    ws_clients: &mut [WsClient],
    signer_addr: Address,
    recipient: Address,
    nonce: u64,
    function: Erc20Function,
) -> Result<TxEip1559> {
    let amount = U256::from(1_000_000_000_000_000_000u64); // 1 token (1e18)
    let calldata = match function {
        Erc20Function::Transfer => encode_transfer(recipient, amount),
        Erc20Function::Approve => encode_approve(recipient, amount),
        Erc20Function::TransferFrom => encode_transfer_from(signer_addr, recipient, amount),
    };
    let estimate =
        TxGenerator::estimate_gas_tx(ws_clients, signer_addr, Some(TEST_TOKEN_ADDRESS), &calldata)
            .await;
    let gas_limit = estimate
        .map(|g| g.saturating_mul(5) / 4) // 25% safety margin
        .unwrap_or(100_000); // ERC-20 call typically ~65k gas
    Ok(TxEip1559 {
        chain_id: TESTNET_CHAIN_ID,
        nonce,
        max_priority_fee_per_gas: 1_000_000_000, // 1 gwei
        max_fee_per_gas: 2_000_000_000,          // 2 gwei
        gas_limit,
        to: Some(TEST_TOKEN_ADDRESS).into(),
        value: U256::ZERO,
        input: calldata,
        access_list: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_transfer_produces_correct_selector_and_args() {
        let to = address!("0000000000000000000000000000000000000001");
        let amount = U256::from(1_000_000_000_000_000_000u64);
        let calldata = encode_transfer(to, amount);

        // ERC-20 transfer(address,uint256) selector = 0xa9059cbb
        assert_eq!(&calldata[..4], &[0xa9, 0x05, 0x9c, 0xbb]);
        // Total length: 4 (selector) + 32 (address) + 32 (amount) = 68
        assert_eq!(calldata.len(), 68);
    }

    #[test]
    fn encode_approve_produces_correct_selector_and_length() {
        let spender = address!("0000000000000000000000000000000000000002");
        let amount = U256::from(1_000_000_000_000_000_000u64);
        let calldata = encode_approve(spender, amount);

        // ERC-20 approve(address,uint256) selector = 0x095ea7b3
        assert_eq!(&calldata[..4], &[0x09, 0x5e, 0xa7, 0xb3]);
        // Total length: 4 (selector) + 32 (spender) + 32 (amount) = 68
        assert_eq!(calldata.len(), 68);
    }

    #[test]
    fn encode_transfer_from_produces_correct_selector_and_length() {
        let from = address!("0000000000000000000000000000000000000001");
        let to = address!("0000000000000000000000000000000000000002");
        let amount = U256::from(1_000_000_000_000_000_000u64);
        let calldata = encode_transfer_from(from, to, amount);

        // ERC-20 transferFrom(address,address,uint256) selector = 0x23b872dd
        assert_eq!(&calldata[..4], &[0x23, 0xb8, 0x72, 0xdd]);
        // Total length: 4 (selector) + 32 (from) + 32 (to) + 32 (amount) = 100
        assert_eq!(calldata.len(), 100);
    }
}
