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

use std::fmt::Write;

use color_eyre::eyre::Result;
use prometheus_parse::{Sample, Scrape, Value};
use url::Url;

use crate::fetch::fetch_all_metrics;
use crate::types::{CheckResult, HistogramStats, NodePerfData, PerfDisplayOptions, Report};

/// Only lines starting with these prefixes (plus their `# TYPE`/`# HELP`
/// comments) are kept from the raw scrape. Everything else is dropped before
/// parsing so we don't allocate `Sample` objects for unrelated metrics.
const METRIC_PREFIXES: &[&str] = &[
    "arc_malachite_app_block_time",
    "arc_malachite_app_block_finalize_time",
    "arc_malachite_app_block_build_time",
    "arc_malachite_app_block_transactions_count",
    "arc_malachite_app_block_size_bytes",
    "arc_malachite_app_block_gas_used",
    "malachitebft_core_consensus_consensus_time",
];

// ── Parsing ──────────────────────────────────────────────────────────────

/// Parse raw Prometheus text, keeping only the metrics we care about.
///
/// Prometheus text format uses `# TYPE` / `# HELP` comment lines as metadata;
/// `prometheus-parse` needs them for correct type inference, so we keep those
/// whose metric name (3rd whitespace token) matches our prefixes alongside the
/// actual data lines.
fn parse_metrics(raw: &str) -> Vec<Sample> {
    let filtered: String = raw
        .lines()
        .filter(|line| {
            let metric_name = if line.starts_with('#') {
                line.split_whitespace().nth(2).unwrap_or("")
            } else {
                line
            };
            METRIC_PREFIXES.iter().any(|p| metric_name.starts_with(p))
        })
        .collect::<Vec<_>>()
        .join("\n");

    let lines = filtered.lines().map(|l| Ok(l.to_owned()));
    Scrape::parse(lines)
        .map(|scrape| scrape.samples)
        .unwrap_or_default()
}

/// Extract the `moniker` label from the first sample that has one.
fn extract_moniker(samples: &[Sample]) -> Option<String> {
    samples
        .iter()
        .find_map(|s| s.labels.get("moniker").map(|m| m.to_string()))
}

/// Extract a gauge/counter value as f64.
fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Gauge(f) | Value::Counter(f) | Value::Untyped(f) => Some(*f),
        _ => None,
    }
}

/// Find the first sample matching `metric` and return its value as f64.
fn extract_gauge(samples: &[Sample], metric: &str) -> Option<f64> {
    samples
        .iter()
        .find(|s| s.metric == metric)
        .and_then(|s| value_as_f64(&s.value))
}

/// Estimate a percentile from cumulative histogram buckets using linear
/// interpolation. This is the same algorithm Prometheus uses for
/// `histogram_quantile()`.
fn estimate_percentile(buckets: &[(f64, f64)], total_count: f64, quantile: f64) -> f64 {
    let target = total_count * quantile;
    let mut prev_le: f64 = 0.0;
    let mut prev_count: f64 = 0.0;

    for &(le, count) in buckets {
        if le.is_infinite() {
            continue;
        }
        if count >= target {
            if (count - prev_count).abs() < f64::EPSILON {
                return le;
            }
            let ratio = (target - prev_count) / (count - prev_count);
            return prev_le + ratio * (le - prev_le);
        }
        prev_le = le;
        prev_count = count;
    }

    buckets
        .iter()
        .filter(|(le, _)| !le.is_infinite())
        .next_back()
        .map(|(le, _)| *le)
        .unwrap_or(0.0)
}

/// Raw cumulative histogram extracted from a Prometheus scrape.
///
/// Preserves the raw bucket data so two scrapes can be subtracted to isolate
/// an observation window (delta histogram).
#[derive(Debug, Clone)]
struct RawHistogram {
    /// Sorted `(le, cumulative_count)` pairs, excluding `+Inf`.
    buckets: Vec<(f64, f64)>,
    /// The `+Inf` bucket count (= total observations).
    inf_count: f64,
    /// The `_sum` counter value.
    sum: f64,
}

/// Extract raw histogram bucket data from parsed samples.
fn extract_raw_histogram(samples: &[Sample], metric: &str) -> Option<RawHistogram> {
    let sample = samples.iter().find(|s| s.metric == metric)?;
    let histogram = match &sample.value {
        Value::Histogram(buckets) => buckets,
        _ => return None,
    };

    if histogram.is_empty() {
        return None;
    }

    let mut buckets: Vec<(f64, f64)> = Vec::new();
    let mut inf_count: f64 = 0.0;

    for hc in histogram {
        if hc.less_than.is_infinite() {
            inf_count = hc.count;
        } else {
            buckets.push((hc.less_than, hc.count));
        }
    }

    buckets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let sum = extract_gauge(samples, &format!("{metric}_sum")).unwrap_or(0.0);

    Some(RawHistogram {
        buckets,
        inf_count,
        sum,
    })
}

/// Compute `HistogramStats` from raw histogram bucket data.
fn stats_from_raw(raw: &RawHistogram) -> Option<HistogramStats> {
    let total_count = raw.inf_count;
    if total_count == 0.0 {
        return None;
    }

    let count_u64 = total_count as u64;
    let avg = if count_u64 > 0 {
        raw.sum / total_count
    } else {
        0.0
    };

    let last_bucket_count = raw.buckets.last().map(|(_, c)| *c).unwrap_or(0.0);
    let highest_le = raw.buckets.last().map(|(le, _)| *le).unwrap_or(0.0);

    let (exceeded_count, max_bucket) = if raw.inf_count > last_bucket_count {
        ((raw.inf_count - last_bucket_count) as u64, highest_le)
    } else {
        let mut max_with_data: f64 = 0.0;
        let mut prev_c: f64 = 0.0;
        for &(le, count) in &raw.buckets {
            if count > prev_c {
                max_with_data = le;
            }
            prev_c = count;
        }
        (
            0,
            if max_with_data > 0.0 {
                max_with_data
            } else {
                highest_le
            },
        )
    };

    let p50 = estimate_percentile(&raw.buckets, total_count, 0.5);
    let p95 = estimate_percentile(&raw.buckets, total_count, 0.95);
    let p99 = estimate_percentile(&raw.buckets, total_count, 0.99);

    Some(HistogramStats {
        count: count_u64,
        sum: raw.sum,
        avg,
        p50,
        p95,
        p99,
        max_bucket,
        exceeded_count,
    })
}

/// Build `HistogramStats` from a prometheus-parse `Value::Histogram`.
fn histogram_stats(samples: &[Sample], metric: &str) -> Option<HistogramStats> {
    let raw = extract_raw_histogram(samples, metric)?;
    stats_from_raw(&raw)
}

/// Subtract two cumulative histograms to isolate the observation window.
///
/// Bucket boundaries must match (same Prometheus config). Returns `None` if
/// the delta has zero observations or if a counter reset is detected.
fn subtract_raw_histograms(before: &RawHistogram, after: &RawHistogram) -> Option<RawHistogram> {
    let delta_inf = after.inf_count - before.inf_count;
    if delta_inf <= 0.0 {
        return None;
    }

    let delta_sum = after.sum - before.sum;
    if delta_sum < 0.0 {
        return None;
    }

    if before.buckets.len() != after.buckets.len() {
        return None;
    }

    let delta_buckets: Vec<(f64, f64)> = after
        .buckets
        .iter()
        .zip(before.buckets.iter())
        .map(|((le, count_after), (_, count_before))| (*le, (count_after - count_before).max(0.0)))
        .collect();

    Some(RawHistogram {
        buckets: delta_buckets,
        inf_count: delta_inf,
        sum: delta_sum,
    })
}

/// Parse raw Prometheus metrics text from multiple nodes into structured
/// performance data. Values are cumulative since each node's process start.
pub fn parse_perf_metrics(raw_metrics: &[(String, String)]) -> Vec<NodePerfData> {
    raw_metrics
        .iter()
        .filter(|(_, m)| !m.is_empty())
        .map(|(name, raw)| {
            let samples = parse_metrics(raw);
            let display_name = extract_moniker(&samples).unwrap_or_else(|| name.clone());

            NodePerfData {
                name: display_name,
                group: None,
                block_time: histogram_stats(&samples, "arc_malachite_app_block_time"),
                block_finalize_time: histogram_stats(
                    &samples,
                    "arc_malachite_app_block_finalize_time",
                ),
                block_build_time: histogram_stats(&samples, "arc_malachite_app_block_build_time"),
                consensus_time: histogram_stats(
                    &samples,
                    "malachitebft_core_consensus_consensus_time",
                ),
                block_tx_count: histogram_stats(
                    &samples,
                    "arc_malachite_app_block_transactions_count",
                ),
                block_size: histogram_stats(&samples, "arc_malachite_app_block_size_bytes"),
                block_gas_used: histogram_stats(&samples, "arc_malachite_app_block_gas_used"),
            }
        })
        .collect()
}

// ── Formatting ───────────────────────────────────────────────────────────

/// Format a histogram stat value with the given decimal precision.
fn fmt_stat(val: f64, precision: usize) -> String {
    format!("{val:.precision$}")
}

/// Format max value with exceeded indicator (e.g. ">2.00(5)").
fn fmt_max(stats: &HistogramStats, precision: usize) -> String {
    if stats.exceeded_count > 0 {
        format!(
            ">{:.precision$}({})",
            stats.max_bucket, stats.exceeded_count
        )
    } else {
        format!("{:.precision$}", stats.max_bucket)
    }
}

/// Format a single histogram as "avg/p50/p95/p99/max".
fn fmt_histogram(stats: &Option<HistogramStats>, precision: usize) -> String {
    match stats {
        Some(s) if s.count > 0 => {
            format!(
                "{}/{}/{}/{}/{}",
                fmt_stat(s.avg, precision),
                fmt_stat(s.p50, precision),
                fmt_stat(s.p95, precision),
                fmt_stat(s.p99, precision),
                fmt_max(s, precision),
            )
        }
        _ => "N/A".to_string(),
    }
}

/// Format a histogram with a divisor applied (e.g. bytes → KB).
fn fmt_histogram_scaled(stats: &Option<HistogramStats>, precision: usize, divisor: f64) -> String {
    match stats {
        Some(s) if s.count > 0 => {
            let scaled = HistogramStats {
                avg: s.avg / divisor,
                p50: s.p50 / divisor,
                p95: s.p95 / divisor,
                p99: s.p99 / divisor,
                max_bucket: s.max_bucket / divisor,
                ..*s
            };
            fmt_histogram(&Some(scaled), precision)
        }
        _ => "N/A".to_string(),
    }
}

/// Format a gas value with K/M suffixes for readability.
fn fmt_gas(val: f64) -> String {
    if val >= 1_000_000.0 {
        format!("{:.1}M", val / 1_000_000.0)
    } else if val >= 1_000.0 {
        format!("{:.0}K", val / 1_000.0)
    } else {
        format!("{:.0}", val)
    }
}

fn fmt_histogram_gas(stats: &Option<HistogramStats>) -> String {
    match stats {
        Some(s) if s.count > 0 => {
            let max_str = if s.exceeded_count > 0 {
                format!(">{}", fmt_gas(s.max_bucket))
            } else {
                fmt_gas(s.max_bucket)
            };
            format!(
                "{}/{}/{}/{}/{}",
                fmt_gas(s.avg),
                fmt_gas(s.p50),
                fmt_gas(s.p95),
                fmt_gas(s.p99),
                max_str,
            )
        }
        _ => "N/A".to_string(),
    }
}

/// Sort nodes: validators first, then non-validators, alphabetical within each group.
fn sorted_by_group(nodes: &[NodePerfData]) -> Vec<&NodePerfData> {
    let mut sorted: Vec<&NodePerfData> = nodes.iter().collect();
    sorted.sort_by(|a, b| {
        crate::types::group_order(a.group.as_deref())
            .cmp(&crate::types::group_order(b.group.as_deref()))
            .then_with(|| a.name.cmp(&b.name))
    });
    sorted
}

/// Format a human-readable duration from seconds (e.g. "2h 15m", "45m 12s", "30s").
fn fmt_duration(secs: f64) -> String {
    let total = secs as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Compute the max width of each column from pre-rendered cell values.
fn col_widths(rows: &[Vec<String>], headers: &[&str], min_width: usize) -> Vec<usize> {
    let ncols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len().max(min_width)).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(ncols) {
            widths[i] = widths[i].max(cell.len());
        }
    }
    widths
}

/// Write a table section (latency or throughput) with dynamic column widths.
fn write_table(
    out: &mut String,
    sorted: &[&NodePerfData],
    has_groups: bool,
    name_width: usize,
    headers: &[&str],
    sub_headers: &[&str],
    render_row: &dyn Fn(&NodePerfData) -> Vec<String>,
) {
    let rows: Vec<Vec<String>> = sorted.iter().map(|n| render_row(n)).collect();
    let min_width = sub_headers.iter().map(|s| s.len()).max().unwrap_or(0);
    let widths = col_widths(&rows, headers, min_width);

    // Header line
    let mut header = format!("{:<name_width$}", "Node");
    for (i, h) in headers.iter().enumerate() {
        let _ = write!(header, "  {:<w$}", h, w = widths[i]);
    }
    let _ = writeln!(out, "{header}");

    // Sub-header
    let mut sub = format!("{:<name_width$}", "");
    for (i, w) in widths.iter().enumerate() {
        let sh = sub_headers.get(i).copied().unwrap_or("");
        let _ = write!(sub, "  {:<w$}", sh);
    }
    let _ = writeln!(out, "{sub}");

    // Separator
    let total_width: usize = name_width + widths.iter().map(|w| w + 2).sum::<usize>();
    let _ = writeln!(out, "{}", "-".repeat(total_width));

    // Data rows with blank line between groups
    let mut last_group: Option<&str> = None;
    for (idx, node) in sorted.iter().enumerate() {
        if has_groups {
            let g = node.group.as_deref().unwrap_or("Other");
            if last_group.is_some() && last_group != Some(g) {
                let _ = writeln!(out);
            }
            last_group = Some(g);
        }
        let mut line = format!("{:<name_width$}", node.name);
        for (i, cell) in rows[idx].iter().enumerate() {
            let _ = write!(line, "  {:<w$}", cell, w = widths[i]);
        }
        let _ = writeln!(out, "{line}");
    }
}

/// Build a formatted performance report from parsed node data.
pub fn format_perf_report(nodes: &[NodePerfData], options: &PerfDisplayOptions) -> String {
    let mut out = String::new();
    let sorted = sorted_by_group(nodes);
    let has_groups = nodes.iter().any(|n| n.group.is_some());

    let node_count = nodes.len();

    // Derive approximate network age from the node with the most blocks
    let max_block_stats = nodes
        .iter()
        .filter_map(|n| n.block_time.as_ref().filter(|s| s.count > 0))
        .max_by_key(|s| s.count);

    let _ = writeln!(out, "{}", "=".repeat(80));
    let _ = writeln!(out, "Performance Metrics");
    if let Some(s) = max_block_stats {
        let _ = writeln!(
            out,
            "({node_count} node{}, ~{} blocks over {}, cumulative since start)",
            if node_count == 1 { "" } else { "s" },
            s.count,
            fmt_duration(s.sum),
        );
    } else {
        let _ = writeln!(
            out,
            "({node_count} node{})",
            if node_count == 1 { "" } else { "s" }
        );
    }
    let _ = writeln!(out, "{}", "=".repeat(80));

    let name_width = nodes
        .iter()
        .map(|n| n.name.len())
        .max()
        .unwrap_or(10)
        .max(10);

    let hist_sub = "avg/p50/p95/p99/max";

    if options.show_latency {
        let _ = writeln!(out);
        let _ = writeln!(out, "Latency (seconds)");
        write_table(
            &mut out,
            &sorted,
            has_groups,
            name_width,
            &["Block Time", "Finalize", "Build", "Consensus"],
            &[hist_sub, hist_sub, hist_sub, hist_sub],
            &|node| {
                vec![
                    fmt_histogram(&node.block_time, 3),
                    fmt_histogram(&node.block_finalize_time, 3),
                    fmt_histogram(&node.block_build_time, 3),
                    fmt_histogram(&node.consensus_time, 3),
                ]
            },
        );
    }

    if options.show_throughput {
        let _ = writeln!(out);
        let _ = writeln!(out, "Throughput");
        write_table(
            &mut out,
            &sorted,
            has_groups,
            name_width,
            &["Txs/Block", "Block Size (KB)", "Gas/Block", "Txs/s"],
            &[hist_sub, hist_sub, hist_sub, ""],
            &|node| {
                let tps = match (&node.block_tx_count, &node.block_time) {
                    (Some(tx), Some(bt)) if tx.count > 0 && bt.sum > 0.0 => {
                        format!("{:.1}", tx.sum / bt.sum)
                    }
                    _ => "N/A".to_string(),
                };
                vec![
                    fmt_histogram(&node.block_tx_count, 0),
                    fmt_histogram_scaled(&node.block_size, 1, 1024.0),
                    fmt_histogram_gas(&node.block_gas_used),
                    tps,
                ]
            },
        );
    }

    if options.show_summary {
        let _ = writeln!(out);
        let _ = writeln!(out, "Summary");
        let _ = writeln!(out, "{}", "-".repeat(80));

        let block_times: Vec<f64> = nodes
            .iter()
            .filter_map(|n| n.block_time.as_ref().filter(|s| s.count > 0).map(|s| s.avg))
            .collect();

        if !block_times.is_empty() {
            let avg_bt = block_times.iter().sum::<f64>() / block_times.len() as f64;
            let bps = if avg_bt > 0.0 { 1.0 / avg_bt } else { 0.0 };
            let _ = writeln!(out, "  Avg Block Time: {avg_bt:.3}s ({bps:.1} blocks/sec)");
        }

        let finalize_times: Vec<f64> = nodes
            .iter()
            .filter_map(|n| {
                n.block_finalize_time
                    .as_ref()
                    .filter(|s| s.count > 0)
                    .map(|s| s.avg)
            })
            .collect();

        if !finalize_times.is_empty() {
            let avg_ft = finalize_times.iter().sum::<f64>() / finalize_times.len() as f64;
            let _ = writeln!(out, "  Avg Finalize Time: {avg_ft:.3}s");
        }

        // Avg throughput: avg txs/block across nodes, then txs/s = txs/block / avg_block_time
        let avg_tx_per_block: Vec<f64> = nodes
            .iter()
            .filter_map(|n| {
                n.block_tx_count
                    .as_ref()
                    .filter(|s| s.count > 0)
                    .map(|s| s.avg)
            })
            .collect();

        if !avg_tx_per_block.is_empty() && !block_times.is_empty() {
            let avg_txpb = avg_tx_per_block.iter().sum::<f64>() / avg_tx_per_block.len() as f64;
            let avg_bt = block_times.iter().sum::<f64>() / block_times.len() as f64;
            if avg_bt > 0.0 {
                let tps = avg_txpb / avg_bt;
                let _ = writeln!(
                    out,
                    "  Avg Throughput: {tps:.1} txs/s ({avg_txpb:.1} txs/block)"
                );
            }
        }

        if let Some(tx_stats) = nodes
            .iter()
            .filter_map(|n| n.block_tx_count.as_ref().filter(|s| s.count > 0))
            .max_by_key(|s| s.count)
        {
            let _ = writeln!(
                out,
                "  Total Transactions: {:.0} (across {} blocks)",
                tx_stats.sum, tx_stats.count,
            );
        }

        let _ = writeln!(out, "  Nodes: {node_count} reporting");
    }

    out
}

// ── Check ────────────────────────────────────────────────────────────────

/// Fetch Prometheus metrics from all nodes and assert that every node's
/// block time p50 and p95 are within the given thresholds.
pub async fn check_block_time(
    metrics_urls: &[(String, Url)],
    p50_threshold_ms: u64,
    p95_threshold_ms: u64,
) -> Result<Report> {
    let raw = fetch_all_metrics(metrics_urls).await;
    let nodes = parse_perf_metrics(&raw);
    let p50_threshold_s = p50_threshold_ms as f64 / 1000.0;
    let p95_threshold_s = p95_threshold_ms as f64 / 1000.0;

    let mut checks: Vec<CheckResult> = nodes
        .iter()
        .map(|node| match &node.block_time {
            Some(stats) if stats.count > 0 => {
                let p50_ok = stats.p50 <= p50_threshold_s;
                let p95_ok = stats.p95 <= p95_threshold_s;
                let passed = p50_ok && p95_ok;
                let message = format!(
                    "block_time p50={:.3}s (limit {:.3}s{}) p95={:.3}s (limit {:.3}s{}) ({} blocks)",
                    stats.p50,
                    p50_threshold_s,
                    if p50_ok { "" } else { " EXCEEDED" },
                    stats.p95,
                    p95_threshold_s,
                    if p95_ok { "" } else { " EXCEEDED" },
                    stats.count,
                );
                CheckResult {
                    name: node.name.clone(),
                    passed,
                    message,
                }
            }
            _ => CheckResult {
                name: node.name.clone(),
                passed: false,
                message: "no block_time histogram data available".to_string(),
            },
        })
        .collect();

    checks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Report { checks })
}

/// Parse raw Prometheus text into per-node raw block_time histograms.
fn parse_raw_block_time(raw_metrics: &[(String, String)]) -> Vec<(String, Option<RawHistogram>)> {
    raw_metrics
        .iter()
        .filter(|(_, m)| !m.is_empty())
        .map(|(name, raw)| {
            let samples = parse_metrics(raw);
            let display_name = extract_moniker(&samples).unwrap_or_else(|| name.clone());
            let hist = extract_raw_histogram(&samples, "arc_malachite_app_block_time");
            (display_name, hist)
        })
        .collect()
}

/// Assert block time p50/p95 thresholds using only the **delta** between two
/// Prometheus scrapes, isolating the observation window.
///
/// This avoids cumulative histogram skew from periods before the observation
/// window (e.g. warmup with empty blocks).
pub fn check_block_time_delta(
    raw_before: &[(String, String)],
    raw_after: &[(String, String)],
    p50_threshold_ms: u64,
    p95_threshold_ms: u64,
) -> Report {
    let before_nodes = parse_raw_block_time(raw_before);
    let after_nodes = parse_raw_block_time(raw_after);

    let p50_threshold_s = p50_threshold_ms as f64 / 1000.0;
    let p95_threshold_s = p95_threshold_ms as f64 / 1000.0;

    let mut checks: Vec<CheckResult> = after_nodes
        .iter()
        .filter_map(|(name, after_hist)| {
            let before_hist = before_nodes
                .iter()
                .find(|(n, _)| n == name)
                .and_then(|(_, h)| h.as_ref());

            let delta_stats = match (before_hist, after_hist.as_ref()) {
                (Some(before), Some(after)) => {
                    subtract_raw_histograms(before, after).and_then(|d| stats_from_raw(&d))
                }
                _ => {
                    // Node not present in both scrapes — skip (not a perf concern)
                    return None;
                }
            };

            Some(match delta_stats {
                Some(stats) if stats.count > 0 => {
                    let p50_ok = stats.p50 <= p50_threshold_s;
                    let p95_ok = stats.p95 <= p95_threshold_s;
                    let passed = p50_ok && p95_ok;
                    let message = format!(
                        "block_time p50={:.3}s (limit {:.3}s{}) p95={:.3}s (limit {:.3}s{}) ({} blocks)",
                        stats.p50,
                        p50_threshold_s,
                        if p50_ok { "" } else { " EXCEEDED" },
                        stats.p95,
                        p95_threshold_s,
                        if p95_ok { "" } else { " EXCEEDED" },
                        stats.count,
                    );
                    CheckResult {
                        name: name.clone(),
                        passed,
                        message,
                    }
                }
                _ => CheckResult {
                    name: name.clone(),
                    passed: false,
                    message: "no block_time delta data (counter reset or no observations)".to_string(),
                },
            })
        })
        .collect();

    checks.sort_by(|a, b| a.name.cmp(&b.name));
    Report { checks }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_histogram_text() -> String {
        r#"# HELP arc_malachite_app_block_time Interval between two blocks, in seconds
# TYPE arc_malachite_app_block_time histogram
arc_malachite_app_block_time_bucket{moniker="validator1",le="0.01"} 0
arc_malachite_app_block_time_bucket{moniker="validator1",le="0.0233"} 0
arc_malachite_app_block_time_bucket{moniker="validator1",le="0.0543"} 0
arc_malachite_app_block_time_bucket{moniker="validator1",le="0.1268"} 0
arc_malachite_app_block_time_bucket{moniker="validator1",le="0.2957"} 10
arc_malachite_app_block_time_bucket{moniker="validator1",le="0.6899"} 90
arc_malachite_app_block_time_bucket{moniker="validator1",le="1.6095"} 98
arc_malachite_app_block_time_bucket{moniker="validator1",le="2.0"} 100
arc_malachite_app_block_time_bucket{moniker="validator1",le="+Inf"} 100
arc_malachite_app_block_time_sum{moniker="validator1"} 50.0
arc_malachite_app_block_time_count{moniker="validator1"} 100
"#
        .to_string()
    }

    #[test]
    fn parse_metrics_filters_relevant_lines() {
        let raw = r#"# HELP go_gc_duration_seconds A summary of GC duration
# TYPE go_gc_duration_seconds summary
go_gc_duration_seconds{quantile="0.5"} 0.000123
# HELP arc_malachite_app_block_time Interval between two blocks
# TYPE arc_malachite_app_block_time histogram
arc_malachite_app_block_time_bucket{moniker="v1",le="0.5"} 50
arc_malachite_app_block_time_bucket{moniker="v1",le="+Inf"} 100
arc_malachite_app_block_time_sum{moniker="v1"} 40.0
arc_malachite_app_block_time_count{moniker="v1"} 100
# HELP grpc_server_handled_total Total RPCs
# TYPE grpc_server_handled_total counter
grpc_server_handled_total{method="Propose"} 500
"#;
        let samples = parse_metrics(raw);

        let metric_names: Vec<&str> = samples.iter().map(|s| s.metric.as_str()).collect();
        assert!(
            metric_names
                .iter()
                .all(|m| m.starts_with("arc_malachite_app_block_time")),
            "unexpected metrics: {metric_names:?}"
        );
        assert!(
            !metric_names.is_empty(),
            "should have parsed block_time samples"
        );
        assert!(
            !metric_names
                .iter()
                .any(|m| m.contains("go_gc") || m.contains("grpc")),
            "should not contain unrelated metrics"
        );
    }

    #[test]
    fn parse_extracts_node_data() {
        let raw = vec![("node1".to_string(), sample_histogram_text())];
        let nodes = parse_perf_metrics(&raw);

        assert_eq!(nodes.len(), 1);
        let node = &nodes[0];
        assert_eq!(node.name, "validator1");

        let bt = node.block_time.as_ref().unwrap();
        assert_eq!(bt.count, 100);
        assert!((bt.avg - 0.5).abs() < 0.01);
        assert_eq!(bt.exceeded_count, 0);
    }

    #[test]
    fn estimate_percentile_exact_values() {
        // Buckets from sample_histogram_text:
        //   le=0.1268 count=0, le=0.2957 count=10, le=0.6899 count=90,
        //   le=1.6095 count=98, le=2.0 count=100
        let buckets = vec![
            (0.01, 0.0),
            (0.0233, 0.0),
            (0.0543, 0.0),
            (0.1268, 0.0),
            (0.2957, 10.0),
            (0.6899, 90.0),
            (1.6095, 98.0),
            (2.0, 100.0),
        ];
        let total = 100.0;

        // p50: target=50, bucket [0.2957,0.6899], ratio=(50-10)/(90-10)=0.5
        // = 0.2957 + 0.5*(0.6899-0.2957) = 0.4928
        let p50 = estimate_percentile(&buckets, total, 0.5);
        assert!((p50 - 0.4928).abs() < 0.001, "p50={p50}, expected ~0.4928");

        // p95: target=95, bucket [0.6899,1.6095], ratio=(95-90)/(98-90)=0.625
        // = 0.6899 + 0.625*(1.6095-0.6899) = 1.2646
        let p95 = estimate_percentile(&buckets, total, 0.95);
        assert!((p95 - 1.2646).abs() < 0.001, "p95={p95}, expected ~1.2646");

        // p99: target=99, bucket [1.6095,2.0], ratio=(99-98)/(100-98)=0.5
        // = 1.6095 + 0.5*(2.0-1.6095) = 1.8048
        let p99 = estimate_percentile(&buckets, total, 0.99);
        assert!((p99 - 1.8048).abs() < 0.001, "p99={p99}, expected ~1.8048");
    }

    #[test]
    fn percentile_via_full_parse() {
        let raw = vec![("node1".to_string(), sample_histogram_text())];
        let nodes = parse_perf_metrics(&raw);
        let bt = nodes[0].block_time.as_ref().unwrap();

        assert!((bt.p50 - 0.4928).abs() < 0.001, "p50={}", bt.p50);
        assert!((bt.p95 - 1.2646).abs() < 0.001, "p95={}", bt.p95);
        assert!((bt.p99 - 1.8048).abs() < 0.001, "p99={}", bt.p99);
    }

    #[test]
    fn max_exceeded_detection() {
        let raw_text = r#"# TYPE arc_malachite_app_block_time histogram
arc_malachite_app_block_time_bucket{moniker="v1",le="0.5"} 90
arc_malachite_app_block_time_bucket{moniker="v1",le="1.0"} 95
arc_malachite_app_block_time_bucket{moniker="v1",le="+Inf"} 100
arc_malachite_app_block_time_sum{moniker="v1"} 60.0
arc_malachite_app_block_time_count{moniker="v1"} 100
"#;
        let raw = vec![("node1".to_string(), raw_text.to_string())];
        let nodes = parse_perf_metrics(&raw);
        let bt = nodes[0].block_time.as_ref().unwrap();

        assert_eq!(bt.exceeded_count, 5);
        assert!((bt.max_bucket - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn format_report_produces_output() {
        let raw = vec![("node1".to_string(), sample_histogram_text())];
        let nodes = parse_perf_metrics(&raw);
        let report = format_perf_report(&nodes, &PerfDisplayOptions::default());

        assert!(report.contains("Performance Metrics"));
        assert!(report.contains("Latency"));
        assert!(report.contains("validator1"));
        assert!(report.contains("Summary"));
    }

    #[test]
    fn empty_metrics_handled() {
        let raw: Vec<(String, String)> = vec![("node1".to_string(), String::new())];
        let nodes = parse_perf_metrics(&raw);
        assert!(nodes.is_empty());
    }

    #[test]
    fn format_report_respects_display_options() {
        let raw = vec![("node1".to_string(), sample_histogram_text())];
        let nodes = parse_perf_metrics(&raw);

        let latency_only = PerfDisplayOptions {
            show_latency: true,
            show_throughput: false,
            show_summary: false,
        };
        let report = format_perf_report(&nodes, &latency_only);
        assert!(report.contains("Latency"), "should contain Latency section");
        assert!(
            !report.contains("Throughput"),
            "should not contain Throughput"
        );
        assert!(!report.contains("Summary"), "should not contain Summary");

        let throughput_only = PerfDisplayOptions {
            show_latency: false,
            show_throughput: true,
            show_summary: false,
        };
        let report = format_perf_report(&nodes, &throughput_only);
        assert!(!report.contains("Latency"), "should not contain Latency");
        assert!(
            report.contains("Throughput"),
            "should contain Throughput section"
        );
    }

    #[test]
    fn histogram_stats_missing_sum() {
        let raw = r#"# TYPE arc_malachite_app_block_time histogram
arc_malachite_app_block_time_bucket{moniker="v1",le="0.5"} 80
arc_malachite_app_block_time_bucket{moniker="v1",le="1.0"} 100
arc_malachite_app_block_time_bucket{moniker="v1",le="+Inf"} 100
arc_malachite_app_block_time_count{moniker="v1"} 100
"#;
        let raw = vec![("node1".to_string(), raw.to_string())];
        let nodes = parse_perf_metrics(&raw);
        let bt = nodes[0].block_time.as_ref().unwrap();

        assert_eq!(bt.count, 100);
        assert!(
            (bt.avg - 0.0).abs() < f64::EPSILON,
            "avg should be 0 when _sum is missing"
        );
        assert!(bt.p50 > 0.0, "percentiles should still work without _sum");
    }

    #[test]
    fn histogram_stats_zero_observations() {
        let raw = r#"# TYPE arc_malachite_app_block_time histogram
arc_malachite_app_block_time_bucket{moniker="v1",le="0.5"} 0
arc_malachite_app_block_time_bucket{moniker="v1",le="1.0"} 0
arc_malachite_app_block_time_bucket{moniker="v1",le="+Inf"} 0
arc_malachite_app_block_time_sum{moniker="v1"} 0
arc_malachite_app_block_time_count{moniker="v1"} 0
"#;
        let raw = vec![("node1".to_string(), raw.to_string())];
        let nodes = parse_perf_metrics(&raw);
        let node = &nodes[0];

        assert!(
            node.block_time.is_none(),
            "zero observations should return None"
        );
    }

    #[test]
    fn parse_metrics_empty_input() {
        let samples = parse_metrics("");
        assert!(samples.is_empty());
    }

    #[test]
    fn parse_metrics_only_unrelated() {
        let raw = r#"# TYPE go_gc_duration_seconds summary
go_gc_duration_seconds{quantile="0.5"} 0.000123
# TYPE grpc_server_handled_total counter
grpc_server_handled_total{method="Propose"} 500
"#;
        let samples = parse_metrics(raw);
        assert!(samples.is_empty());
    }

    #[test]
    fn estimate_percentile_single_bucket() {
        // All 100 observations in a single bucket [0, 1.0]
        let buckets = vec![(1.0, 100.0)];
        let p50 = estimate_percentile(&buckets, 100.0, 0.5);
        // target=50, bucket [0, 1.0], ratio=50/100=0.5 → 0 + 0.5*1.0 = 0.5
        assert!((p50 - 0.5).abs() < 0.001, "p50={p50}, expected 0.5");
    }

    #[test]
    fn estimate_percentile_all_in_first_bucket() {
        // 100 observations, all in [0, 0.1], nothing in higher buckets
        let buckets = vec![(0.1, 100.0), (0.5, 100.0), (1.0, 100.0)];
        let p95 = estimate_percentile(&buckets, 100.0, 0.95);
        // target=95 hits first bucket (count=100 >= 95), ratio=(95-0)/(100-0)=0.95
        // = 0 + 0.95*0.1 = 0.095
        assert!((p95 - 0.095).abs() < 0.001, "p95={p95}, expected 0.095");
    }

    #[test]
    fn estimate_percentile_at_exact_boundary() {
        // target lands exactly at a bucket boundary
        let buckets = vec![(0.5, 50.0), (1.0, 100.0)];
        // p50: target=50, count=50 >= 50, ratio=(50-0)/(50-0)=1.0 → 0 + 1.0*0.5 = 0.5
        let p50 = estimate_percentile(&buckets, 100.0, 0.5);
        assert!((p50 - 0.5).abs() < 0.001, "p50={p50}, expected 0.5");
    }

    // ── Delta histogram tests ────────────────────────────────────────

    fn make_histogram_text(moniker: &str, bucket_counts: &[u64], sum: f64) -> String {
        let les = [
            "0.01", "0.0233", "0.0543", "0.1268", "0.2957", "0.6899", "1.6095", "2.0",
        ];
        let inf_count = *bucket_counts.last().unwrap_or(&0);
        let mut lines = vec![
            "# HELP arc_malachite_app_block_time Interval between two blocks, in seconds"
                .to_string(),
            "# TYPE arc_malachite_app_block_time histogram".to_string(),
        ];
        for (le, &count) in les.iter().zip(bucket_counts.iter()) {
            lines.push(format!(
                "arc_malachite_app_block_time_bucket{{moniker=\"{moniker}\",le=\"{le}\"}} {count}"
            ));
        }
        lines.push(format!(
            "arc_malachite_app_block_time_bucket{{moniker=\"{moniker}\",le=\"+Inf\"}} {inf_count}"
        ));
        lines.push(format!(
            "arc_malachite_app_block_time_sum{{moniker=\"{moniker}\"}} {sum}"
        ));
        lines.push(format!(
            "arc_malachite_app_block_time_count{{moniker=\"{moniker}\"}} {inf_count}"
        ));
        lines.join("\n")
    }

    #[test]
    fn subtract_raw_histograms_basic() {
        let before = RawHistogram {
            buckets: vec![(0.5, 20.0), (1.0, 50.0)],
            inf_count: 50.0,
            sum: 25.0,
        };
        let after = RawHistogram {
            buckets: vec![(0.5, 80.0), (1.0, 150.0)],
            inf_count: 150.0,
            sum: 75.0,
        };
        let delta = subtract_raw_histograms(&before, &after).unwrap();
        assert_eq!(delta.inf_count, 100.0);
        assert!((delta.sum - 50.0).abs() < 0.001);
        assert_eq!(delta.buckets, vec![(0.5, 60.0), (1.0, 100.0)]);
    }

    #[test]
    fn subtract_raw_histograms_counter_reset() {
        let before = RawHistogram {
            buckets: vec![(0.5, 80.0), (1.0, 100.0)],
            inf_count: 100.0,
            sum: 50.0,
        };
        let after = RawHistogram {
            buckets: vec![(0.5, 10.0), (1.0, 20.0)],
            inf_count: 20.0,
            sum: 10.0,
        };
        assert!(
            subtract_raw_histograms(&before, &after).is_none(),
            "should return None on counter reset"
        );
    }

    #[test]
    fn check_block_time_delta_isolates_window() {
        // "before": 50 blocks, all fast (in first few buckets)
        let before_text = make_histogram_text("v1", &[0, 0, 0, 0, 50, 50, 50, 50], 15.0);
        // "after": 150 blocks total; the 100 new blocks are slower
        //   delta buckets: [0,0,0,0,0,40,90,100] → most in 0.29–0.69 range
        let after_text = make_histogram_text("v1", &[0, 0, 0, 0, 50, 90, 140, 150], 90.0);

        let raw_before = vec![("node1".to_string(), before_text)];
        let raw_after = vec![("node1".to_string(), after_text)];

        let report = check_block_time_delta(&raw_before, &raw_after, 1000, 2000);
        assert_eq!(report.checks.len(), 1);
        let check = &report.checks[0];
        assert!(check.passed, "should pass: {}", check.message);
        assert!(
            check.message.contains("100 blocks"),
            "should reflect delta block count, got: {}",
            check.message
        );
    }

    #[test]
    fn check_block_time_delta_no_before_skips_node() {
        let after_text = sample_histogram_text();
        let raw_before: Vec<(String, String)> = vec![];
        let raw_after = vec![("node1".to_string(), after_text)];

        let report = check_block_time_delta(&raw_before, &raw_after, 800, 2000);
        assert_eq!(
            report.checks.len(),
            0,
            "node missing from first scrape should be skipped"
        );
    }

    #[test]
    fn check_block_time_delta_fails_threshold() {
        // All 100 blocks in the slow 1.6–2.0 range
        let before_text = make_histogram_text("v1", &[0, 0, 0, 0, 0, 0, 0, 0], 0.0);
        let after_text = make_histogram_text("v1", &[0, 0, 0, 0, 0, 0, 0, 100], 180.0);

        let raw_before = vec![("node1".to_string(), before_text)];
        let raw_after = vec![("node1".to_string(), after_text)];

        let report = check_block_time_delta(&raw_before, &raw_after, 550, 1000);
        assert_eq!(report.checks.len(), 1);
        assert!(
            !report.checks[0].passed,
            "should fail with slow blocks: {}",
            report.checks[0].message
        );
    }

    #[test]
    fn subtract_raw_histograms_zero_delta() {
        let hist = RawHistogram {
            buckets: vec![(0.5, 50.0), (1.0, 100.0)],
            inf_count: 100.0,
            sum: 50.0,
        };
        assert!(
            subtract_raw_histograms(&hist, &hist).is_none(),
            "identical scrapes should return None (zero new observations)"
        );
    }

    #[test]
    fn check_block_time_delta_multiple_nodes() {
        let before_v1 = make_histogram_text("v1", &[0, 0, 0, 0, 10, 10, 10, 10], 5.0);
        let before_v2 = make_histogram_text("v2", &[0, 0, 0, 0, 20, 20, 20, 20], 10.0);
        let after_v1 = make_histogram_text("v1", &[0, 0, 0, 0, 60, 60, 60, 60], 30.0);
        let after_v2 = make_histogram_text("v2", &[0, 0, 0, 0, 70, 70, 70, 70], 35.0);

        let raw_before = vec![
            ("node1".to_string(), before_v1),
            ("node2".to_string(), before_v2),
        ];
        let raw_after = vec![
            ("node1".to_string(), after_v1),
            ("node2".to_string(), after_v2),
        ];

        let report = check_block_time_delta(&raw_before, &raw_after, 1000, 2000);
        assert_eq!(report.checks.len(), 2);
        assert!(report.checks[0].passed, "v1: {}", report.checks[0].message);
        assert!(report.checks[1].passed, "v2: {}", report.checks[1].message);
        assert!(report.checks[0].message.contains("50 blocks"));
        assert!(report.checks[1].message.contains("50 blocks"));
    }

    #[test]
    fn check_block_time_delta_node_disappeared_skips() {
        let before_text = make_histogram_text("v1", &[0, 0, 0, 0, 50, 50, 50, 50], 25.0);
        let raw_before = vec![("node1".to_string(), before_text)];
        let raw_after: Vec<(String, String)> = vec![];

        let report = check_block_time_delta(&raw_before, &raw_after, 550, 1000);
        assert_eq!(
            report.checks.len(),
            0,
            "node missing from second scrape should be skipped"
        );
    }

    #[test]
    fn check_block_time_delta_node_restart_between_scrapes() {
        // before: 100 blocks, after: 20 blocks (counter reset from restart)
        let before_text = make_histogram_text("v1", &[0, 0, 0, 0, 50, 80, 100, 100], 50.0);
        let after_text = make_histogram_text("v1", &[0, 0, 0, 0, 10, 15, 20, 20], 10.0);

        let raw_before = vec![("node1".to_string(), before_text)];
        let raw_after = vec![("node1".to_string(), after_text)];

        let report = check_block_time_delta(&raw_before, &raw_after, 550, 1000);
        assert_eq!(report.checks.len(), 1);
        assert!(
            !report.checks[0].passed,
            "counter reset should produce a failing check"
        );
        assert!(
            report.checks[0]
                .message
                .contains("no block_time delta data"),
            "should indicate no delta data: {}",
            report.checks[0].message
        );
    }
}
