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

#![allow(clippy::unwrap_used)]

pub mod fetch;
pub mod health;
pub mod mempool;
pub mod mesh;
pub mod mev;
pub mod perf;
pub mod store;
pub mod types;

pub use fetch::fetch_all_metrics;
pub use health::{
    check_health_deltas, compute_health_deltas, format_health_delta_report, format_health_report,
    parse_all_health_metrics, parse_health_metrics,
};
pub use mempool::check_mempool;
pub use mesh::check_mesh;
pub use mev::check_pending_state;
pub use perf::{check_block_time, check_block_time_delta, format_perf_report, parse_perf_metrics};
pub use store::{check_store_pruning, collect_store_info, StoreInfo};
pub use types::{
    CheckResult, HistogramStats, NodeHealthData, NodeHealthDelta, NodePerfData, PerfDisplayOptions,
    Report,
};
