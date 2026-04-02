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
use thiserror::Error;

/// Revert message when a transaction involves a denylisted address.
pub const ERR_DENYLISTED_ADDRESS: &str = "Address is denylisted";

/// Default Denylist contract address when deployed in genesis (localdev).
/// Matches the deterministic address used by the genesis builder (`scripts/genesis/addresses.ts`).
/// Node CLI can use this as the default for `--arc.denylist.address` when the contract is deployed in genesis.
///
/// Address derived via deterministic CREATE2 salt search: cast create2 with --seed keccak256("Denylist.v1"),
/// first match with prefix 0x360. Reproduce: `make mine-denylist-salt INIT_CODE_HASH=<hash>`
pub const DEFAULT_DENYLIST_ADDRESS: Address =
    address!("0x36082bA812806eB06C2758c412522669b5E2ac7b");

/// ERC-7201 base storage slot for the Denylist contract (arc.storage.Denylist.v1).
/// Matches the slot used by the genesis builder (`scripts/genesis/Denylist.ts`).
/// Node CLI can use this as the default for `--arc.denylist.storage-slot` when the contract is deployed in genesis.
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

/// Error when building [`AddressesDenylistConfig`] with `enabled` but missing address or slot.
#[derive(Debug, Error)]
pub enum AddressesDenylistConfigError {
    #[error("denylist is enabled but address is not set")]
    MissingContractAddress,
    #[error("denylist is enabled but storage slot is not set")]
    MissingStorageSlot,
}

/// Configuration for the addresses denylist.
///
/// Invalid states (enabled without address or slot) are unrepresentable.
#[derive(Default, Debug, Clone)]
pub enum AddressesDenylistConfig {
    /// Denylist checks disabled.
    #[default]
    Disabled,
    /// Denylist checks enabled. All fields required.
    Enabled {
        /// Denylist contract address.
        contract_address: Address,
        /// ERC-7201 base storage slot for the denylist.
        storage_slot: B256,
        /// Addresses to exclude from denylist checks (e.g. ops recovery).
        /// Stored deduplicated for fast lookup.
        addresses_exclusions: Vec<Address>,
    },
}

impl AddressesDenylistConfig {
    /// Build config. When `enabled` is true, `contract_address` and `storage_slot` must be set.
    /// Deduplicates exclusions.
    pub fn try_new(
        enabled: bool,
        contract_address: Option<Address>,
        storage_slot: Option<B256>,
        addresses_exclusions: Vec<Address>,
    ) -> Result<Self, AddressesDenylistConfigError> {
        if enabled {
            let contract_address =
                contract_address.ok_or(AddressesDenylistConfigError::MissingContractAddress)?;
            let storage_slot =
                storage_slot.ok_or(AddressesDenylistConfigError::MissingStorageSlot)?;
            let addresses_exclusions = addresses_exclusions.into_iter().unique().collect();
            Ok(Self::Enabled {
                contract_address,
                storage_slot,
                addresses_exclusions,
            })
        } else {
            Ok(Self::Disabled)
        }
    }

    /// Returns true if denylist checks are enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }

    /// Returns true if the given address is in the address exclusions set.
    /// When disabled, returns false (no exclusions apply).
    #[inline]
    pub fn is_address_excluded(&self, addr: &Address) -> bool {
        match self {
            Self::Disabled => false,
            Self::Enabled {
                addresses_exclusions,
                ..
            } => addresses_exclusions.iter().any(|a| a == addr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn default_is_disabled() {
        let cfg = AddressesDenylistConfig::default();
        assert!(!cfg.is_enabled());
    }

    #[test]
    fn try_new_enabled_with_both_succeeds() {
        let addr = address!("0x3600000000000000000000000000000000000001");
        let slot = B256::from([1u8; 32]);
        let cfg =
            AddressesDenylistConfig::try_new(true, Some(addr), Some(slot), Vec::new()).unwrap();
        assert!(cfg.is_enabled());
        if let AddressesDenylistConfig::Enabled {
            contract_address,
            storage_slot,
            ..
        } = &cfg
        {
            assert_eq!(*contract_address, addr);
            assert_eq!(*storage_slot, slot);
        } else {
            panic!("expected Enabled variant");
        }
    }

    #[test]
    fn try_new_enabled_without_address_fails() {
        let err =
            AddressesDenylistConfig::try_new(true, None, Some(B256::ZERO), Vec::new()).unwrap_err();
        assert!(matches!(
            err,
            AddressesDenylistConfigError::MissingContractAddress
        ));
    }

    #[test]
    fn try_new_enabled_without_slot_fails() {
        let err = AddressesDenylistConfig::try_new(true, Some(Address::ZERO), None, Vec::new())
            .unwrap_err();
        assert!(matches!(
            err,
            AddressesDenylistConfigError::MissingStorageSlot
        ));
    }

    #[test]
    fn try_new_disabled_accepts_none() {
        let cfg = AddressesDenylistConfig::try_new(false, None, None, Vec::new()).unwrap();
        assert!(!cfg.is_enabled());
    }

    #[test]
    fn exclusions_deduplicated() {
        let addr = address!("0x3600000000000000000000000000000000000001");
        let slot = B256::from([1u8; 32]);
        let addrs = vec![addr, addr];
        let cfg = AddressesDenylistConfig::try_new(true, Some(addr), Some(slot), addrs).unwrap();
        if let AddressesDenylistConfig::Enabled {
            addresses_exclusions,
            ..
        } = &cfg
        {
            assert_eq!(addresses_exclusions.len(), 1);
        } else {
            panic!("expected Enabled variant");
        }
    }

    #[test]
    fn is_address_excluded() {
        let addr1 = address!("0x3600000000000000000000000000000000000001");
        let addr2 = address!("0x3600000000000000000000000000000000000002");
        let slot = B256::from([1u8; 32]);
        let cfg =
            AddressesDenylistConfig::try_new(true, Some(addr1), Some(slot), vec![addr1]).unwrap();
        assert!(cfg.is_address_excluded(&addr1));
        assert!(!cfg.is_address_excluded(&addr2));
    }
}
