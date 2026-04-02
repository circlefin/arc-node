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
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600) // Set permissions at creation time
            .open(path)
            .map_err(|_| Error::OpenFile(path.to_path_buf()))?
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
