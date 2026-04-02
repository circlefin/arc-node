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

//! Addresses denylist checker: answers "is this address denylisted?" by reading the Denylist
//! contract storage. Call from both mempool and execution/Revm.
//! Uses ERC-7201 storage layout matching the Denylist contract. Provider/SLOAD
//! errors are propagated; callers may treat `Err` as fail-open (do not block) if desired.
//!
//! Config lives in [`arc_execution_config::addresses_denylist`]; this module only implements the check.

use alloy_primitives::{Address, B256};
use arc_execution_config::addresses_denylist::{
    compute_denylist_storage_slot, AddressesDenylistConfig,
};
use reth_storage_api::errors::provider::ProviderError;
use reth_storage_api::StateProvider;
use tracing::trace;

/// Error types for denylist storage lookups
#[derive(Debug, thiserror::Error)]
pub enum DenylistError {
    #[error("Storage read failed: {0}")]
    StorageReadFailed(#[from] ProviderError),
}

/// Abstraction for reading a single storage slot. Implemented for [`StateProvider`].
#[cfg_attr(test, mockall::automock)]
pub trait DenylistStorageReader {
    /// Read storage at (address, slot). Returns Ok(Some(value)) if non-empty, Ok(None) if empty,
    /// Err on provider/account failure.
    fn read_storage(
        &self,
        address: Address,
        slot: alloy_primitives::StorageKey,
    ) -> Result<Option<B256>, DenylistError>;
}

impl<T: StateProvider + ?Sized> DenylistStorageReader for T {
    fn read_storage(
        &self,
        address: Address,
        slot: alloy_primitives::StorageKey,
    ) -> Result<Option<B256>, DenylistError> {
        self.storage(address, slot)
            .map(|opt| opt.map(|v| B256::from(v.to_be_bytes::<32>())))
            .map_err(DenylistError::StorageReadFailed)
    }
}

/// Returns whether `address` is denylisted at the given state.
///
/// Call from mempool validation with a state provider for the relevant block.
///
/// - Returns `Ok(false)` if config has denylist disabled, or `address` is in
///   `addresses_exclusions`.
/// - Otherwise performs one SLOAD at `(contract_address, slot)` and returns
///   `Ok(true)` iff the value is non-zero (denylisted).
/// - On provider/SLOAD error returns `Err(DenylistError)`; callers may treat as fail-open (do not block).
#[inline]
#[must_use = "ignoring denylist check result could allow denylisted addresses to transact"]
pub fn is_denylisted<P: DenylistStorageReader + ?Sized>(
    provider: &P,
    config: &AddressesDenylistConfig,
    address: Address,
) -> Result<bool, DenylistError> {
    let AddressesDenylistConfig::Enabled {
        contract_address,
        storage_slot,
        ..
    } = config
    else {
        return Ok(false);
    };

    if config.is_address_excluded(&address) {
        trace!(%address, "address is explicitly excluded from denylist");
        return Ok(false);
    }

    let storage_key =
        alloy_primitives::StorageKey::from(compute_denylist_storage_slot(address, *storage_slot));
    let value = provider.read_storage(*contract_address, storage_key)?;

    // Non-zero means denylisted (bool true in Solidity).
    // None (uninitialized storage) equals zero (not denylisted per Solidity semantics).
    Ok(!value.unwrap_or_default().is_zero())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arc_execution_config::addresses_denylist::DEFAULT_DENYLIST_ERC7201_BASE_SLOT;

    fn denylisted_slot_value() -> B256 {
        let mut one = [0u8; 32];
        one[31] = 1;
        B256::new(one)
    }

    #[test]
    fn is_denylisted_returns_false_when_disabled() {
        let config = AddressesDenylistConfig::try_new(
            false,
            Some(Address::ZERO),
            Some(DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
            Vec::new(),
        )
        .unwrap();
        let mock = MockDenylistStorageReader::new();
        assert!(!is_denylisted(&mock, &config, Address::from([1u8; 20])).unwrap());
    }

    #[test]
    fn is_denylisted_returns_false_when_address_excluded() {
        let addr = Address::from([1u8; 20]);
        let config = AddressesDenylistConfig::try_new(
            true,
            Some(Address::from([0x36u8; 20])),
            Some(DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
            vec![addr],
        )
        .unwrap();
        let mock = MockDenylistStorageReader::new();
        assert!(!is_denylisted(&mock, &config, addr).unwrap());
    }

    #[test]
    fn is_denylisted_returns_true_when_storage_non_zero() {
        let addr = Address::from([1u8; 20]);
        let contract = Address::from([0x36u8; 20]);
        let config = AddressesDenylistConfig::try_new(
            true,
            Some(contract),
            Some(DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
            Vec::new(),
        )
        .unwrap();
        let mut mock = MockDenylistStorageReader::new();
        mock.expect_read_storage()
            .returning(|_addr, _slot| Ok(Some(denylisted_slot_value())));
        assert!(is_denylisted(&mock, &config, addr).unwrap());
    }

    #[test]
    fn is_denylisted_returns_false_when_storage_zero() {
        let addr = Address::from([1u8; 20]);
        let contract = Address::from([0x36u8; 20]);
        let config = AddressesDenylistConfig::try_new(
            true,
            Some(contract),
            Some(DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
            Vec::new(),
        )
        .unwrap();
        let mut mock = MockDenylistStorageReader::new();
        mock.expect_read_storage()
            .returning(|_addr, _slot| Ok(None));
        assert!(!is_denylisted(&mock, &config, addr).unwrap());
    }

    #[test]
    fn is_denylisted_returns_err_on_provider_error() {
        let config = AddressesDenylistConfig::try_new(
            true,
            Some(Address::from([0x36u8; 20])),
            Some(DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
            Vec::new(),
        )
        .unwrap();
        let mut mock = MockDenylistStorageReader::new();
        mock.expect_read_storage().returning(|_addr, _slot| {
            Err(DenylistError::StorageReadFailed(
                reth_storage_api::errors::provider::ProviderError::BlockHashNotFound(B256::ZERO),
            ))
        });
        let res = is_denylisted(&mock, &config, Address::from([1u8; 20]));
        assert!(res.is_err(), "provider error must be propagated");
    }
}
