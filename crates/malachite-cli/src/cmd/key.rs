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

//! Key command
//!
//! Loads the private validator key from disk and displays the public key and address.

use std::path::{Path, PathBuf};

use clap::Parser;
use tracing::info;

use arc_consensus_types::signing::PrivateKey;
use arc_consensus_types::Address;

use crate::error::Error;

#[derive(Parser, Debug, Clone, Default, PartialEq)]
pub struct KeyCmd {
    /// Path to the private validator key file.
    /// If not specified, uses the default path under --home.
    pub key_file: Option<PathBuf>,
}

impl KeyCmd {
    pub fn run(&self, default_key_file: &Path) -> Result<(), Error> {
        let priv_validator_key_file = self.key_file.as_deref().unwrap_or(default_key_file);
        let contents = std::fs::read_to_string(priv_validator_key_file)
            .map_err(|_| Error::LoadFile(priv_validator_key_file.to_path_buf()))?;

        let private_key: PrivateKey =
            serde_json::from_str(&contents).map_err(|e| Error::FromJSON(e.to_string()))?;

        info!(file = %priv_validator_key_file.display(), "Loaded private key");

        let public_key = private_key.public_key();
        let address = Address::from_public_key(&public_key);

        info!(
            public_key = %format!("0x{}", hex::encode(public_key.as_bytes())),
            address = %address,
            "Key information",
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::save_priv_validator_key;
    use crate::new::generate_private_keys;
    use tempfile::tempdir;

    #[test]
    fn key_cmd_displays_key_info() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("priv_validator_key.json");

        let private_keys = generate_private_keys(1, false).unwrap();
        let priv_key = private_keys[0].clone();
        save_priv_validator_key(&key_file, &priv_key).unwrap();

        let cmd = KeyCmd::default();
        let result = cmd.run(&key_file);
        assert!(result.is_ok());
    }

    #[test]
    fn key_cmd_fails_on_missing_file() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("nonexistent.json");

        let cmd = KeyCmd::default();
        let result = cmd.run(&key_file);
        assert!(result.is_err());
    }

    #[test]
    fn key_cmd_fails_on_invalid_json() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("bad_key.json");
        std::fs::write(&key_file, "not valid json").unwrap();

        let cmd = KeyCmd::default();
        let result = cmd.run(&key_file);
        assert!(result.is_err());
    }
}
