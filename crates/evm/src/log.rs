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

use alloy_primitives::{Address, U256};
use alloy_sol_types::{sol, SolEvent};
use arc_precompiles::NATIVE_COIN_AUTHORITY_ADDRESS;
use reth_ethereum::primitives::Log;
use revm::handler::SYSTEM_ADDRESS;

sol! {
    #[derive(Debug, PartialEq, Eq)]
    event NativeCoinTransferred(address indexed from, address indexed to, uint256 amount);
}

// Creates a log for native coin transfers
pub(crate) fn create_native_transfer_log(from: Address, to: Address, amount: U256) -> Log {
    let log_data = NativeCoinTransferred { from, to, amount }.encode_log_data();

    Log {
        address: NATIVE_COIN_AUTHORITY_ADDRESS,
        data: log_data,
    }
}

sol! {
    #[derive(Debug, PartialEq, Eq)]
    event Transfer(address indexed from, address indexed to, uint256 amount);
}

/// Creates an EIP-7708 ERC-20 Transfer log for native coin transfers.
///
/// Constructs the log manually to match the exact format REVM will use when upgraded
/// (via `eip7708_transfer_log`), rather than using `SolEvent::encode_log_data()`.
pub(crate) fn create_eip7708_transfer_log(from: Address, to: Address, amount: U256) -> Log {
    let log_data = Transfer { from, to, amount }.encode_log_data();

    Log {
        address: SYSTEM_ADDRESS,
        data: log_data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256, B256};
    use rstest::rstest;

    const ALICE: Address = address!("0x000000000000000000000000000000000000A11c");
    const BOB: Address = address!("0x0000000000000000000000000000000000000B0b");

    #[rstest]
    #[case::typical_transfer(
        ALICE,
        BOB,
        U256::from(1_000_000_000_000_000_000u128), // 1 USDC (18 decimals)
    )]
    #[case::zero_amount(ALICE, BOB, U256::ZERO)]
    #[case::max_amount(ALICE, BOB, U256::MAX)]
    #[case::same_address(ALICE, ALICE, U256::from(42))]
    #[case::zero_address_from(Address::ZERO, BOB, U256::from(1))]
    #[case::zero_address_to(ALICE, Address::ZERO, U256::from(1))]
    fn eip7708_log_structure(#[case] from: Address, #[case] to: Address, #[case] amount: U256) {
        let log = create_eip7708_transfer_log(from, to, amount);

        // Emitted from EIP-7708 system address
        assert_eq!(log.address, SYSTEM_ADDRESS);

        // 3 topics: event signature, indexed from, indexed to
        let topics = log.data.topics();
        assert_eq!(topics.len(), 3);
        assert_eq!(
            topics[0],
            b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef")
        );
        assert_eq!(topics[1], B256::left_padding_from(from.as_slice()));
        assert_eq!(topics[2], B256::left_padding_from(to.as_slice()));

        // Data encodes amount as big-endian uint256
        assert_eq!(log.data.data.as_ref(), &amount.to_be_bytes::<32>());
    }
}
