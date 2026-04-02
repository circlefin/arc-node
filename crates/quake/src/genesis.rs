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

use color_eyre::eyre::{self, Result};
use std::{fs, path::Path};

// Get from the genesis file the number of prefunded accounts (allocations)
pub(crate) fn num_prefunded_accounts(genesis_file: &Path, num_validators: usize) -> Result<usize> {
    // Prefunded accounts are the allocations that have no code
    let genesis_data = fs::read_to_string(genesis_file)
        .map_err(|e| eyre::eyre!("Failed to read genesis file at {genesis_file:?}: {e}"))?;
    let genesis: serde_json::Value = serde_json::from_str(&genesis_data)?;
    let alloc = genesis["alloc"].as_object().unwrap();
    let total_prefunded_accounts = alloc.iter().filter(|(_, v)| v["code"].is_null()).count();

    // Other accounts are the 2 unused accounts (sender, receiver) plus 3 system
    // accounts (operator, admin, proxyAdmin), plus 1 sentinel EOA derived from
    // private key 0x…01.
    let other_accounts = 6;

    // Number of extra prefunded accounts = total number of Externally Owned
    // Accounts (EOAs) minus other accounts minus validator controller accounts.
    let num_extra_accounts = total_prefunded_accounts - other_accounts - num_validators;
    Ok(num_extra_accounts)
}
