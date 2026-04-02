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

/// Aggregated result of running one or more checks.
pub struct Report {
    pub checks: Vec<CheckResult>,
}

impl Report {
    /// Returns true if every individual check passed.
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }
}

/// A single check outcome for one node or component.
pub struct CheckResult {
    /// Node or component name (e.g. "validator0").
    pub name: String,
    /// Whether this individual check passed.
    pub passed: bool,
    /// Human-readable detail.
    pub message: String,
}

/// Statistics extracted from a Prometheus histogram.
///
/// Percentiles are estimated via linear interpolation between bucket
/// boundaries (same algorithm as Prometheus `histogram_quantile()`).
/// Values are cumulative since process start, not windowed.
#[derive(Debug, Clone, Default)]
pub struct HistogramStats {
    pub count: u64,
    pub sum: f64,
    pub avg: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    /// Highest bucket boundary that contained new observations.
    /// When `exceeded_count > 0`, this is the highest *defined* boundary.
    pub max_bucket: f64,
    /// Number of observations beyond the highest defined bucket.
    /// When non-zero, the true max is unknown — display as `>max_bucket`.
    pub exceeded_count: u64,
}

/// Per-node performance data parsed from Prometheus metrics.
#[derive(Debug, Clone)]
pub struct NodePerfData {
    pub name: String,
    /// Optional grouping label (e.g. "Validators", "Non-Validators").
    /// When set, `format_perf_report` groups and sorts nodes by this field.
    pub group: Option<String>,
    // Latency histograms (seconds)
    pub block_time: Option<HistogramStats>,
    pub block_finalize_time: Option<HistogramStats>,
    pub block_build_time: Option<HistogramStats>,
    pub consensus_time: Option<HistogramStats>,
    // Throughput histograms (per block)
    pub block_tx_count: Option<HistogramStats>,
    pub block_size: Option<HistogramStats>,
    pub block_gas_used: Option<HistogramStats>,
}

/// Controls which sections `format_perf_report` includes.
#[derive(Debug, Clone)]
pub struct PerfDisplayOptions {
    pub show_latency: bool,
    pub show_throughput: bool,
    pub show_summary: bool,
}

impl Default for PerfDisplayOptions {
    fn default() -> Self {
        Self {
            show_latency: true,
            show_throughput: true,
            show_summary: true,
        }
    }
}

// ── Health types ────────────────────────────────────────────────────────

/// Health metrics for a single node from a single Prometheus scrape.
///
/// Derived from `malachitebft_core_consensus_consensus_round` histogram
/// buckets and `arc_malachite_app_*` counters.
#[derive(Debug, Clone)]
pub struct NodeHealthData {
    pub name: String,
    /// Optional grouping label (e.g. "Validators", "Non-Validators").
    pub group: Option<String>,
    /// Decisions completed in round 0 (optimal).
    pub round_0: u64,
    /// Decisions that needed exactly one retry (round 1).
    pub round_1: u64,
    /// Decisions that needed more than one retry (round > 1).
    pub round_gt1: u64,
    /// Total consensus decisions recorded.
    pub total_decisions: u64,
    /// Cumulative height restart count since process start.
    pub height_restarts: u64,
    /// Cumulative sync-fell-behind count since process start.
    pub sync_fell_behind: u64,
}

/// Delta between two health scrapes for a single node.
#[derive(Debug, Clone)]
pub struct NodeHealthDelta {
    pub name: String,
    pub group: Option<String>,
    /// New round 0 decisions during the observation window.
    pub delta_round_0: i64,
    /// New round 1 decisions.
    pub delta_round_1: i64,
    /// New round > 1 decisions.
    pub delta_round_gt1: i64,
    /// New decisions total.
    pub delta_decisions: i64,
    /// New height restarts during the observation window.
    pub delta_height_restarts: i64,
    /// New sync-fell-behind events during the observation window.
    pub delta_sync_fell_behind: i64,
}

/// Priority ordering for node groups: Validators first, then Non-Validators, then ungrouped.
pub fn group_order(g: Option<&str>) -> u8 {
    match g {
        Some("Validators") => 0,
        Some("Non-Validators") => 1,
        _ => 2,
    }
}
