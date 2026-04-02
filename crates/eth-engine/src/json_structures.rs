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

use alloy_primitives::BlockTimestamp;
use arc_consensus_types::{BlockHash, BlockNumber};
use serde::{Deserialize, Serialize};

/// JSON execution block.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionBlock {
    #[serde(rename = "hash")]
    pub block_hash: BlockHash,
    #[serde(rename = "number", with = "serde_utils::u64_hex_be")]
    pub block_number: BlockNumber,
    pub parent_hash: BlockHash,
    #[serde(with = "serde_utils::u64_hex_be")]
    pub timestamp: BlockTimestamp,
}
