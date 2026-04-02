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

use std::collections::HashMap;

use color_eyre::eyre::Result;
use url::Url;

use crate::types::Report;

/// Validate the gossipsub mesh against expected peers.
///
/// Reports per-node tier: fully-connected, multi-hop, or
/// not-connected.
pub async fn check_mesh(
    _rpc_urls: &[(String, Url)],
    _expected_peers: &HashMap<String, Vec<String>>,
) -> Result<Report> {
    todo!()
}
