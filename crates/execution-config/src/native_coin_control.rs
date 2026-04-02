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

//! Native coin control storage helpers.

use alloy_primitives::{b256, keccak256, Address, B256};
use revm_primitives::U256;

/// Solidity mapping slot index for NativeCoinControl `isBlocklisted`.
pub const BLOCKLIST_MAPPING_SLOT: B256 =
    b256!("0000000000000000000000000000000000000000000000000000000000000002");

/// Computes the storage slot for a mapping key of type address.
///
/// Implements Solidity's mapping storage slot calculation:
/// Formula: `keccak256(h(k) . p)`, where:
/// - `k` is the mapping key (address)
/// - `p` is the mapping slot position ([`BLOCKLIST_MAPPING_SLOT`])
/// - `h` left-pads the key to 32 bytes
/// - `.` is concatenation
#[inline]
pub fn compute_is_blocklisted_storage_slot(key: Address) -> B256 {
    // Left-pad address to 32 bytes (addresses are 20 bytes, so 12 zero bytes prefix).
    let mut key_bytes = [0u8; 32];
    key_bytes[12..].copy_from_slice(key.as_ref());

    // Concatenate key and slot, then hash.
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(&key_bytes);
    data[32..].copy_from_slice(BLOCKLIST_MAPPING_SLOT.as_ref());

    B256::new(keccak256(data).0)
}

/// Returns true if a blocklist storage word means "blocked".
#[inline]
pub fn is_blocklisted_status(status: U256) -> bool {
    status != U256::ZERO
}

#[cfg(test)]
mod tests {
    use super::{compute_is_blocklisted_storage_slot, is_blocklisted_status};
    use alloy_primitives::{address, b256};
    use revm_primitives::U256;

    #[test]
    fn is_blocklisted_status_returns_false_for_zero() {
        assert!(!is_blocklisted_status(U256::ZERO));
    }

    #[test]
    fn is_blocklisted_status_returns_true_for_non_zero() {
        assert!(is_blocklisted_status(U256::from(1u64)));
        assert!(is_blocklisted_status(U256::MAX));
    }

    #[test]
    fn compute_is_blocklisted_storage_slot_matches() {
        // Example account taken from `assets/localdev/genesis.json` alloc.
        // Expected slot computed via:
        //   cast index address 0xD308a07F97db36C338e8FE2AfB09267781d00811 2
        let account = address!("0xD308a07F97db36C338e8FE2AfB09267781d00811");
        let expected_slot =
            b256!("c0814ebfa96e99aee5c17f259ae3205e7b664343916807a4a968c9f94e32f89b");
        let actual = compute_is_blocklisted_storage_slot(account);
        assert_eq!(actual, expected_slot);
    }
}
