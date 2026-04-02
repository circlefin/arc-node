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

use crate::types::{CheckResult, NodeHealthData, NodeHealthDelta, Report};
use std::collections::HashMap;

// Metric name prefixes we need from Prometheus exposition text.
const ROUND_HISTOGRAM: &str = "malachitebft_core_consensus_consensus_round";
const HEIGHT_RESTART: &str = "arc_malachite_app_height_restart_count";
const SYNC_FELL_BEHIND: &str = "arc_malachite_app_sync_fell_behind_count";

/// Parse raw Prometheus text into health metrics for a single node.
///
/// The histogram `malachitebft_core_consensus_consensus_round` has
/// cumulative buckets — `le="0.0"` contains round-0 decisions, `le="1.0"`
/// contains round-0 *plus* round-1, and `_count` is the total. We
/// derive R0, R1, R>1 from these.
pub fn parse_health_metrics(node_name: &str, raw: &str) -> NodeHealthData {
    let mut le_0: Option<f64> = None;
    let mut le_1: Option<f64> = None;
    let mut hist_count: Option<f64> = None;
    let mut height_restarts: f64 = 0.0;
    let mut sync_fell_behind: f64 = 0.0;

    for line in raw.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        if line.starts_with(ROUND_HISTOGRAM) {
            if let Some(val) = parse_line_value(line) {
                if line.contains(r#"le="0""#) || line.contains(r#"le="0.0""#) {
                    le_0 = Some(val);
                } else if line.contains(r#"le="1""#) || line.contains(r#"le="1.0""#) {
                    le_1 = Some(val);
                } else if line.starts_with(&format!("{ROUND_HISTOGRAM}_count")) {
                    hist_count = Some(val);
                }
            }
        } else if line.starts_with(HEIGHT_RESTART) {
            if let Some(val) = parse_line_value(line) {
                height_restarts = val;
            }
        } else if line.starts_with(SYNC_FELL_BEHIND) {
            if let Some(val) = parse_line_value(line) {
                sync_fell_behind = val;
            }
        }
    }

    let total = hist_count.unwrap_or(0.0) as u64;
    let round_0 = le_0.unwrap_or(0.0) as u64;
    let round_le_1 = le_1.unwrap_or(round_0 as f64) as u64;
    let round_1 = round_le_1.saturating_sub(round_0);
    let round_gt1 = total.saturating_sub(round_le_1);

    NodeHealthData {
        name: node_name.to_string(),
        group: None,
        round_0,
        round_1,
        round_gt1,
        total_decisions: total,
        height_restarts: height_restarts as u64,
        sync_fell_behind: sync_fell_behind as u64,
    }
}

/// Parse all `(node_name, raw_text)` pairs into health data.
pub fn parse_all_health_metrics(raw_metrics: &[(String, String)]) -> Vec<NodeHealthData> {
    raw_metrics
        .iter()
        .filter(|(_, body)| !body.is_empty())
        .map(|(name, body)| parse_health_metrics(name, body))
        .collect()
}

/// Compute deltas between two ordered health snapshots.
///
/// Pairs are matched by node name. Nodes present only in one snapshot
/// are silently skipped.
pub fn compute_health_deltas(
    before: &[NodeHealthData],
    after: &[NodeHealthData],
) -> Vec<NodeHealthDelta> {
    let before_map: HashMap<&str, &NodeHealthData> =
        before.iter().map(|n| (n.name.as_str(), n)).collect();

    let mut deltas: Vec<NodeHealthDelta> = after
        .iter()
        .filter_map(|a| {
            before_map.get(a.name.as_str()).map(|b| NodeHealthDelta {
                name: a.name.clone(),
                group: a.group.clone(),
                delta_round_0: a.round_0 as i64 - b.round_0 as i64,
                delta_round_1: a.round_1 as i64 - b.round_1 as i64,
                delta_round_gt1: a.round_gt1 as i64 - b.round_gt1 as i64,
                delta_decisions: a.total_decisions as i64 - b.total_decisions as i64,
                delta_height_restarts: a.height_restarts as i64 - b.height_restarts as i64,
                delta_sync_fell_behind: a.sync_fell_behind as i64 - b.sync_fell_behind as i64,
            })
        })
        .collect();
    deltas.sort_by(|a, b| a.name.cmp(&b.name));
    deltas
}

/// Assert that no unhealthy events occurred during the observation window.
///
/// Checks each node's delta for:
/// - `delta_round_1 + delta_round_gt1 == 0`  (all decisions at round 0)
/// - `delta_height_restarts == 0`
/// - `delta_sync_fell_behind == 0`
pub fn check_health_deltas(deltas: &[NodeHealthDelta]) -> Report {
    let checks: Vec<CheckResult> = deltas
        .iter()
        .map(|d| {
            if d.delta_decisions < 0 {
                return CheckResult {
                    name: d.name.clone(),
                    passed: true,
                    message: "counter reset detected (node restart?), skipped".to_string(),
                };
            }

            let round_gt0 = d.delta_round_1 + d.delta_round_gt1;
            let mut issues: Vec<String> = Vec::new();

            if round_gt0 != 0 {
                issues.push(format!(
                    "round>0: {} (R1={}, R>1={})",
                    round_gt0, d.delta_round_1, d.delta_round_gt1
                ));
            }
            if d.delta_height_restarts != 0 {
                issues.push(format!("restarts: {}", d.delta_height_restarts));
            }
            if d.delta_sync_fell_behind != 0 {
                issues.push(format!("sync behind: {}", d.delta_sync_fell_behind));
            }

            let passed = issues.is_empty();
            let message = if passed {
                format!(
                    "{} decisions, all round 0, no restarts, no sync behind",
                    d.delta_decisions
                )
            } else {
                format!("{} decisions, {}", d.delta_decisions, issues.join(", "))
            };

            CheckResult {
                name: d.name.clone(),
                passed,
                message,
            }
        })
        .collect();

    Report { checks }
}

/// Format a human-readable health report for `quake info health` / MCP.
///
/// Nodes are grouped by `group` field (validators first). Output shows
/// round breakdown, height restart count, and sync-fell-behind count.
pub fn format_health_report(nodes: &[NodeHealthData]) -> String {
    if nodes.is_empty() {
        return "No health metrics available.\n".to_string();
    }

    let sorted = sorted_by_group(nodes);

    let name_w = sorted
        .iter()
        .map(|n| n.name.len())
        .max()
        .unwrap_or(12)
        .max(4);

    // Column widths: R0/R1/R>1 need space for "1004(100%)" = 10 chars + padding
    let rnd_w = 12;
    let tot_w = 8;
    let hr_w = 16;
    let sf_w = 16;
    // name + 2-space gap + 3 round cols + total + hr + sf (each preceded by 2-space gap)
    let line_w = name_w + 2 + rnd_w + 2 + rnd_w + 2 + rnd_w + 2 + tot_w + 2 + hr_w + 2 + sf_w;

    let mut out = String::new();

    out.push_str(&"=".repeat(line_w));
    out.push('\n');
    out.push_str(&format!("Consensus Health ({} nodes)\n", sorted.len()));
    out.push_str(&"=".repeat(line_w));
    out.push('\n');
    out.push('\n');

    out.push_str(&format!(
        "{:<name_w$}  {:^rnd_w$}  {:^rnd_w$}  {:^rnd_w$}  {:^tot_w$}  {:^hr_w$}  {:^sf_w$}\n",
        "Node", "R0", "R1", "R>1", "Total", "Height Restart", "Sync Fell Behind",
    ));
    out.push_str(&"-".repeat(line_w));
    out.push('\n');

    let mut prev_group: Option<&Option<String>> = None;

    for node in &sorted {
        if let Some(pg) = prev_group {
            if pg != &node.group {
                out.push('\n');
            }
        }
        prev_group = Some(&node.group);

        let pct = |count: u64| -> f64 {
            if node.total_decisions > 0 {
                count as f64 / node.total_decisions as f64 * 100.0
            } else {
                0.0
            }
        };

        out.push_str(&format!(
            "{:<name_w$}  {:^rnd_w$}  {:^rnd_w$}  {:^rnd_w$}  {:^tot_w$}  {:^hr_w$}  {:^sf_w$}\n",
            node.name,
            format!("{}({:.0}%)", node.round_0, pct(node.round_0)),
            format!("{}({:.0}%)", node.round_1, pct(node.round_1)),
            format!("{}({:.0}%)", node.round_gt1, pct(node.round_gt1)),
            node.total_decisions,
            node.height_restarts,
            node.sync_fell_behind,
        ));
    }

    out.push_str(&"-".repeat(line_w));
    out.push('\n');
    out
}

/// Format the delta report printed after `quake test health:stability`.
pub fn format_health_delta_report(deltas: &[NodeHealthDelta]) -> String {
    if deltas.is_empty() {
        return "No delta data available.\n".to_string();
    }

    let name_w = deltas
        .iter()
        .map(|d| d.name.len())
        .max()
        .unwrap_or(12)
        .max(4);

    let dec_w = 12;
    let rgt0_w = 12;
    let hr_w = 16;
    let sf_w = 18;
    let line_w = name_w + 2 + dec_w + 2 + rgt0_w + 2 + hr_w + 2 + sf_w;

    let mut sorted: Vec<&NodeHealthDelta> = deltas.iter().collect();
    sorted.sort_by(|a, b| {
        crate::types::group_order(a.group.as_deref())
            .cmp(&crate::types::group_order(b.group.as_deref()))
            .then_with(|| a.name.cmp(&b.name))
    });

    let mut out = String::new();
    out.push_str(&format!(
        "{:<name_w$}  {:^dec_w$}  {:^rgt0_w$}  {:^hr_w$}  {:^sf_w$}\n",
        "Node", "Decisions", "Round > 0", "Height Restart", "Sync Fell Behind",
    ));
    out.push_str(&"-".repeat(line_w));
    out.push('\n');

    for d in &sorted {
        out.push_str(&format!(
            "{:<name_w$}  {:^dec_w$}  {:^rgt0_w$}  {:^hr_w$}  {:^sf_w$}\n",
            d.name,
            d.delta_decisions,
            d.delta_round_1 + d.delta_round_gt1,
            d.delta_height_restarts,
            d.delta_sync_fell_behind,
        ));
    }

    out
}

/// Sort nodes: validators first, then non-validators, alphabetical within each group.
fn sorted_by_group(nodes: &[NodeHealthData]) -> Vec<&NodeHealthData> {
    let mut sorted: Vec<&NodeHealthData> = nodes.iter().collect();
    sorted.sort_by(|a, b| {
        crate::types::group_order(a.group.as_deref())
            .cmp(&crate::types::group_order(b.group.as_deref()))
            .then_with(|| a.name.cmp(&b.name))
    });
    sorted
}

/// Extract the numeric value from a Prometheus exposition line.
///
/// Lines look like:
///   `metric_name{labels} 42.0`
///   `metric_name_total 7`
fn parse_line_value(line: &str) -> Option<f64> {
    line.rsplit_once(' ')
        .and_then(|(_, val)| val.parse::<f64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_prometheus_text() -> &'static str {
        r#"# HELP malachitebft_core_consensus_consensus_round Round at which consensus was reached
# TYPE malachitebft_core_consensus_consensus_round histogram
malachitebft_core_consensus_consensus_round_bucket{le="0"} 95
malachitebft_core_consensus_consensus_round_bucket{le="1"} 98
malachitebft_core_consensus_consensus_round_bucket{le="2"} 100
malachitebft_core_consensus_consensus_round_bucket{le="5"} 100
malachitebft_core_consensus_consensus_round_bucket{le="+Inf"} 100
malachitebft_core_consensus_consensus_round_sum 7
malachitebft_core_consensus_consensus_round_count 100
# HELP arc_malachite_app_height_restart_count The number of times the consensus height has been restarted
# TYPE arc_malachite_app_height_restart_count counter
arc_malachite_app_height_restart_count_total 2
# HELP arc_malachite_app_sync_fell_behind_count Number of times the node fell behind
# TYPE arc_malachite_app_sync_fell_behind_count counter
arc_malachite_app_sync_fell_behind_count_total 1
"#
    }

    #[test]
    fn parse_health_round_breakdown() {
        let data = parse_health_metrics("validator1", sample_prometheus_text());
        assert_eq!(data.round_0, 95);
        assert_eq!(data.round_1, 3);
        assert_eq!(data.round_gt1, 2);
        assert_eq!(data.total_decisions, 100);
    }

    #[test]
    fn parse_health_counters() {
        let data = parse_health_metrics("validator1", sample_prometheus_text());
        assert_eq!(data.height_restarts, 2);
        assert_eq!(data.sync_fell_behind, 1);
    }

    #[test]
    fn parse_empty_text_yields_zeros() {
        let data = parse_health_metrics("empty", "");
        assert_eq!(data.total_decisions, 0);
        assert_eq!(data.round_0, 0);
        assert_eq!(data.height_restarts, 0);
        assert_eq!(data.sync_fell_behind, 0);
    }

    #[test]
    fn compute_deltas_basic() {
        let before = vec![NodeHealthData {
            name: "v1".into(),
            group: None,
            round_0: 50,
            round_1: 2,
            round_gt1: 0,
            total_decisions: 52,
            height_restarts: 1,
            sync_fell_behind: 0,
        }];

        let after = vec![NodeHealthData {
            name: "v1".into(),
            group: None,
            round_0: 100,
            round_1: 3,
            round_gt1: 1,
            total_decisions: 104,
            height_restarts: 1,
            sync_fell_behind: 0,
        }];

        let deltas = compute_health_deltas(&before, &after);
        assert_eq!(deltas.len(), 1);
        let d = &deltas[0];
        assert_eq!(d.delta_round_0, 50); // 100 - 50
        assert_eq!(d.delta_round_1 + d.delta_round_gt1, 2); // (3+1) - (2+0) = 2
        assert_eq!(d.delta_round_1, 1);
        assert_eq!(d.delta_round_gt1, 1);
        assert_eq!(d.delta_decisions, 52);
        assert_eq!(d.delta_height_restarts, 0);
        assert_eq!(d.delta_sync_fell_behind, 0);
    }

    #[test]
    fn compute_deltas_missing_node_skipped() {
        let before = vec![NodeHealthData {
            name: "v1".into(),
            group: None,
            round_0: 10,
            round_1: 0,
            round_gt1: 0,
            total_decisions: 10,
            height_restarts: 0,
            sync_fell_behind: 0,
        }];

        let after = vec![NodeHealthData {
            name: "v2".into(),
            group: None,
            round_0: 20,
            round_1: 0,
            round_gt1: 0,
            total_decisions: 20,
            height_restarts: 0,
            sync_fell_behind: 0,
        }];

        let deltas = compute_health_deltas(&before, &after);
        assert!(deltas.is_empty());
    }

    #[test]
    fn compute_deltas_multiple_nodes() {
        let before = vec![
            NodeHealthData {
                name: "v1".into(),
                group: None,
                round_0: 10,
                round_1: 1,
                round_gt1: 0,
                total_decisions: 11,
                height_restarts: 0,
                sync_fell_behind: 0,
            },
            NodeHealthData {
                name: "v2".into(),
                group: None,
                round_0: 20,
                round_1: 0,
                round_gt1: 0,
                total_decisions: 20,
                height_restarts: 1,
                sync_fell_behind: 0,
            },
        ];

        let after = vec![
            NodeHealthData {
                name: "v2".into(),
                group: None,
                round_0: 40,
                round_1: 2,
                round_gt1: 0,
                total_decisions: 42,
                height_restarts: 1,
                sync_fell_behind: 0,
            },
            NodeHealthData {
                name: "v1".into(),
                group: None,
                round_0: 30,
                round_1: 1,
                round_gt1: 0,
                total_decisions: 31,
                height_restarts: 0,
                sync_fell_behind: 0,
            },
        ];

        let deltas = compute_health_deltas(&before, &after);
        assert_eq!(deltas.len(), 2);

        let d1 = deltas.iter().find(|d| d.name == "v1").unwrap();
        assert_eq!(d1.delta_round_0, 20);
        assert_eq!(d1.delta_decisions, 20);

        let d2 = deltas.iter().find(|d| d.name == "v2").unwrap();
        assert_eq!(d2.delta_round_0, 20);
        assert_eq!(d2.delta_round_1, 2);
        assert_eq!(d2.delta_decisions, 22);
        assert_eq!(d2.delta_height_restarts, 0);
    }

    #[test]
    fn compute_deltas_counter_reset() {
        let before = vec![NodeHealthData {
            name: "v1".into(),
            group: None,
            round_0: 100,
            round_1: 5,
            round_gt1: 2,
            total_decisions: 107,
            height_restarts: 3,
            sync_fell_behind: 1,
        }];

        let after = vec![NodeHealthData {
            name: "v1".into(),
            group: None,
            round_0: 10,
            round_1: 0,
            round_gt1: 0,
            total_decisions: 10,
            height_restarts: 0,
            sync_fell_behind: 0,
        }];

        let deltas = compute_health_deltas(&before, &after);
        assert_eq!(deltas.len(), 1);
        let d = &deltas[0];
        assert!(
            d.delta_decisions < 0,
            "negative delta indicates counter reset"
        );
        assert!(d.delta_round_0 < 0);
        assert!(d.delta_height_restarts < 0);
    }

    #[test]
    fn check_deltas_all_healthy() {
        let deltas = vec![NodeHealthDelta {
            name: "v1".into(),
            group: None,
            delta_round_0: 50,
            delta_round_1: 0,
            delta_round_gt1: 0,
            delta_decisions: 50,
            delta_height_restarts: 0,
            delta_sync_fell_behind: 0,
        }];

        let report = check_health_deltas(&deltas);
        assert!(report.passed());
    }

    #[test]
    fn check_deltas_round_gt0_fails() {
        let deltas = vec![NodeHealthDelta {
            name: "v1".into(),
            group: None,
            delta_round_0: 47,
            delta_round_1: 2,
            delta_round_gt1: 1,
            delta_decisions: 50,
            delta_height_restarts: 0,
            delta_sync_fell_behind: 0,
        }];

        let report = check_health_deltas(&deltas);
        assert!(!report.passed());
    }

    #[test]
    fn check_deltas_height_restart_fails() {
        let deltas = vec![NodeHealthDelta {
            name: "v1".into(),
            group: None,
            delta_round_0: 50,
            delta_round_1: 0,
            delta_round_gt1: 0,
            delta_decisions: 50,
            delta_height_restarts: 1,
            delta_sync_fell_behind: 0,
        }];

        let report = check_health_deltas(&deltas);
        assert!(!report.passed());
    }

    #[test]
    fn check_deltas_sync_fell_behind_fails() {
        let deltas = vec![NodeHealthDelta {
            name: "v1".into(),
            group: None,
            delta_round_0: 50,
            delta_round_1: 0,
            delta_round_gt1: 0,
            delta_decisions: 50,
            delta_height_restarts: 0,
            delta_sync_fell_behind: 2,
        }];

        let report = check_health_deltas(&deltas);
        assert!(!report.passed());
    }

    #[test]
    fn format_health_report_renders() {
        let nodes = vec![
            NodeHealthData {
                name: "validator1".into(),
                group: Some("Validators".into()),
                round_0: 95,
                round_1: 3,
                round_gt1: 2,
                total_decisions: 100,
                height_restarts: 0,
                sync_fell_behind: 0,
            },
            NodeHealthData {
                name: "sentry1".into(),
                group: Some("Non-Validators".into()),
                round_0: 90,
                round_1: 5,
                round_gt1: 5,
                total_decisions: 100,
                height_restarts: 1,
                sync_fell_behind: 0,
            },
        ];

        let report = format_health_report(&nodes);
        assert!(report.contains("validator1"));
        assert!(report.contains("sentry1"));
        assert!(report.contains("Consensus Health"));
        assert!(report.contains("R0"));
        assert!(report.contains("R>1"));
    }

    #[test]
    fn format_health_report_empty() {
        let report = format_health_report(&[]);
        assert!(report.contains("No health metrics"));
    }

    #[test]
    fn parse_all_skips_empty_bodies() {
        let data = vec![
            ("v1".into(), sample_prometheus_text().into()),
            ("v2".into(), String::new()),
        ];
        let results = parse_all_health_metrics(&data);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "v1");
    }

    #[test]
    fn parse_health_all_round_zero() {
        let raw = r#"# TYPE malachitebft_core_consensus_consensus_round histogram
malachitebft_core_consensus_consensus_round_bucket{le="0"} 100
malachitebft_core_consensus_consensus_round_bucket{le="1"} 100
malachitebft_core_consensus_consensus_round_bucket{le="+Inf"} 100
malachitebft_core_consensus_consensus_round_count 100
"#;
        let data = parse_health_metrics("v1", raw);
        assert_eq!(data.round_0, 100);
        assert_eq!(data.round_1, 0);
        assert_eq!(data.round_gt1, 0);
        assert_eq!(data.total_decisions, 100);
    }

    #[test]
    fn parse_health_only_counters_no_histogram() {
        let raw = r#"# TYPE arc_malachite_app_height_restart_count counter
arc_malachite_app_height_restart_count_total 5
# TYPE arc_malachite_app_sync_fell_behind_count counter
arc_malachite_app_sync_fell_behind_count_total 3
"#;
        let data = parse_health_metrics("v1", raw);
        assert_eq!(data.total_decisions, 0);
        assert_eq!(data.round_0, 0);
        assert_eq!(data.height_restarts, 5);
        assert_eq!(data.sync_fell_behind, 3);
    }

    #[test]
    fn parse_health_only_histogram_no_counters() {
        let raw = r#"# TYPE malachitebft_core_consensus_consensus_round histogram
malachitebft_core_consensus_consensus_round_bucket{le="0"} 50
malachitebft_core_consensus_consensus_round_bucket{le="1"} 60
malachitebft_core_consensus_consensus_round_bucket{le="+Inf"} 70
malachitebft_core_consensus_consensus_round_count 70
"#;
        let data = parse_health_metrics("v1", raw);
        assert_eq!(data.round_0, 50);
        assert_eq!(data.round_1, 10);
        assert_eq!(data.round_gt1, 10);
        assert_eq!(data.total_decisions, 70);
        assert_eq!(data.height_restarts, 0);
        assert_eq!(data.sync_fell_behind, 0);
    }

    #[test]
    fn parse_health_ignores_unrelated_metrics() {
        let raw = r#"# TYPE go_gc_duration_seconds summary
go_gc_duration_seconds{quantile="0.5"} 0.000123
# TYPE grpc_server_handled_total counter
grpc_server_handled_total{method="Propose"} 500
# TYPE malachitebft_core_consensus_consensus_round histogram
malachitebft_core_consensus_consensus_round_bucket{le="0"} 40
malachitebft_core_consensus_consensus_round_bucket{le="1"} 40
malachitebft_core_consensus_consensus_round_bucket{le="+Inf"} 40
malachitebft_core_consensus_consensus_round_count 40
arc_malachite_app_height_restart_count_total 0
"#;
        let data = parse_health_metrics("v1", raw);
        assert_eq!(data.round_0, 40);
        assert_eq!(data.round_1, 0);
        assert_eq!(data.round_gt1, 0);
        assert_eq!(data.total_decisions, 40);
        assert_eq!(data.height_restarts, 0);
    }

    #[test]
    fn parse_health_decimal_le_format() {
        let raw = r#"# TYPE malachitebft_core_consensus_consensus_round histogram
malachitebft_core_consensus_consensus_round_bucket{le="0.0"} 80
malachitebft_core_consensus_consensus_round_bucket{le="1.0"} 90
malachitebft_core_consensus_consensus_round_bucket{le="2.0"} 100
malachitebft_core_consensus_consensus_round_bucket{le="+Inf"} 100
malachitebft_core_consensus_consensus_round_count 100
"#;
        let data = parse_health_metrics("v1", raw);
        assert_eq!(data.round_0, 80);
        assert_eq!(data.round_1, 10);
        assert_eq!(data.round_gt1, 10);
        assert_eq!(data.total_decisions, 100);
    }
}
