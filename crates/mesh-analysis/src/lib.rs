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

// Offline diagnostic CLI tool — not part of the node runtime.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

mod analyze;
mod fetch;
mod parse;
mod report;
mod tier;
mod types;

pub use analyze::analyze;
pub use fetch::fetch_all_metrics;
pub use parse::parse_all_metrics;
pub use report::format_report;
pub use tier::{classify_all, MeshTier};
pub use types::{
    DiscoveredPeer, MeshAnalysis, MeshDisplayOptions, MessageCounts, NodeMetricsData, NodeType,
    TopicAnalysis, ValidatorConnectivity,
};
