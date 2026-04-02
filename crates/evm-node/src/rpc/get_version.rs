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
