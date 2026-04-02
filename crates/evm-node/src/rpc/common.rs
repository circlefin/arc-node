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

use jsonrpsee::types::{ErrorCode, ErrorObjectOwned};

pub const ARC_DEFAULT_BASE_URL: &str = "http://127.0.0.1:31000";

pub fn invalid_params(msg: impl Into<String>) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(ErrorCode::InvalidParams.code(), msg.into(), None::<()>)
}

pub mod codes {
    /// Artifact not found (e.g., certificate missing upstream).
    pub const NOT_FOUND: i32 = -32004;
    /// Upstream service unreachable (TCP connect failures).
    pub const UPSTREAM_UNREACHABLE: i32 = -32005;
}
