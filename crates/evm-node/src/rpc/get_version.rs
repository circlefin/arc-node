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

//! getVersion RPC API implementation

use jsonrpsee::core::RpcResult;
use serde::Serialize;

/// Version information returned by the RPC
#[derive(Debug, Clone, Serialize)]
pub struct RpcVersionInfo {
    /// Git version (tag or short commit hash)
    pub git_version: String,
    /// Full git commit hash
    pub git_commit: String,
    /// Short git commit hash
    pub git_short_hash: String,
    /// Cargo package version
    pub cargo_version: String,
}

/// Core logic for the `version` RPC method
pub fn rpc_get_version() -> RpcResult<RpcVersionInfo> {
    Ok(RpcVersionInfo {
        git_version: arc_version::GIT_VERSION.to_string(),
        git_commit: arc_version::GIT_COMMIT_HASH.to_string(),
        git_short_hash: arc_version::GIT_SHORT_HASH.to_string(),
        cargo_version: arc_version::SHORT_VERSION.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_get_version_returns_ok() {
        assert!(rpc_get_version().is_ok());
    }

    #[test]
    fn rpc_get_version_fields_non_empty() {
        let info = rpc_get_version().unwrap();
        assert!(!info.git_version.is_empty());
        assert!(!info.git_commit.is_empty());
        assert!(!info.git_short_hash.is_empty());
        assert!(!info.cargo_version.is_empty());
    }

    #[test]
    fn rpc_get_version_short_hash_is_prefix_of_commit() {
        let info = rpc_get_version().unwrap();
        assert!(
            info.git_commit.starts_with(&info.git_short_hash),
            "git_short_hash '{}' should be a prefix of git_commit '{}'",
            info.git_short_hash,
            info.git_commit
        );
    }

    #[test]
    fn rpc_get_version_cargo_version_contains_short_hash() {
        let info = rpc_get_version().unwrap();
        assert!(
            info.cargo_version.contains(&info.git_short_hash),
            "cargo_version '{}' should contain short hash '{}'",
            info.cargo_version,
            info.git_short_hash
        );
    }
}
