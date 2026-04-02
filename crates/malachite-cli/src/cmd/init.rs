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

//! Init command
//!
//! This command generates a private validator key file for the node.
//! Configuration is now provided via CLI flags instead of a config file.

use std::path::Path;

use clap::Parser;
use tracing::{info, warn};

use arc_consensus_types::Address;

use crate::error::Error;
use crate::file::save_priv_validator_key;
use crate::new::generate_private_keys;

#[derive(Parser, Debug, Clone, Default, PartialEq)]
pub struct InitCmd {
    /// Overwrite existing private key file
    #[clap(long)]
    pub overwrite: bool,
}

impl InitCmd {
    /// Execute the init command
    ///
    /// This generates only the private validator key file.
    /// Configuration is now provided via CLI flags when starting the node.
    pub fn run(&self, priv_validator_key_file: &Path) -> Result<(), Error> {
        init(priv_validator_key_file, self.overwrite)
    }
}

/// init command to generate the private validator key.
pub fn init(priv_validator_key_file: &Path, overwrite: bool) -> Result<(), Error> {
    // Save default priv_validator_key
    if priv_validator_key_file.exists() && !overwrite {
        warn!(
            file = %priv_validator_key_file.display(),
            "Private key file already exists, skipping. Use --overwrite to replace it.",
        );

        return Ok(());
    }

    info!(file = %priv_validator_key_file.display(), "Generating private validator key");

    let private_keys = generate_private_keys(1, false)?;
    let priv_validator_key = private_keys[0].clone();
    save_priv_validator_key(priv_validator_key_file, &priv_validator_key)?;

    info!(file = %priv_validator_key_file.display(), "Private validator key generated successfully");

    let public_key = priv_validator_key.public_key();

    info!(
        address = %Address::from_public_key(&public_key),
        public_key = %format!("0x{}", hex::encode(public_key.as_bytes())),
        "Key information",
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Assert that a file has secure permissions (0600 - read/write for owner only) on Unix systems
    #[cfg(unix)]
    fn assert_file_permissions_secure(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path).unwrap();
        let permissions = metadata.permissions();
        assert_eq!(
            permissions.mode() & 0o777,
            0o600,
            "File permissions should be 0600 (read/write for owner only)"
        );
    }

    #[test]
    fn init_generates_private_key_file() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("priv_validator_key.json");

        let result = init(&key_file, false);
        assert!(result.is_ok());
        assert!(key_file.exists());

        // Verify the file contains valid JSON
        let contents = fs::read_to_string(&key_file).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&contents).is_ok());

        // Verify file permissions on Unix systems
        #[cfg(unix)]
        assert_file_permissions_secure(&key_file);
    }

    #[test]
    fn init_with_overwrite_replaces_existing_key() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("priv_validator_key.json");

        // Create initial key
        init(&key_file, false).unwrap();
        let original_contents = fs::read_to_string(&key_file).unwrap();

        // Overwrite with new key
        init(&key_file, true).unwrap();
        let new_contents = fs::read_to_string(&key_file).unwrap();

        // Keys should be different (different random generation)
        assert_ne!(original_contents, new_contents);
    }

    #[test]
    fn init_without_overwrite_skips_existing_key() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("priv_validator_key.json");

        // Create initial key
        init(&key_file, false).unwrap();
        let original_contents = fs::read_to_string(&key_file).unwrap();

        // Try to init again without overwrite
        init(&key_file, false).unwrap();
        let contents_after = fs::read_to_string(&key_file).unwrap();

        // Contents should be unchanged
        assert_eq!(original_contents, contents_after);
    }

    #[test]
    fn init_cmd_run_generates_key() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("priv_validator_key.json");

        let cmd = InitCmd { overwrite: false };
        let result = cmd.run(&key_file);

        assert!(result.is_ok());
        assert!(key_file.exists());
    }

    #[test]
    fn init_cmd_with_overwrite_flag() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("priv_validator_key.json");

        // Create initial key
        let cmd = InitCmd { overwrite: false };
        cmd.run(&key_file).unwrap();
        let original_contents = fs::read_to_string(&key_file).unwrap();

        // Overwrite
        let cmd = InitCmd { overwrite: true };
        cmd.run(&key_file).unwrap();
        let new_contents = fs::read_to_string(&key_file).unwrap();

        assert_ne!(original_contents, new_contents);
    }

    #[test]
    fn init_creates_parent_directories() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("config").join("priv_validator_key.json");

        // Parent directory doesn't exist yet
        assert!(!key_file.parent().unwrap().exists());

        let result = init(&key_file, false);

        // Should create parent directories
        assert!(result.is_ok());
        assert!(key_file.exists());
        assert!(key_file.parent().unwrap().exists());
    }

    #[test]
    fn init_cmd_default_has_overwrite_false() {
        let cmd = InitCmd::default();
        assert!(!cmd.overwrite);
    }

    #[test]
    fn init_cmd_parses_overwrite_flag() {
        let args = vec!["init", "--overwrite"];
        let cmd = InitCmd::try_parse_from(args).unwrap();
        assert!(cmd.overwrite);
    }

    #[test]
    fn init_cmd_without_overwrite_flag() {
        let args = vec!["init"];
        let cmd = InitCmd::try_parse_from(args).unwrap();
        assert!(!cmd.overwrite);
    }
}
