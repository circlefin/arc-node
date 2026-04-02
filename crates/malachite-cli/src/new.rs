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

//! key and configuration generation

use std::str::FromStr;

use rand::rngs::OsRng;
use tracing::debug;

use bip32::{ChildNumber, DerivationPath, XPrv};
use bip39::Mnemonic;

use arc_consensus_types::signing::PrivateKey;

use crate::error::Error;

/// Fixed, non-secret test mnemonic for reproducible local testnets.
/// These are made to match reth test genesis generation in `assets/localdev/genesis.config.ts`.
/// DO NOT USE THESE SEEDS IN PRODUCTION!
const TEST_MNEMONIC: &str = "test test test test test test test test test test test junk";
const TEST_PASSPHRASE: &str = "";
const TEST_BIP39_DERIVATION_PATH_PREFIX: &str = "m/44'/60'/0'/1";

/// Derive `count` child private keys at m/44'/60'/0'/1/{start..}.
fn derive_bip39_child_sk_bytes(start_index: usize, count: usize) -> Result<Vec<[u8; 32]>, Error> {
    let m = Mnemonic::parse(TEST_MNEMONIC).map_err(|e| Error::DeriveBip39(e.to_string()))?;
    let seed = Mnemonic::to_seed(&m, TEST_PASSPHRASE);

    let base_path = DerivationPath::from_str(TEST_BIP39_DERIVATION_PATH_PREFIX)
        .map_err(|e| Error::DeriveBip39(e.to_string()))?;
    let base_xprv =
        XPrv::derive_from_path(seed, &base_path).map_err(|e| Error::DeriveBip39(e.to_string()))?;

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let idx = start_index + i;
        let cn =
            ChildNumber::new(idx as u32, false).map_err(|e| Error::DeriveBip39(e.to_string()))?;
        let child = base_xprv
            .derive_child(cn)
            .map_err(|e| Error::DeriveBip39(e.to_string()))?;
        let ga = child.private_key().to_bytes();
        let mut sk = [0u8; 32];
        sk.copy_from_slice(&ga);

        let display_path = format!("{TEST_BIP39_DERIVATION_PATH_PREFIX}/{idx}");
        debug!(path = %display_path, seed = %format!("0x{}", hex::encode(sk)), "derived child seed");

        out.push(sk);
    }

    Ok(out)
}

/// Generate private keys. Supports:
/// - random (default).
/// - deterministic via BIP-39/BIP-32 derived child private keys.
pub fn generate_private_keys(size: usize, deterministic: bool) -> Result<Vec<PrivateKey>, Error> {
    if deterministic {
        // `start_index` matches `assets/localdev/genesis.config.ts`.
        let start_index = 2;
        let seeds = derive_bip39_child_sk_bytes(start_index, size)?;
        let keys = seeds.into_iter().map(Into::into).collect();
        Ok(keys)
    } else {
        Ok((0..size).map(|_| PrivateKey::generate(OsRng)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_child_keys_deterministic() {
        // first three indices starting at 2
        let seeds = derive_bip39_child_sk_bytes(2, 3).unwrap();
        assert_eq!(seeds.len(), 3);

        // Re-run and ensure determinism
        let seeds2 = derive_bip39_child_sk_bytes(2, 3).unwrap();
        assert_eq!(seeds, seeds2);
    }

    #[test]
    fn derives_child_keys_well_known() {
        let expected_seeds = vec![
            "93ac6a66e7b27d3b21eb05c3edf07d3380019460b761ee117cdca9d3215e1b2d",
            "1a864302982c12335b26a63fd7b841c6491e58530fc2f25c23a4191a7ea31c90",
            "a8bf2e57dbee36fcc50f072cd71ee0f885e8b36cad256c927048fd5474b6ad56",
        ];

        let seeds = derive_bip39_child_sk_bytes(2, 3).unwrap();

        for (i, (seed, expected)) in seeds.iter().zip(expected_seeds).enumerate() {
            assert_eq!(hex::encode(seed), expected, "index {i}");
        }
    }
}
