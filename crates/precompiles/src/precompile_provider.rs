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

extern crate alloc;

use crate::native_coin_authority::{run_native_coin_authority, NATIVE_COIN_AUTHORITY_ADDRESS};
use crate::native_coin_control::{run_native_coin_control, NATIVE_COIN_CONTROL_ADDRESS};
use crate::pq::{run_pq, PQ_ADDRESS};
use crate::system_accounting::{run_system_accounting, SYSTEM_ACCOUNTING_ADDRESS};
use alloy_evm::precompiles::PrecompilesMap;
use alloy_primitives::Address;
use arc_execution_config::hardforks::{ArcHardfork, ArcHardforkFlags};
use reth_ethereum::evm::revm::precompile::PrecompileSpecId;
use reth_ethereum::evm::revm::precompile::Precompiles;
use reth_evm::precompiles::DynPrecompile;
use revm::precompile::PrecompileId;
use revm::primitives::hardfork::SpecId;
use revm_primitives::address;

/// Custom precompile provider that extends Ethereum's standard precompiles
/// with Arc functionality.
///
/// This provider supports both:
/// - **Stateless precompiles**: Added at compile time via `custom_stateless_precompiles()`
/// - **Stateful precompiles**: Managed dynamically via the `stateful_precompiles` HashMap
#[derive(Debug)]
pub struct ArcPrecompileProvider;

impl ArcPrecompileProvider {
    /// Creates a precompiles map based on the spec and hardfork flags
    pub fn create_precompiles_map(
        spec: SpecId,
        hardfork_flags: ArcHardforkFlags,
    ) -> PrecompilesMap {
        let base_precompiles = Precompiles::new(PrecompileSpecId::from_spec_id(spec));
        let mut precompile_map = PrecompilesMap::from_static(base_precompiles);
        precompile_map.ensure_dynamic_precompiles();
        precompile_map.set_precompile_lookup(move |address: &Address| match *address {
            NATIVE_COIN_AUTHORITY_ADDRESS => Some(DynPrecompile::new_stateful(
                PrecompileId::Custom("NATIVE_COIN_AUTHORITY".into()),
                move |input| run_native_coin_authority(input, hardfork_flags),
            )),
            NATIVE_COIN_CONTROL_ADDRESS => Some(DynPrecompile::new_stateful(
                PrecompileId::Custom("NATIVE_COIN_CONTROL".into()),
                move |input| run_native_coin_control(input, hardfork_flags),
            )),
            SYSTEM_ACCOUNTING_ADDRESS => Some(DynPrecompile::new_stateful(
                PrecompileId::Custom("SYSTEM_ACCOUNTING".into()),
                move |input| run_system_accounting(input, hardfork_flags),
            )),
            PQ_ADDRESS => {
                // Only register PQ precompile if Zero6 hardfork is active
                if !hardfork_flags.is_active(ArcHardfork::Zero6) {
                    return None;
                }
                Some(DynPrecompile::new_stateful(
                    PrecompileId::Custom("PQ".into()),
                    move |input| run_pq(input, hardfork_flags),
                ))
            }
            _ => handle_unknown_precompile(address),
        });
        precompile_map
    }

    /// The P256 (secp256r1) precompile address as defined in EIP-7212.
    /// This precompile is available starting from the Osaka hardfork.
    pub const P256_PRECOMPILE_ADDRESS: Address =
        address!("0x0000000000000000000000000000000000000100");
}

fn handle_unknown_precompile(_address: &Address) -> Option<DynPrecompile> {
    #[cfg(any(test, feature = "integration"))]
    return match_e2e_precompile(_address);
    #[cfg(not(any(test, feature = "integration")))]
    None
}

#[cfg(any(test, feature = "integration"))]
pub const PANIC_PRECOMPILE_ADDRESS: Address =
    address!("0xdead000000000000000000000000000000000001");
#[cfg(any(test, feature = "integration"))]
fn match_e2e_precompile(address: &Address) -> Option<DynPrecompile> {
    match *address {
        PANIC_PRECOMPILE_ADDRESS => Some(DynPrecompile::new_stateful(
            PrecompileId::Custom("PANICKING_TEST".into()),
            |_input| panic!("test panicking precompile"),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_p256_precompile_available_with_osaka() {
        let precompiles = ArcPrecompileProvider::create_precompiles_map(
            SpecId::OSAKA,
            ArcHardforkFlags::default(),
        );

        // Verify P256 precompile exists at address 0x100 with Osaka
        assert!(
            precompiles
                .get(&ArcPrecompileProvider::P256_PRECOMPILE_ADDRESS)
                .is_some(),
            "P256 precompile should be available with Osaka hardfork"
        );
    }

    #[test]
    fn test_pq_precompile_available_with_zero6() {
        let precompiles = ArcPrecompileProvider::create_precompiles_map(
            SpecId::PRAGUE,
            ArcHardforkFlags::with(&[ArcHardfork::Zero6]),
        );

        assert!(
            precompiles.get(&PQ_ADDRESS).is_some(),
            "PQ precompile should be available when Zero6 is active"
        );
    }

    #[test]
    fn test_pq_precompile_not_available_without_zero6() {
        let precompiles = ArcPrecompileProvider::create_precompiles_map(
            SpecId::PRAGUE,
            ArcHardforkFlags::default(),
        );

        assert!(
            precompiles.get(&PQ_ADDRESS).is_none(),
            "PQ precompile should NOT be available without Zero6"
        );
    }

    #[test]
    fn test_p256_precompile_not_available_with_prague() {
        let precompiles = ArcPrecompileProvider::create_precompiles_map(
            SpecId::PRAGUE,
            ArcHardforkFlags::default(),
        );

        // P256 precompile should NOT be available with Prague
        assert!(
            precompiles
                .get(&ArcPrecompileProvider::P256_PRECOMPILE_ADDRESS)
                .is_none(),
            "P256 precompile should NOT be available with Prague hardfork"
        );
    }
}
