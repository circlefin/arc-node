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

//! Configuration for the addresses denylist.
//!
//! Used by mempool validation and Revm pre-flight when integrated.
//! This module defines the config type and validation only; no chain reads.

use alloy_primitives::{address, b256, Address, B256};
use itertools::Itertools;

/// Revert message when a transaction involves a denylisted address.
pub const ERR_DENYLISTED_ADDRESS: &str = "Address is denylisted";

/// Denylist proxy address on localdev, sourced from the genesis builder
/// (`scripts/genesis/addresses.ts`, `denylistAddressByNetwork.localdev`).
///
/// Unlike the other networks, this is a mined CREATE2 address rather than a fixed
/// system-contract slot, so it changes whenever the Denylist init-code changes and the
/// localdev genesis is regenerated. Derived via deterministic CREATE2 salt search: cast
/// create2 with --seed keccak256("Denylist.v1"), first match with prefix 0x360.
/// Reproduce: `make mine-denylist-salt INIT_CODE_HASH=<hash>`.
///
/// Deployed networks hardcode their addresses in `chainspec.rs` instead; resolution for all
/// networks goes through [`ArcChainSpec::denylist_address`](crate::chainspec::ArcChainSpec::denylist_address).
pub const DENYLIST_ADDRESS_LOCALDEV: Address =
    address!("0x36059b615370eB999e8eC0c9401835B407834221");

/// ERC-7201 base storage slot for the Denylist contract (arc.storage.Denylist.v1).
/// Matches the slot used by the genesis builder (`scripts/genesis/Denylist.ts`).
/// Namespace-derived, so it is identical on every network.
pub const DEFAULT_DENYLIST_ERC7201_BASE_SLOT: B256 =
    b256!("0x1d7e1388d3ae56f3d9c18b1ce8d2b3b1a238a0edf682d2053af5d8a1d2f12f00");

/// Computes the ERC-7201 storage slot for `address` in the Denylist contract's denylisted mapping.
/// Matches the formula: `keccak256(abi.encode(address, base_slot))`.
#[inline]
#[must_use]
pub fn compute_denylist_storage_slot(address: Address, base_slot: B256) -> B256 {
    use alloy_primitives::keccak256;
    use alloy_sol_types::SolValue;

    let encoded = (address, base_slot).abi_encode();
    B256::from(keccak256(encoded.as_slice()).0)
}

/// Configuration for the addresses denylist.
///
/// There is no "denylist off" state. The denylist is a protocol requirement, so every running
/// node has one; a chain spec Arc does not recognise is rejected at startup rather than run
/// without denylist checks. Fields are private so [`AddressesDenylistConfig::new`] is the
/// only way to build one, which keeps the exclusions deduplicated.
#[derive(Debug, Clone)]
pub struct AddressesDenylistConfig {
    /// Denylist contract address.
    contract_address: Address,
    /// ERC-7201 base storage slot for the denylist.
    storage_slot: B256,
    /// Addresses to exclude from denylist checks (e.g. ops recovery).
    /// Stored deduplicated for fast lookup.
    addresses_exclusions: Vec<Address>,
}

impl AddressesDenylistConfig {
    /// Build a denylist config. Deduplicates exclusions.
    ///
    /// Infallible: the contract address and storage slot are resolved from the chain spec rather
    /// than operator input, so there is no unconfigured state to reject.
    pub fn new(
        contract_address: Address,
        storage_slot: B256,
        addresses_exclusions: Vec<Address>,
    ) -> Self {
        Self {
            contract_address,
            storage_slot,
            addresses_exclusions: addresses_exclusions.into_iter().unique().collect(),
        }
    }

    /// Denylist contract address to read denylist storage from.
    #[inline]
    pub fn contract_address(&self) -> Address {
        self.contract_address
    }

    /// ERC-7201 base storage slot for the denylist mapping.
    #[inline]
    pub fn storage_slot(&self) -> B256 {
        self.storage_slot
    }

    /// Returns true if the given address is in the address exclusions set.
    #[inline]
    pub fn is_address_excluded(&self, addr: &Address) -> bool {
        self.addresses_exclusions.iter().any(|a| a == addr)
    }

    /// Addresses excluded from denylist checks, deduplicated.
    #[inline]
    pub fn addresses_exclusions(&self) -> &[Address] {
        &self.addresses_exclusions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn new_sets_address_and_slot() {
        let addr = address!("0x3600000000000000000000000000000000000001");
        let slot = B256::from([1u8; 32]);
        let cfg = AddressesDenylistConfig::new(addr, slot, Vec::new());
        assert_eq!(cfg.contract_address(), addr);
        assert_eq!(cfg.storage_slot(), slot);
        assert!(cfg.addresses_exclusions().is_empty());
    }

    #[test]
    fn exclusions_deduplicated() {
        let addr = address!("0x3600000000000000000000000000000000000001");
        let slot = B256::from([1u8; 32]);
        let cfg = AddressesDenylistConfig::new(addr, slot, vec![addr, addr]);
        assert_eq!(cfg.addresses_exclusions(), &[addr]);
    }

    #[test]
    fn is_address_excluded() {
        let addr1 = address!("0x3600000000000000000000000000000000000001");
        let addr2 = address!("0x3600000000000000000000000000000000000002");
        let slot = B256::from([1u8; 32]);
        let cfg = AddressesDenylistConfig::new(addr1, slot, vec![addr1]);
        assert!(cfg.is_address_excluded(&addr1));
        assert!(!cfg.is_address_excluded(&addr2));
    }
}
