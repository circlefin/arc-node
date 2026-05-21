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

use color_eyre::eyre::{eyre, Result};
use std::{fs, path::Path};

// Get from the genesis file the number of prefunded accounts (allocations)
pub(crate) fn num_prefunded_accounts(genesis_file: &Path, num_validators: usize) -> Result<usize> {
    let genesis_data = fs::read_to_string(genesis_file)
        .map_err(|e| eyre!("Failed to read genesis file at {genesis_file:?}: {e}"))?;
    let genesis: serde_json::Value = serde_json::from_str(&genesis_data)?;
    let alloc = genesis["alloc"].as_object().ok_or_else(|| {
        eyre!("Malformed genesis at {genesis_file:?}: missing or non-object 'alloc'")
    })?;

    // Count EOAs (no code) with non-zero balance. This excludes:
    //   - contracts (have code)
    //   - one-time-address deployer stubs (no code but balance "0x0", nonce "0x1")
    // It includes the 10 hardhat default accounts, the sentinel EOA, controller
    // accounts, and the extra prefund accounts — all of which carry funded balances.
    let total_funded_eoas = alloc
        .iter()
        .filter(|(_, v)| {
            if !v["code"].is_null() {
                return false;
            }
            let hex = v["balance"].as_str().unwrap_or("0x0");
            // Non-zero if there is at least one hex digit that isn't '0'
            hex.strip_prefix("0x")
                .unwrap_or(hex)
                .chars()
                .any(|c| c != '0')
        })
        .count();

    // Other accounts: 10 hardhat default accounts (m/44'/60'/0'/0/{0-9}, of which
    // accounts 0-1 are sender/receiver, 7-9 are operator/admin/proxyAdmin, and 2-6
    // are unnamed prefunded fillers) plus 1 sentinel EOA (private key 0x…01).
    // Controller accounts are subtracted separately via num_validators.
    let other_accounts = 11;

    let reserved = other_accounts + num_validators;
    total_funded_eoas.checked_sub(reserved).ok_or_else(|| {
        eyre!(
            "Malformed genesis at {genesis_file:?}: only {total_funded_eoas} funded EOAs found, \
             but {other_accounts} hardhat/sentinel accounts plus {num_validators} validator \
             controller accounts ({reserved} total) were expected"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tempfile::NamedTempFile;

    fn write_genesis(alloc: Value) -> NamedTempFile {
        let f = NamedTempFile::new().unwrap();
        fs::write(
            f.path(),
            serde_json::to_string(&json!({ "alloc": alloc })).unwrap(),
        )
        .unwrap();
        f
    }

    fn funded(addr: &str) -> (String, Value) {
        (addr.into(), json!({ "balance": "0xde0b6b3a7640000" }))
    }

    fn contract(addr: &str) -> (String, Value) {
        (addr.into(), json!({ "balance": "0x0", "code": "0x6080" }))
    }

    fn deployer_stub(addr: &str) -> (String, Value) {
        (addr.into(), json!({ "balance": "0x0", "nonce": "0x1" }))
    }

    fn alloc_obj(entries: Vec<(String, Value)>) -> Value {
        let mut map = serde_json::Map::new();
        for (k, v) in entries {
            map.insert(k, v);
        }
        Value::Object(map)
    }

    #[test]
    fn counts_only_extra_funded_eoas() {
        let mut entries = vec![
            contract("0xc1"),
            contract("0xc2"),
            deployer_stub("0xd1"),
            deployer_stub("0xd2"),
            deployer_stub("0xd3"),
        ];
        // 11 hardhat/sentinel + 3 validator controllers + 4 extra prefund
        for i in 0..(11 + 3 + 4) {
            entries.push(funded(&format!("0x{i:040x}")));
        }
        let f = write_genesis(alloc_obj(entries));

        assert_eq!(num_prefunded_accounts(f.path(), 3).unwrap(), 4);
    }

    #[test]
    fn zero_extra_when_only_reserved_present() {
        let entries: Vec<_> = (0..(11 + 5))
            .map(|i| funded(&format!("0x{i:040x}")))
            .collect();
        let f = write_genesis(alloc_obj(entries));

        assert_eq!(num_prefunded_accounts(f.path(), 5).unwrap(), 0);
    }

    #[test]
    fn returns_error_when_funded_below_reserved() {
        let entries: Vec<_> = (0..5).map(|i| funded(&format!("0x{i:040x}"))).collect();
        let f = write_genesis(alloc_obj(entries));

        let err = num_prefunded_accounts(f.path(), 3).unwrap_err();
        assert!(err.to_string().contains("only 5 funded EOAs"));
    }

    #[test]
    fn returns_error_when_alloc_missing() {
        let f = NamedTempFile::new().unwrap();
        fs::write(f.path(), "{}").unwrap();

        let err = num_prefunded_accounts(f.path(), 0).unwrap_err();
        assert!(err.to_string().contains("missing or non-object 'alloc'"));
    }
}
