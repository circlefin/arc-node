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

//! File save functions

use std::fs;
use std::path::Path;

use arc_consensus_types::signing::PrivateKey;

use crate::error::Error;

/// Save private_key validator key to file
pub fn save_priv_validator_key(
    priv_validator_key_file: &Path,
    priv_validator_key: &PrivateKey,
) -> Result<(), Error> {
    save(
        priv_validator_key_file,
        &serde_json::to_string_pretty(priv_validator_key)
            .map_err(|e| Error::ToJSON(e.to_string()))?,
    )
}

fn save(path: &Path, data: &str) -> Result<(), Error> {
    use std::io::Write;

    if let Some(parent_dir) = path.parent() {
        fs::create_dir_all(parent_dir).map_err(|_| Error::ParentDir(parent_dir.to_path_buf()))?;
    }

    // Create file with secure permissions (0600) on Unix systems
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600) // Set permissions at creation time
            .open(path)
            .map_err(|_| Error::OpenFile(path.to_path_buf()))?;
        // `mode()` above applies only when the file is created. If the path already
        // exists with looser permissions, it is silently ignored and the private key
        // would be written into a world-readable file. Enforce 0600 explicitly so an
        // existing file is tightened before the key is written to it.
        f.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| Error::OpenFile(path.to_path_buf()))?;
        f
    };

    #[cfg(not(unix))]
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|_| Error::OpenFile(path.to_path_buf()))?;

    f.write_all(data.as_bytes())
        .map_err(|_| Error::WriteFile(path.to_path_buf()))?;

    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use arc_consensus_types::signing::PrivateKey;
    use rand::rngs::OsRng;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn save_priv_validator_key_creates_file_with_0600() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("priv_validator_key.json");

        save_priv_validator_key(&key_file, &PrivateKey::generate(OsRng)).unwrap();

        assert_eq!(mode_of(&key_file), 0o600);
    }

    #[test]
    fn save_priv_validator_key_tightens_existing_loose_permissions() {
        let dir = tempdir().unwrap();
        let key_file = dir.path().join("priv_validator_key.json");

        // Simulate a key file left world-readable by a backup restore or an
        // older version. `OpenOptions::mode()` is ignored for existing files,
        // so without an explicit set_permissions the key would be written into
        // this 0644 file.
        fs::write(&key_file, "{}").unwrap();
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode_of(&key_file), 0o644);

        save_priv_validator_key(&key_file, &PrivateKey::generate(OsRng)).unwrap();

        assert_eq!(
            mode_of(&key_file),
            0o600,
            "existing file should be tightened to 0600 before the key is written"
        );
    }
}
