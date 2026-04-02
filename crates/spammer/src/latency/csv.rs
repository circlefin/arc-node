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

/// Block metadata needed for a CSV row.
#[derive(Debug, Clone)]
pub(super) struct BlockInfo {
    /// Block height.
    pub height: u64,
    /// Block hash.
    pub hash: String,
    /// Block timestamp in RFC 3339 UTC.
    pub timestamp: String,
}

/// One CSV row describing a finalized transaction.
#[derive(Debug, Clone)]
pub(super) struct CsvRow {
    /// Transaction hash as a 0x-prefixed hex string.
    pub tx_hash: String,
    /// Submit time in RFC 3339 UTC.
    pub submitted_at: String,
    /// Observation time in RFC 3339 UTC.
    pub finalized_observed_at: String,
    /// Block information for the finalized inclusion.
    pub block: BlockInfo,
}

impl CsvRow {
    /// Create a new CSV row from transaction and block metadata.
    pub fn new(
        tx_hash: String,
        submitted_at: String,
        finalized_observed_at: String,
        block: BlockInfo,
    ) -> Self {
        Self {
            tx_hash,
            submitted_at,
            finalized_observed_at,
            block,
        }
    }
}
