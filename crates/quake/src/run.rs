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

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use arc_checks::{
    check_mempool, compute_health_deltas, fetch_all_metrics, parse_all_health_metrics,
    parse_perf_metrics_delta, NodeHealthDelta, NodePerfData,
};
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use color_eyre::eyre::{bail, eyre, Result, WrapErr};
use serde::{Deserialize, Serialize};
use tokio::join;
use tokio::time::{sleep, Instant};
use tracing::{debug, info, warn};
use url::Url;

use crate::genesis;
use crate::testnet::Testnet;
use spammer::SpammerArgs;

pub(crate) const DEFAULT_BLOCK_GAS_LIMIT: u64 = 30_000_000;

/// Transaction mix applied to every phase (not configurable — varying load type
/// is out of scope for this experiment).
pub(crate) const TX_MIX: &str = "transfer=35,legacy=25,erc20=25,guzzler=15";
pub(crate) const GUZZLER_FN_WEIGHTS: &str = "hash-loop=77@200,storage-write=3@1,storage-read=20@35";
pub(crate) const ERC20_FN_WEIGHTS: &str = "transfer=100";

fn format_tx_type_mix(m: &spammer::TxTypeMix) -> String {
    format!(
        "transfer={},legacy={},erc20={},guzzler={}",
        m.transfer, m.legacy, m.erc20, m.guzzler,
    )
}

fn format_erc20_fn_weights(w: &spammer::Erc20FnWeights) -> String {
    format!(
        "transfer={},approve={},transfer-from={}",
        w.transfer, w.approve, w.transfer_from,
    )
}

fn format_guzzler_fn_weights(w: &spammer::GuzzlerFnWeights) -> String {
    format!(
        "hash-loop={}@{},storage-write={}@{},storage-read={}@{},guzzle={}@{},guzzle2={}@{}",
        w.hash_loop.weight,
        w.hash_loop.arg,
        w.storage_write.weight,
        w.storage_write.arg,
        w.storage_read.weight,
        w.storage_read.arg,
        w.guzzle.weight,
        w.guzzle.arg,
        w.guzzle2.weight,
        w.guzzle2.arg,
    )
}

/// Prometheus metrics fetched at the end of the experiment for offline analysis.
/// These cover exactly the signals used for inline detection plus consensus latency.
// Prometheus metric names to download at experiment end.
//
// All CL metrics use prometheus-client 0.23, which exposes:
//   - Histograms as <name>_sum, <name>_count (no queryable base name)
//   - Counters as <name>_total
// Querying the bare histogram base name (e.g. "arc_malachite_app_block_time")
// returns empty results because no series with that exact name exists.
pub(crate) const DOWNLOAD_METRICS: &[&str] = &[
    // Histograms — download both _sum and _count to allow average computation
    "arc_malachite_app_block_time_sum",
    "arc_malachite_app_block_time_count",
    "arc_malachite_app_block_build_time_sum",
    "arc_malachite_app_block_build_time_count",
    "arc_malachite_app_block_finalize_time_sum",
    "arc_malachite_app_block_finalize_time_count",
    "arc_malachite_app_block_gas_used_sum",
    "arc_malachite_app_block_transactions_count_sum",
    "malachitebft_core_consensus_consensus_round_sum",
    "malachitebft_core_consensus_consensus_time_sum",
    // Counters
    "arc_malachite_app_height_restart_count_total",
    "arc_malachite_app_sync_fell_behind_count_total",
    // EL process CPU — counter (seconds), compute rate() for utilisation %
    "reth_process_cpu_seconds_total",
    // EL txpool — gauges; pending = executable now, queued = waiting on a nonce gap
    "reth_transaction_pool_pending_pool_transactions",
    "reth_transaction_pool_queued_pool_transactions",
    // Pool evictions — pending is the sub-pool that fills first and whose cascade
    // clears the queued pool; basefee/blob counters included for completeness.
    "reth_transaction_pool_pending_transactions_evicted_total",
    "reth_transaction_pool_basefee_transactions_evicted_total",
    "reth_transaction_pool_blob_transactions_evicted_total",
    "reth_transaction_pool_queued_transactions_evicted_total",
    // EL process memory — gauge (resident set size in bytes)
    "reth_process_resident_memory_bytes",
];

// ── Saturation detection thresholds ──────────────────────────────────────────

/// Gas/s or actual TPS must grow by at least this fraction of the previous value
/// to avoid the plateau signal.
const PLATEAU_THRESHOLD: f64 = 0.10;

/// The TPS ratio (actual/offered) must not drop by more than this fraction vs
/// the prior phase.
const TPS_RATIO_DROP_THRESHOLD: f64 = 0.15;

/// p95 latency must not more than double vs the prior phase.
const LATENCY_SPIKE_FACTOR: f64 = 2.0;

/// Mempool must grow by at least this many pending transactions to signal.
const MEMPOOL_GROWTH_MIN: f64 = 100.0;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub(crate) enum RunSubcommand {
    /// Ramp offered TPS against an already-running testnet, measure throughput
    /// and latency per phase, and identify the saturation point.
    ///
    /// Iterates through the configured rate phases with cooldowns, downloads
    /// a targeted Prometheus metrics snapshot, and writes experiment.json.
    /// The testnet must be started separately and is left running afterwards.
    ///
    /// Examples:
    ///   quake -f scenarios/mainnet.toml run saturation
    ///   quake -f scenarios/mainnet.toml run saturation --rates 100,500,1000,2000 -d 5m
    #[command(verbatim_doc_comment)]
    Saturation {
        #[command(flatten)]
        args: SaturationArgs,
    },
}

#[derive(Args)]
pub(crate) struct SaturationArgs {
    /// Offered TPS targets in strictly ascending order.
    ///
    /// Comma-separated values, `START-END:STEP` ranges, or a mix of both.
    /// Range expansion is inclusive on both ends when divisible; an uneven
    /// endpoint stops at the largest `start + k*step <= end`.
    ///
    /// Examples:
    ///   `500,1000,2000,4000`            (the default)
    ///   `1000-2000:100`                  (1000, 1100, …, 2000)
    ///   `500,1000-2000:100,4000`         (singles + range mixed)
    #[clap(long, default_value = "500,1000,2000,4000")]
    pub rates: String,

    /// Duration to hold each rate phase
    #[clap(short = 'd', long, default_value = "5m", value_parser = crate::parse_duration)]
    pub phase_duration: Duration,

    /// Ramp-up period: runs actual load at the first rate to warm generator nonces
    /// and the txpool before any measured phase begins
    #[clap(long, default_value = "90s", value_parser = crate::parse_duration)]
    pub rampup: Duration,

    /// Cooldown between phases to let mempool and metrics settle
    #[clap(long, default_value = "90s", value_parser = crate::parse_duration)]
    pub cooldown: Duration,

    /// Number of parallel spammer generators
    #[clap(long, default_value_t = 10)]
    pub generators: usize,

    /// Wall-clock hard limit for the entire experiment
    #[clap(long, default_value = "3h", value_parser = crate::parse_duration)]
    pub max_duration: Duration,

    /// Directory for experiment artifacts (experiment.json, latency CSVs, metrics tarball)
    #[clap(long, default_value = ".quake/experiments")]
    pub output_dir: PathBuf,

    /// Comma-separated list of target nodes for the spammer (exact names or
    /// manifest groups like `ALL_VALIDATORS`). When omitted, every manifest
    /// node is targeted.
    #[clap(long, value_delimiter = ',')]
    pub targets: Option<Vec<String>>,

    /// Weighted transaction type mix (same format as `quake load --mix`,
    /// e.g. `transfer=70,erc20=30`). Defaults to the saturation profile
    /// `transfer=35,legacy=25,erc20=25,guzzler=15`.
    #[clap(long = "mix")]
    pub tx_type_mix: Option<spammer::TxTypeMix>,

    /// Weighted function mix for guzzler calls (same format as
    /// `quake load --guzzler-fn-weights`). Defaults to the saturation profile
    /// `hash-loop=77@200,storage-write=3@1,storage-read=20@35`.
    #[clap(long = "guzzler-fn-weights")]
    pub guzzler_fn_weights: Option<spammer::GuzzlerFnWeights>,

    /// Weighted function mix for ERC-20 calls (same format as
    /// `quake load --erc20-fn-weights`). Defaults to 100% `transfer`.
    #[clap(long = "erc20-fn-weights")]
    pub erc20_fn_weights: Option<spammer::Erc20FnWeights>,

    /// Extra random bytes appended to each transaction's input field
    /// (same as `quake load --tx-input-size`).
    #[clap(long, default_value_t = 0)]
    pub tx_input_size: usize,
}

// ── experiment.json schema ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExperimentMetadata {
    /// Unique identifier: "saturation-YYYYMMDDTHHMMSSZ"
    pub experiment_id: String,
    pub parameters: SaturationParameters,
    pub status: ExperimentStatus,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub phases: Vec<PhaseRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SaturationParameters {
    pub rates: Vec<u64>,
    pub hold_secs: u64,
    pub rampup_secs: u64,
    pub cooldown_secs: u64,
    pub generators: usize,
    pub tx_mix: String,
    pub guzzler_fn_weights: String,
    pub erc20_fn_weights: String,
    /// The manifest used to run the experiment, parsed from its TOML file
    /// into a JSON object. Embedded structurally so the report is
    /// self-describing — every node, region, image tag, and config override
    /// is captured without a separate schema.
    pub manifest: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PhaseRecord {
    pub offered_tps: u64,
    pub started_at: DateTime<Utc>,
    /// Timestamp when the spammer finished (load phase ended, cooldown not included)
    pub load_ended_at: DateTime<Utc>,
    /// Timestamp when the cooldown ended and the next phase (or cleanup) began
    pub ended_at: DateTime<Utc>,
    pub metrics: PhaseMetrics,
    /// Saturation signals that fired during this phase transition
    pub signals: Vec<SaturationSignal>,
}

/// All measured values for a single rate phase.
///
/// Prometheus-derived fields are populated from before/after scrape deltas.
/// `max_mempool` is queried via RPC at the end of the load window.
/// Latency fields come from the per-phase CSV written by the spammer.
/// CPU fields are queried from the Prometheus API at phase end.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct PhaseMetrics {
    /// Gas committed per second across the phase window (`block_gas_used.sum / duration_s`).
    pub gas_per_sec: Option<f64>,
    /// Transactions committed per second (`block_tx_count.sum / duration_s`).
    pub actual_tps: Option<f64>,
    /// Average TPS as observed locally by the spammer: total transactions
    /// submitted divided by the spammer's wall-clock run. Independent of
    /// server acceptance and of the chain-confirmed rate. Drift from
    /// `offered_tps` (configured target) indicates the load generator itself
    /// could not keep up with the requested rate.
    pub actual_offered_tps: Option<f64>,
    /// Average bytes-per-second locally offered by the spammer (total tx
    /// bytes / spammer wall-clock). Together with `actual_offered_tps` this
    /// shows whether load is dominated by many tiny txs or fewer fat ones.
    pub actual_offered_bytes_per_sec: Option<f64>,
    /// Average block gas utilization as % of the block gas limit
    /// (`avg_gas_per_block / block_gas_limit × 100`). Near 100% = blocks full.
    pub fill_pct: Option<f64>,
    /// Average time between consecutive blocks in seconds (`block_time.avg`).
    pub avg_block_time_s: Option<f64>,
    /// Average time to build a block in milliseconds (`block_build_time.avg × 1000`).
    /// Measured from the CL's GetValue call until the payload is returned to consensus.
    pub avg_block_build_time_ms: Option<f64>,
    /// Average time to finalize a block in milliseconds (`block_finalize_time.avg × 1000`).
    /// Covers `engine_newPayload` + `engine_forkchoiceUpdated` on all nodes.
    pub avg_block_finalize_time_ms: Option<f64>,
    /// Fraction of consensus decisions reached in round 0 (informational, not a signal).
    pub round_0_pct: Option<f64>,
    /// Fraction of consensus decisions reached in round 1 (informational, not a signal).
    pub round_1_pct: Option<f64>,
    /// Peak pending sub-pool transaction count across all nodes during the
    /// load window. Sourced from Prometheus
    /// `max(max_over_time(reth_transaction_pool_pending_pool_transactions[Ns]))`
    /// rather than the geth-compat `txpool_status` RPC, which silently
    /// reports `0` for any node whose RPC handler stalls under load — i.e.
    /// the very node we want to observe.
    pub max_mempool: Option<f64>,
    /// Average pending sub-pool transaction count across all nodes during
    /// the load window (`avg(avg_over_time(..._pending_pool_transactions))`).
    pub avg_pending_mempool: Option<f64>,
    /// Peak queued (nonce-gapped) sub-pool transaction count across all
    /// nodes during the load window
    /// (`max(max_over_time(..._queued_pool_transactions))`).
    pub max_queued_mempool: Option<f64>,
    /// Average queued (nonce-gapped) sub-pool transaction count across all
    /// nodes during the load window.
    pub avg_queued_mempool: Option<f64>,
    /// Peak basefee sub-pool depth across all nodes during the load window (via
    /// Prometheus: `max(max_over_time(reth_transaction_pool_basefee_pool_transactions[Ns]))`).
    /// Not exposed by `txpool_status` RPC — geth-compat returns only pending+queued, so
    /// we'd be blind to basefee overflow without this. Legacy txs whose gas price
    /// falls below the chain's current base fee land here.
    pub max_basefee_mempool: Option<f64>,
    /// Average basefee sub-pool depth across all nodes during the load window (via
    /// Prometheus: `avg(avg_over_time(reth_transaction_pool_basefee_pool_transactions[Ns]))`).
    pub avg_basefee_mempool: Option<f64>,
    /// Peak blob sub-pool depth across all nodes during the load window. Recorded for
    /// completeness; ~always 0 in our experiments (no EIP-4844 traffic).
    pub max_blob_mempool: Option<f64>,
    /// Average blob sub-pool depth across all nodes during the load window.
    pub avg_blob_mempool: Option<f64>,
    /// Peak pending sub-pool cumulative size in megabytes during the load window
    /// (from `reth_transaction_pool_pending_pool_size_bytes`). Pairs with
    /// `max_mempool` (count) — Reth caps each subpool on count AND size in MB
    /// independently, so both dimensions matter for diagnosing "txpool is full".
    pub max_pending_size_mb: Option<f64>,
    /// Average pending sub-pool cumulative size in megabytes during the load window.
    pub avg_pending_size_mb: Option<f64>,
    /// Peak basefee sub-pool cumulative size in megabytes during the load window.
    pub max_basefee_size_mb: Option<f64>,
    /// Average basefee sub-pool cumulative size in megabytes during the load window.
    pub avg_basefee_size_mb: Option<f64>,
    /// Peak queued sub-pool cumulative size in megabytes during the load window.
    pub max_queued_size_mb: Option<f64>,
    /// Average queued sub-pool cumulative size in megabytes during the load window.
    pub avg_queued_size_mb: Option<f64>,
    /// Peak blob sub-pool cumulative size in megabytes during the load window.
    pub max_blob_size_mb: Option<f64>,
    /// Average blob sub-pool cumulative size in megabytes during the load window.
    pub avg_blob_size_mb: Option<f64>,
    /// Total transactions evicted from any sub-pool during the phase, summed across
    /// all nodes (`increase({pending|basefee|blob|queued}_evicted_total[Ns])`).
    /// In practice only the pending counter fires; queued is cleared via cascade.
    pub pool_evictions: Option<f64>,
    /// Mean submit-to-finalized latency in milliseconds, from the spammer CSV.
    pub latency_avg_ms: Option<f64>,
    /// Sample standard deviation of submit-to-finalized latency in milliseconds.
    /// Together with the mean this captures spread without the percentile sorting cost.
    pub latency_stddev_ms: Option<f64>,
    /// Median submit-to-finalized latency in milliseconds, from the spammer CSV.
    pub latency_p50_ms: Option<f64>,
    /// 95th-percentile submit-to-finalized latency in milliseconds, from the spammer CSV.
    pub latency_p95_ms: Option<f64>,
    /// Average EL (Reth) CPU utilization across validators during the load window
    /// (% of one core; 100 = one full core, 200 = two cores).
    pub el_cpu_avg_pct: Option<f64>,
    /// Maximum EL CPU utilization seen on any single validator during the load window.
    pub el_cpu_max_pct: Option<f64>,
    /// Average CL (Malachite) CPU utilization across validators during the load window
    /// (same units as `el_cpu_avg_pct`).
    pub cl_cpu_avg_pct: Option<f64>,
    /// Maximum CL CPU utilization seen on any single validator during the load window.
    pub cl_cpu_max_pct: Option<f64>,
    /// Average EL (Reth) resident memory in MiB across validators at phase end
    /// (`avg(reth_process_resident_memory_bytes) / 1 048 576`).
    pub el_mem_avg_mb: Option<f64>,
    /// Peak EL (Reth) resident memory in MiB on any single validator at phase end.
    pub el_mem_peak_mb: Option<f64>,
    /// JSON-RPC error counts from the spammer, keyed by the raw server error string.
    /// Empty when fire-and-forget mode produced no rejected transactions.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub rpc_errors: HashMap<String, u64>,
}

/// A saturation signal that fired comparing this phase to the previous one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SaturationSignal {
    /// Gas/s grew by less than the plateau threshold vs the prior phase
    GasPlateaued,
    /// Actual committed TPS grew by less than the plateau threshold
    TpsPlateaued,
    /// Ratio of actual TPS to offered TPS dropped significantly
    TpsRatioDrop,
    /// Latency p95 more than doubled vs the prior phase
    LatencySpike,
    /// Max mempool depth grew between phases
    MempoolGrowth,
    /// Any single EL node exceeded one full CPU core during the phase
    ElCpuSaturated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub(crate) enum ExperimentStatus {
    Completed,
    TimedOut,
    Failed { reason: String },
}

// ── Measurement helpers ───────────────────────────────────────────────────────

/// Aggregate per-node Prometheus deltas into single phase-level metrics.
///
/// Since all validators commit the same blocks, values should be nearly identical
/// across nodes; averaging reduces measurement noise.
fn aggregate_phase_metrics(
    perf_deltas: &[NodePerfData],
    health_deltas: &[NodeHealthDelta],
    block_gas_limit: u64,
    phase_duration_secs: f64,
) -> PhaseMetrics {
    let gas_per_sec = avg_opt(perf_deltas.iter().filter_map(|n| {
        n.block_gas_used
            .as_ref()
            .map(|h| h.sum / phase_duration_secs)
    }));

    let actual_tps = avg_opt(perf_deltas.iter().filter_map(|n| {
        n.block_tx_count
            .as_ref()
            .map(|h| h.sum / phase_duration_secs)
    }));

    let fill_pct = avg_opt(perf_deltas.iter().filter_map(|n| {
        n.block_gas_used.as_ref().and_then(|h| {
            if h.count > 0 {
                Some(h.sum / h.count as f64 / block_gas_limit as f64 * 100.0)
            } else {
                None
            }
        })
    }));

    let avg_block_time_s = avg_opt(
        perf_deltas
            .iter()
            .filter_map(|n| n.block_time.as_ref().map(|h| h.avg)),
    );

    let avg_block_build_time_ms = avg_opt(
        perf_deltas
            .iter()
            .filter_map(|n| n.block_build_time.as_ref().map(|h| h.avg * 1000.0)),
    );

    let avg_block_finalize_time_ms = avg_opt(
        perf_deltas
            .iter()
            .filter_map(|n| n.block_finalize_time.as_ref().map(|h| h.avg * 1000.0)),
    );

    // Sum round deltas across all nodes for a network-wide view.
    let total_decisions: i64 = health_deltas.iter().map(|n| n.delta_decisions).sum();
    let total_round_0: i64 = health_deltas.iter().map(|n| n.delta_round_0).sum();
    let total_round_1: i64 = health_deltas.iter().map(|n| n.delta_round_1).sum();

    let (round_0_pct, round_1_pct) = if total_decisions > 0 {
        let d: f64 = total_decisions as f64;
        (
            Some(total_round_0 as f64 / d * 100.0),
            Some(total_round_1 as f64 / d * 100.0),
        )
    } else {
        (None, None)
    };

    PhaseMetrics {
        gas_per_sec,
        actual_tps,
        fill_pct,
        avg_block_time_s,
        avg_block_build_time_ms,
        avg_block_finalize_time_ms,
        round_0_pct,
        round_1_pct,
        ..Default::default()
    }
}

fn avg_opt(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0_f64, 0usize), |(s, c), v| (s + v, c + 1));
    if count == 0 {
        None
    } else {
        Some(sum / count as f64)
    }
}

/// Parse a latency CSV written by the spammer and return per-transaction
/// latencies in milliseconds.
///
/// CSV format (columns): tx_hash, submitted_at, finalized_observed_at, …
/// Both timestamp columns are RFC 3339 with millisecond precision.
fn read_latency_csv(path: &Path) -> Result<Vec<f64>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut latencies = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if i == 0 {
            continue; // skip header
        }
        let mut cols = line.splitn(3, ',');
        let _tx_hash = cols.next();
        let submitted = cols.next().unwrap_or("").trim();
        let finalized = cols
            .next()
            .and_then(|s| s.split(',').next())
            .unwrap_or("")
            .trim();

        let t_sub = DateTime::parse_from_rfc3339(submitted);
        let t_fin = DateTime::parse_from_rfc3339(finalized);

        if let (Ok(s), Ok(f)) = (t_sub, t_fin) {
            let diff_ms = (f - s).num_milliseconds();
            if diff_ms >= 0 {
                latencies.push(diff_ms as f64);
            }
        }
    }
    Ok(latencies)
}

/// Compute p50 and p95 latency percentiles in milliseconds.
/// Mean, sample standard deviation, p50, and p95 of a latency sample.
///
/// Returns `(avg, stddev, p50, p95)`. Each is `None` only when the input is
/// empty (stddev is also `None` for a single-element sample, since sample
/// stddev requires n ≥ 2).
fn compute_latency_stats(
    latencies_ms: &[f64],
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    if latencies_ms.is_empty() {
        return (None, None, None, None);
    }
    let n = latencies_ms.len();
    let sum: f64 = latencies_ms.iter().sum();
    let mean = sum / n as f64;
    let stddev = if n >= 2 {
        let var = latencies_ms.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
        Some(var.sqrt())
    } else {
        None
    };
    let mut sorted = latencies_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    (
        Some(mean),
        stddev,
        Some(sorted_percentile(&sorted, 50.0)),
        Some(sorted_percentile(&sorted, 95.0)),
    )
}

fn sorted_percentile(sorted: &[f64], pct: f64) -> f64 {
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let idx = ((pct / 100.0) * (n - 1) as f64).round() as usize;
    sorted[idx.min(n - 1)]
}

// ── Saturation signal detection ───────────────────────────────────────────────

/// Any single EL node exceeding this CPU threshold (% of one core) is a saturation signal.
/// The payload builder is single-threaded, so its ceiling is exactly one core (100%)
/// regardless of how many vCPUs the machine has. Once any node's Reth process crosses this
/// threshold the builder thread is saturated and TPS will not grow further.
const EL_CPU_SATURATION_PCT: f64 = 100.0;

/// Detect which saturation signals fired by comparing `current` to `prev`.
///
/// Returns an empty `Vec` for the first phase (no previous to compare against).
fn detect_saturation_signals(
    prev: Option<&PhaseRecord>,
    current: &PhaseRecord,
) -> Vec<SaturationSignal> {
    let mut signals = Vec::new();
    let Some(prev) = prev else { return signals };
    if prev.offered_tps == 0 || current.offered_tps == 0 {
        return signals;
    }

    // GasPlateaued: gas throughput barely grew despite higher offered load
    if let (Some(g_prev), Some(g_curr)) = (prev.metrics.gas_per_sec, current.metrics.gas_per_sec) {
        if g_prev > 0.0 && (g_curr - g_prev) / g_prev < PLATEAU_THRESHOLD {
            signals.push(SaturationSignal::GasPlateaued);
        }
    }

    // TpsPlateaued: committed TPS barely grew
    if let (Some(t_prev), Some(t_curr)) = (prev.metrics.actual_tps, current.metrics.actual_tps) {
        if t_prev > 0.0 && (t_curr - t_prev) / t_prev < PLATEAU_THRESHOLD {
            signals.push(SaturationSignal::TpsPlateaued);
        }
    }

    // TpsRatioDrop: fraction of offered load that was committed dropped significantly
    if let (Some(t_prev), Some(t_curr)) = (prev.metrics.actual_tps, current.metrics.actual_tps) {
        let ratio_prev = t_prev / prev.offered_tps as f64;
        let ratio_curr = t_curr / current.offered_tps as f64;
        if ratio_prev > 0.0 && (ratio_prev - ratio_curr) / ratio_prev > TPS_RATIO_DROP_THRESHOLD {
            signals.push(SaturationSignal::TpsRatioDrop);
        }
    }

    // LatencySpike: p95 more than doubled
    if let (Some(p95_prev), Some(p95_curr)) =
        (prev.metrics.latency_p95_ms, current.metrics.latency_p95_ms)
    {
        if p95_prev > 0.0 && p95_curr > p95_prev * LATENCY_SPIKE_FACTOR {
            signals.push(SaturationSignal::LatencySpike);
        }
    }

    // MempoolGrowth: pending transactions accumulated across the phase boundary
    if let (Some(m_prev), Some(m_curr)) = (prev.metrics.max_mempool, current.metrics.max_mempool) {
        if m_curr > m_prev + MEMPOOL_GROWTH_MIN {
            signals.push(SaturationSignal::MempoolGrowth);
        }
    }

    // ElCpuSaturated: the busiest EL node exceeded one full CPU core
    if let Some(cpu_max) = current.metrics.el_cpu_max_pct {
        if cpu_max > EL_CPU_SATURATION_PCT {
            signals.push(SaturationSignal::ElCpuSaturated);
        }
    }

    signals
}

// ── Inline phase table ────────────────────────────────────────────────────────

/// Reprint the complete phase-results table at every phase boundary.
///
/// Cumulative re-emission means a reader scrolling back through interleaved
/// `INFO`-level log lines always finds a contiguous, fully-rendered table at
/// the bottom — instead of one-row-at-a-time fragments scattered between log
/// messages, which makes columnar comparison difficult.
fn print_phase_table(phases: &[PhaseRecord]) {
    println!();
    println!(
        "{:>6}  {:>7}  {:>8}  {:>8}  {:>7}  {:>6}  {:>8}  {:>11}  {:>13}  {:>13}  {:>13}  {:>13}  {:>8}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}  {:>8}",
        "Rate", "OffTPS", "ActlTPS", "Gas/s", "Fill%", "BlkT", "FinalMs",
        "Avg±SDms", "PeakPend(MB)", "AvgPend(MB)", "PeakQued(MB)", "AvgQued(MB)",
        "PoolEvct", "ELCPUavg", "ELCPUmax", "CLCPUavg", "CLCPUmax", "MemAvgMB", "MemPkMB",
    );
    println!("{}", "-".repeat(196));
    for phase in phases {
        println!("{phase}");
    }
    println!();
}

/// Render a "count(MB)" cell. Either piece is rendered as `-` when missing,
/// so a half-populated row stays legible. Used for the mempool columns where
/// Reth caps each subpool on count AND size independently.
fn fmt_pool_cell(count: Option<f64>, size_mb: Option<f64>) -> String {
    match (count, size_mb) {
        (Some(c), Some(s)) => format!("{c:.0}({s:.1})"),
        (Some(c), None) => format!("{c:.0}(-)"),
        (None, Some(s)) => format!("-({s:.1})"),
        (None, None) => "-".into(),
    }
}

impl fmt::Display for PhaseRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = &self.metrics;
        let off_tps = m
            .actual_offered_tps
            .map(|t| format!("{t:.0}"))
            .unwrap_or_else(|| "-".into());
        let tps = m
            .actual_tps
            .map(|t| format!("{t:.0}"))
            .unwrap_or_else(|| "-".into());
        let gas = m.gas_per_sec.map(fmt_gas).unwrap_or_else(|| "-".into());
        let fill = m
            .fill_pct
            .map(|f| format!("{f:.1}%"))
            .unwrap_or_else(|| "-".into());
        let blkt = m
            .avg_block_time_s
            .map(|t| format!("{t:.2}s"))
            .unwrap_or_else(|| "-".into());
        let final_ms = m
            .avg_block_finalize_time_ms
            .map(|t| format!("{t:.0}"))
            .unwrap_or_else(|| "-".into());
        let avg_sd = match (m.latency_avg_ms, m.latency_stddev_ms) {
            (Some(avg), Some(sd)) => format!("{avg:.0}±{sd:.0}"),
            (Some(avg), None) => format!("{avg:.0}"),
            _ => "-".into(),
        };
        let pool = fmt_pool_cell(m.max_mempool, m.max_pending_size_mb);
        let avg_pend = fmt_pool_cell(m.avg_pending_mempool, m.avg_pending_size_mb);
        let queued = fmt_pool_cell(m.max_queued_mempool, m.max_queued_size_mb);
        let avg_qued = fmt_pool_cell(m.avg_queued_mempool, m.avg_queued_size_mb);
        let pool_evct = m
            .pool_evictions
            .map(|e| format!("{e:.0}"))
            .unwrap_or_else(|| "-".into());
        let el_cpu_avg = m
            .el_cpu_avg_pct
            .map(|c| format!("{c:.0}%"))
            .unwrap_or_else(|| "-".into());
        let el_cpu_max = m
            .el_cpu_max_pct
            .map(|c| format!("{c:.0}%"))
            .unwrap_or_else(|| "-".into());
        let cl_cpu_avg = m
            .cl_cpu_avg_pct
            .map(|c| format!("{c:.0}%"))
            .unwrap_or_else(|| "-".into());
        let cl_cpu_max = m
            .cl_cpu_max_pct
            .map(|c| format!("{c:.0}%"))
            .unwrap_or_else(|| "-".into());
        let mem_avg = m
            .el_mem_avg_mb
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "-".into());
        let mem_pk = m
            .el_mem_peak_mb
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "-".into());
        write!(
            f,
            "{:>6}  {:>7}  {:>8}  {:>8}  {:>7}  {:>6}  {:>8}  {:>11}  {:>13}  {:>13}  {:>13}  {:>13}  {:>8}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}  {:>8}",
            self.offered_tps, off_tps, tps, gas, fill, blkt, final_ms,
            avg_sd, pool, avg_pend, queued, avg_qued, pool_evct,
            el_cpu_avg, el_cpu_max, cl_cpu_avg, cl_cpu_max,
            mem_avg, mem_pk,
        )
    }
}

fn fmt_gas(gas_per_sec: f64) -> String {
    if gas_per_sec >= 1_000_000.0 {
        format!("{:.1}M", gas_per_sec / 1_000_000.0)
    } else if gas_per_sec >= 1_000.0 {
        format!("{:.1}K", gas_per_sec / 1_000.0)
    } else {
        format!("{gas_per_sec:.0}")
    }
}

#[cfg(test)]
fn signal_abbrev(s: &SaturationSignal) -> &'static str {
    match s {
        SaturationSignal::GasPlateaued => "GAS_PLATEAU",
        SaturationSignal::TpsPlateaued => "TPS_PLATEAU",
        SaturationSignal::TpsRatioDrop => "TPS_RATIO",
        SaturationSignal::LatencySpike => "LATENCY",
        SaturationSignal::MempoolGrowth => "MEMPOOL",
        SaturationSignal::ElCpuSaturated => "CPU_SAT",
    }
}

// ── Prometheus helpers ────────────────────────────────────────────────────────

/// Query the Prometheus instant API for a scalar PromQL expression.
///
/// `eval_time` is a Unix timestamp; the query is evaluated at that point in time.
/// Returns `None` on any network or parse error.
async fn query_prometheus_scalar(prometheus_url: &str, query: &str, eval_time: i64) -> Option<f64> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let url = format!("{prometheus_url}/api/v1/query");
    let resp = client
        .get(&url)
        .query(&[("query", query), ("time", &eval_time.to_string())])
        .send()
        .await
        .ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    body.pointer("/data/result/0/value/1")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
}

/// Query average and max EL CPU utilization (% of one core) over the phase window.
///
/// Uses `rate(reth_process_cpu_seconds_total[{duration}s])` evaluated at phase end.
/// Returns `(None, None)` if Prometheus is unreachable or the metric is absent.
async fn query_el_cpu_pct(
    prometheus_url: &str,
    phase_duration_secs: f64,
    eval_time: i64,
) -> (Option<f64>, Option<f64>) {
    let dur = phase_duration_secs as u64;
    let q_avg = format!("avg(rate(reth_process_cpu_seconds_total[{dur}s])) * 100");
    let q_max = format!("max(rate(reth_process_cpu_seconds_total[{dur}s])) * 100");
    let (avg, max) = join!(
        query_prometheus_scalar(prometheus_url, &q_avg, eval_time),
        query_prometheus_scalar(prometheus_url, &q_max, eval_time),
    );
    (avg, max)
}

/// Query average and peak CL (Malachite) CPU utilization across validators
/// over the load window, in % of one core.
///
/// Uses `rate(process_cpu_seconds_total{job=~".+_cl"}[{duration}s])` — the
/// `_cl` job filter excludes the EL targets which expose
/// `reth_process_cpu_seconds_total` under a different name but could otherwise
/// pollute results if any other exporter ever surfaces a bare
/// `process_cpu_seconds_total`.
async fn query_cl_cpu_pct(
    prometheus_url: &str,
    phase_duration_secs: f64,
    eval_time: i64,
) -> (Option<f64>, Option<f64>) {
    let dur = phase_duration_secs as u64;
    // malachite-app's prometheus exporter prefixes its process metrics with
    // `arc_malachite_app_*`; plain `process_cpu_seconds_total` only exists on
    // the cc node-exporter, so without the prefix this query returns no series.
    let q_avg = format!(
        r#"avg(rate(arc_malachite_app_process_cpu_seconds_total{{job=~".+_cl"}}[{dur}s])) * 100"#
    );
    let q_max = format!(
        r#"max(rate(arc_malachite_app_process_cpu_seconds_total{{job=~".+_cl"}}[{dur}s])) * 100"#
    );
    let (avg, max) = join!(
        query_prometheus_scalar(prometheus_url, &q_avg, eval_time),
        query_prometheus_scalar(prometheus_url, &q_max, eval_time),
    );
    (avg, max)
}

/// Query average and peak EL resident memory in MiB at phase end.
///
/// Uses `reth_process_resident_memory_bytes` evaluated as an instant vector.
/// Returns `(None, None)` if Prometheus is unreachable or the metric is absent.
async fn query_el_memory_mb(prometheus_url: &str, eval_time: i64) -> (Option<f64>, Option<f64>) {
    let mib = 1_048_576_f64;
    let q_avg = format!("avg(reth_process_resident_memory_bytes) / {mib}");
    let q_peak = format!("max(reth_process_resident_memory_bytes) / {mib}");
    let (avg, peak) = join!(
        query_prometheus_scalar(prometheus_url, &q_avg, eval_time),
        query_prometheus_scalar(prometheus_url, &q_peak, eval_time),
    );
    (avg, peak)
}

/// Query total transactions evicted from all sub-pools (pending, basefee, blob, queued)
/// over the phase window. In practice only the pending counter fires; the queued pool
/// is cleared via cascade from pending evictions and its own counter stays at zero.
async fn query_pool_evictions(
    prometheus_url: &str,
    phase_duration_secs: f64,
    eval_time: i64,
) -> Option<f64> {
    let dur = phase_duration_secs as u64;
    let query = format!(
        r#"sum(increase({{__name__=~"reth_transaction_pool_(pending|basefee|blob|queued)_transactions_evicted_total"}}[{dur}s]))"#
    );
    query_prometheus_scalar(prometheus_url, &query, eval_time).await
}

/// Query chain-confirmed TPS over the spammer's actual sending window.
///
/// Uses the histogram `_sum` (per-block tx count summed across observations)
/// from the consensus app. `avg(rate(_sum[duration_s]))` evaluated at the
/// spammer's `finished_at` gives per-node mean tx-per-second over the window;
/// averaging across nodes mirrors `aggregate_phase_metrics`. Returns `None`
/// when Prometheus is unreachable or the metric is absent.
async fn query_chain_actual_tps(
    prometheus_url: &str,
    phase_duration_secs: f64,
    eval_time: i64,
) -> Option<f64> {
    let dur = phase_duration_secs as u64;
    let query = format!("avg(rate(arc_malachite_app_block_transactions_count_sum[{dur}s]))");
    query_prometheus_scalar(prometheus_url, &query, eval_time).await
}

/// Time-aligned per-block average for a histogram metric, restricted to the
/// spammer's actual sending window. Aggregates across nodes as
/// `sum(rate(_sum)) / sum(rate(_count))` — total observed time across the
/// whole cluster divided by total observations across the whole cluster.
///
/// The earlier `avg(rate(_sum) / rate(_count))` form computed a per-node
/// average and then averaged across nodes, which produces `NaN` whenever
/// any node's `rate(_count)` is zero (e.g. `block_build_time` is only
/// recorded by the block proposer, so non-proposer nodes have a flat
/// counter during the window and trigger `0/0`). Sum-then-divide tolerates
/// sparse observers and is the form a reader expects for "per-block
/// average across the network".
///
/// Returns `None` when Prometheus is unreachable, the metric is absent, or
/// no blocks landed in the window (total count delta = 0).
async fn query_block_histogram_avg(
    prometheus_url: &str,
    metric_base: &str,
    phase_duration_secs: f64,
    eval_time: i64,
) -> Option<f64> {
    let dur = phase_duration_secs as u64;
    let query =
        format!("sum(rate({metric_base}_sum[{dur}s])) / sum(rate({metric_base}_count[{dur}s]))");
    query_prometheus_scalar(prometheus_url, &query, eval_time).await
}

/// Time-aligned average block-gas fill percentage over the spammer's window.
/// Per-block average gas-used `/` block_gas_limit × 100, averaged across nodes.
///
/// Same dilution problem as the block-timing histograms: the snapshot delta
/// path averages across ~340 s of subprocess wall-clock including idle
/// setup/teardown, so the resulting fill ratio understates how full blocks
/// actually were during the load window.
async fn query_block_fill_pct_aligned(
    prometheus_url: &str,
    phase_duration_secs: f64,
    eval_time: i64,
    block_gas_limit: u64,
) -> Option<f64> {
    let dur = phase_duration_secs as u64;
    let query = format!(
        "sum(rate(arc_malachite_app_block_gas_used_sum[{dur}s])) \
         / sum(rate(arc_malachite_app_block_gas_used_count[{dur}s])) \
         / {block_gas_limit} * 100"
    );
    query_prometheus_scalar(prometheus_url, &query, eval_time).await
}

/// Query chain-confirmed gas-per-second over the spammer's sending window.
///
/// Same approach as [`query_chain_actual_tps`] but on the block-gas histogram.
async fn query_chain_gas_per_sec(
    prometheus_url: &str,
    phase_duration_secs: f64,
    eval_time: i64,
) -> Option<f64> {
    let dur = phase_duration_secs as u64;
    let query = format!("avg(rate(arc_malachite_app_block_gas_used_sum[{dur}s]))");
    query_prometheus_scalar(prometheus_url, &query, eval_time).await
}

/// Query the peak value of a Reth subpool metric across all nodes during the
/// load window. `pool_name` is one of `pending|basefee|blob|queued`,
/// `metric_suffix` is `transactions` (count) or `size_bytes` (cumulative size).
///
/// We need this because the geth-compat `txpool_status` RPC only returns
/// pending+queued counts; basefee and blob subpools are invisible at the RPC
/// layer, and no subpool exposes byte size via RPC at all. Reth exposes every
/// subpool's count *and* size as Prometheus gauges, so we read them here.
async fn query_subpool_peak(
    prometheus_url: &str,
    pool_name: &str,
    metric_suffix: &str,
    phase_duration_secs: f64,
    eval_time: i64,
) -> Option<f64> {
    let dur = phase_duration_secs as u64;
    let query = format!(
        "max(max_over_time(reth_transaction_pool_{pool_name}_pool_{metric_suffix}[{dur}s]))"
    );
    query_prometheus_scalar(prometheus_url, &query, eval_time).await
}

/// Query the average value of a Reth subpool metric across all nodes during
/// the load window. Companion to [`query_subpool_peak`].
async fn query_subpool_avg(
    prometheus_url: &str,
    pool_name: &str,
    metric_suffix: &str,
    phase_duration_secs: f64,
    eval_time: i64,
) -> Option<f64> {
    let dur = phase_duration_secs as u64;
    let query = format!(
        "avg(avg_over_time(reth_transaction_pool_{pool_name}_pool_{metric_suffix}[{dur}s]))"
    );
    query_prometheus_scalar(prometheus_url, &query, eval_time).await
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub(crate) async fn dispatch(testnet: Testnet, command: RunSubcommand) -> Result<()> {
    match command {
        RunSubcommand::Saturation { args } => run_saturation(testnet, args).await,
    }
}

/// Read-only inputs threaded through every phase of a saturation run.
/// Mutable per-phase state (generator state, remote state path) is passed
/// separately via `PhaseLoopState`.
struct SaturationCtx<'a> {
    testnet: &'a Testnet,
    args: &'a SaturationArgs,
    block_gas_limit: u64,
    tx_type_mix: spammer::TxTypeMix,
    guzzler_fn_weights: spammer::GuzzlerFnWeights,
    erc20_fn_weights: spammer::Erc20FnWeights,
    tx_input_size: usize,
    out_dir: PathBuf,
    deadline: Instant,
    num_generators: usize,
    max_accounts: usize,
    spammer_nodes: Vec<String>,
    target_ws_urls: Vec<(String, Url)>,
    rpc_urls: Vec<(String, Url)>,
    metrics_urls: Vec<(String, Url)>,
    prometheus_url: String,
    is_remote: bool,
    remote_state_dir: &'static str,
}

/// Mutable phase-to-phase state — local generator handles (for in-process
/// resume) and the remote-mode state-file path written by the previous phase.
struct PhaseLoopState {
    generator_state: Option<spammer::SpammerState>,
    remote_state_in_path: Option<String>,
}

async fn run_saturation(testnet: Testnet, args: SaturationArgs) -> Result<()> {
    let rates = parse_rates(&args.rates)?;
    let (mut meta, ctx) = init_saturation(&testnet, &args, &rates)?;
    save_experiment_json(&ctx.out_dir, &meta)?;

    preflight_checks(ctx.testnet).await?;

    let mut phase_state = run_rampup_phase(&ctx, rates[0]).await?;

    let mut experiment_status = ExperimentStatus::Completed;
    for rate in &rates {
        if Instant::now() >= ctx.deadline {
            info!("Max duration reached, stopping before phase {rate} TPS");
            experiment_status = ExperimentStatus::TimedOut;
            break;
        }
        let prev = meta.phases.last().cloned();
        let record = run_one_phase(&ctx, *rate, prev.as_ref(), &mut phase_state).await?;
        meta.phases.push(record);
        print_phase_table(&meta.phases);
        save_experiment_json(&ctx.out_dir, &meta)?;
    }

    finalize_experiment(&ctx, &mut meta, experiment_status).await
}

/// Resolve CLI inputs into a [`SaturationCtx`] and build the initial
/// [`ExperimentMetadata`] record. Reaches out to the genesis file and the
/// node URL helpers to populate routing, so the testnet must be deployed —
/// but consensus health is checked separately by [`preflight_checks`].
fn init_saturation<'a>(
    testnet: &'a Testnet,
    args: &'a SaturationArgs,
    rates: &[u64],
) -> Result<(ExperimentMetadata, SaturationCtx<'a>)> {
    // Pull the chain's actual gas limit from the manifest the testnet was
    // started with — single source of truth for both the genesis
    // ProtocolConfig (what the chain enforces) and the runner's `Fill%`
    // denominator (what the table reports). Previously the runner took a
    // separate `--block-gas-limit` CLI flag defaulting to 30M, which made
    // every bumped-gas-limit experiment require a matching --flag value
    // and would silently produce `Fill% > 100%` if the caller forgot.
    let block_gas_limit = testnet
        .manifest
        .block_gas_limit
        .unwrap_or(DEFAULT_BLOCK_GAS_LIMIT);

    let experiment_id = format!("saturation-{}", Utc::now().format("%Y%m%dT%H%M%SZ"));
    let out_dir = args.output_dir.join(&experiment_id);
    fs::create_dir_all(&out_dir)?;

    let deadline = Instant::now() + args.max_duration;

    // Resolve mix / weights / input size up front so experiment.json records the
    // values that actually ran (not the compile-time defaults). CLI overrides
    // win; otherwise fall back to the saturation defaults shared across phases.
    let tx_type_mix = match args.tx_type_mix {
        Some(m) => m,
        None => TX_MIX.parse().map_err(|e: String| eyre!("{e}"))?,
    };
    let guzzler_fn_weights = match args.guzzler_fn_weights {
        Some(w) => w,
        None => GUZZLER_FN_WEIGHTS
            .parse()
            .map_err(|e: String| eyre!("{e}"))?,
    };
    let erc20_fn_weights = match args.erc20_fn_weights {
        Some(w) => w,
        None => ERC20_FN_WEIGHTS.parse().map_err(|e: String| eyre!("{e}"))?,
    };
    let tx_input_size = args.tx_input_size;

    let manifest_json = {
        let path = &testnet.manifest_path;
        let toml_str = fs::read_to_string(path)
            .wrap_err_with(|| format!("read manifest at {}", path.display()))?;
        let toml_value: toml::Value = toml::from_str(&toml_str)
            .wrap_err_with(|| format!("parse manifest at {}", path.display()))?;
        serde_json::to_value(toml_value)
            .wrap_err("convert manifest TOML to JSON for experiment report")?
    };

    let meta = ExperimentMetadata {
        experiment_id,
        parameters: SaturationParameters {
            rates: rates.to_vec(),
            hold_secs: args.phase_duration.as_secs(),
            rampup_secs: args.rampup.as_secs(),
            cooldown_secs: args.cooldown.as_secs(),
            generators: args.generators,
            tx_mix: format_tx_type_mix(&tx_type_mix),
            guzzler_fn_weights: format_guzzler_fn_weights(&guzzler_fn_weights),
            erc20_fn_weights: format_erc20_fn_weights(&erc20_fn_weights),
            manifest: manifest_json,
        },
        // Written early so a partial record exists even on failure
        status: ExperimentStatus::Failed {
            reason: "experiment did not complete".into(),
        },
        started_at: Utc::now(),
        ended_at: None,
        phases: vec![],
    };

    let num_extra_accounts = genesis::num_prefunded_accounts(
        &testnet.dir.join("assets").join("genesis.json"),
        testnet.manifest.num_validators(),
    )?;
    let max_accounts = num_extra_accounts;
    let num_generators = if args.generators > max_accounts {
        warn!(
            "Requested {} generators but only {} spammer accounts available; \
             capping generators. Re-deploy with more accounts to use more generators.",
            args.generators, max_accounts
        );
        max_accounts
    } else {
        args.generators
    };

    let selectors = args.targets.clone().unwrap_or_default();
    let spammer_nodes = crate::load::resolve_load_target_nodes(&testnet.manifest, &selectors)?;
    let target_ws_urls = testnet.nodes_metadata.to_execution_ws_urls(&spammer_nodes);
    // Mempool + RPC checks scan every node (not just spammer targets) so we
    // observe the whole cluster's reaction to load applied at a subset.
    let all_node_names = testnet.nodes_metadata.node_names();
    let rpc_urls = testnet
        .nodes_metadata
        .to_execution_http_urls(&all_node_names);
    let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();

    let (prometheus_port, _, _) = testnet.infra_data.monitoring_ports();
    let prometheus_url = format!("http://127.0.0.1:{prometheus_port}");

    let ctx = SaturationCtx {
        testnet,
        args,
        block_gas_limit,
        tx_type_mix,
        guzzler_fn_weights,
        erc20_fn_weights,
        tx_input_size,
        out_dir,
        deadline,
        num_generators,
        max_accounts,
        spammer_nodes,
        target_ws_urls,
        rpc_urls,
        metrics_urls,
        prometheus_url,
        is_remote: testnet.is_remote(),
        remote_state_dir: "saturation_state",
    };

    Ok((meta, ctx))
}

async fn preflight_checks(testnet: &Testnet) -> Result<()> {
    // The testnet must already be set up and running. We never start or stop
    // the testnet here — operators control that lifecycle with `quake start`
    // and `quake clean`. `is_setup` works for both local and remote testnets.
    testnet.infra.is_setup(&[]).wrap_err_with(|| {
        format!(
            "Testnet at {} is not set up. Start it first with `quake start` (add `--remote` for AWS).",
            testnet.dir.display()
        )
    })?;
    info!("Using running testnet at {}", testnet.dir.display());
    testnet.wait_rounds(3, Duration::from_secs(120)).await?;

    // Pre-experiment readiness gate: confirm the in-container tc netem rules
    // match the manifest's latency_emulation expectation. A multi-region
    // saturation run on a flat (no-tc-rules) cluster, or vice versa, would
    // otherwise produce results that look fine but don't reflect the intended
    // network conditions.
    info!("Verifying latency-emulation setup matches manifest...");
    testnet
        .run_tests(
            "infra:latency_emulation",
            false,
            Duration::from_secs(30),
            &crate::tests::TestParams::default(),
        )
        .await
        .wrap_err("pre-experiment latency_emulation check failed")?;
    Ok(())
}

/// Run an actual-load warmup at the first rate so every account cycles
/// once before the first measured phase begins. Replaces the earlier
/// hard-coded sleep and eliminates the per-phase nonce-warmup dead zone
/// that arises when nonces are queried lazily inside `next_tx()`.
async fn run_rampup_phase(ctx: &SaturationCtx<'_>, first_rate: u64) -> Result<PhaseLoopState> {
    info!(
        "Ramp-up: running at {} TPS for {}s to warm generators and txpool...",
        first_rate,
        ctx.args.rampup.as_secs()
    );
    let warmup_config = spammer::Config {
        num_generators: ctx.num_generators,
        partition_mode: spammer::PartitionMode::Linear,
        max_num_accounts: ctx.max_accounts,
        preinit_accounts: false,
        query_latest_nonce: true,
        max_num_txs: 0,
        max_rate: first_rate,
        max_time: ctx.args.rampup.as_secs(),
        tx_input_size: ctx.tx_input_size,
        max_txs_per_account: 0,
        silent: false,
        show_pool_status: false,
        tx_latency: false,
        wait_response: false,
        fire_and_forget: true,
        reconnect_attempts: 3,
        reconnect_period: Duration::from_secs(3),
        tx_type_mix: ctx.tx_type_mix,
        guzzler_fn_weights: ctx.guzzler_fn_weights,
        erc20_fn_weights: ctx.erc20_fn_weights,
        csv_dir: None,
    };
    warmup_config.validate()?;

    // Remote-mode state persistence: the spammer subprocess on each phase
    // writes its captured SpammerState to a JSON file on CC; the next phase's
    // subprocess reads it back via --state-in. Avoids the BIP32-derivation +
    // nonce-query startup cost (~50s for 50k accounts) at the start of every
    // phase. The path is tracked across iterations of the phase loop.
    let remote_ramp_up_state = format!("{}/ramp-up.json", ctx.remote_state_dir);
    let mut state = PhaseLoopState {
        generator_state: None,
        remote_state_in_path: None,
    };
    if ctx.is_remote {
        let warmup_args = build_phase_spammer_args(
            &warmup_config,
            ctx.tx_type_mix,
            ctx.guzzler_fn_weights,
            ctx.erc20_fn_weights,
            None,
            None,
            None,
            Some(remote_ramp_up_state.clone()),
        );
        run_phase_remote(ctx.testnet, warmup_args, &ctx.spammer_nodes, None).await?;
        state.remote_state_in_path = Some(remote_ramp_up_state);
    } else {
        let warmup_load = spammer::Spammer::new(ctx.target_ws_urls.clone(), &warmup_config).await?;
        state.generator_state = Some(warmup_load.run_capturing_state().await?.state);
    }

    // Wait for the mempool to drain naturally before the first measured phase.
    // Clear any residue if the drain times out so the first phase starts on
    // an empty mempool (the unconditional per-phase resync handles cache
    // alignment regardless).
    info!(
        "Ramp-up complete. Draining mempool (up to {}s) before first phase...",
        ctx.args.cooldown.as_secs()
    );
    if !wait_for_mempool_drain(&ctx.rpc_urls, ctx.args.cooldown).await {
        warn!(
            "Ramp-up: mempool did not drain within {}s — clearing before first phase",
            ctx.args.cooldown.as_secs()
        );
        clear_mempools(&ctx.rpc_urls).await;
    }
    Ok(state)
}

/// Run a single rate phase end-to-end: spammer load, Prometheus-aligned
/// metric collection, mempool drain, latency CSV parse, and saturation-signal
/// detection. Returns the full [`PhaseRecord`] for the caller to append.
async fn run_one_phase(
    ctx: &SaturationCtx<'_>,
    rate: u64,
    prev_phase: Option<&PhaseRecord>,
    state: &mut PhaseLoopState,
) -> Result<PhaseRecord> {
    let phase_started_at = Utc::now();
    info!(
        "Phase {rate} TPS: running for {}s",
        ctx.args.phase_duration.as_secs()
    );

    // Per-phase CSV directory; the spammer creates a timestamped file inside
    let phase_csv_dir = ctx.out_dir.join(format!("phase_{rate}"));

    let spammer_config = spammer::Config {
        num_generators: ctx.num_generators,
        partition_mode: spammer::PartitionMode::Linear,
        max_num_accounts: ctx.max_accounts,
        preinit_accounts: false,
        query_latest_nonce: true,
        max_num_txs: 0,
        max_rate: rate,
        max_time: ctx.args.phase_duration.as_secs(),
        tx_input_size: ctx.tx_input_size,
        max_txs_per_account: 0,
        silent: false,
        show_pool_status: false,
        tx_latency: true,
        wait_response: false,
        fire_and_forget: true,
        reconnect_attempts: 3,
        reconnect_period: Duration::from_secs(3),
        tx_type_mix: ctx.tx_type_mix,
        guzzler_fn_weights: ctx.guzzler_fn_weights,
        erc20_fn_weights: ctx.erc20_fn_weights,
        csv_dir: Some(phase_csv_dir.clone()),
    };
    spammer_config.validate()?;
    let raw_before = fetch_all_metrics(&ctx.metrics_urls).await;
    let load_started_at = Utc::now();

    let phase_outcome = if ctx.is_remote {
        let phase_dir = format!("saturation_csvs/phase_{rate}");
        let phase_state_out = format!("{}/phase_{rate}.json", ctx.remote_state_dir);
        let phase_args = build_phase_spammer_args(
            &spammer_config,
            ctx.tx_type_mix,
            ctx.guzzler_fn_weights,
            ctx.erc20_fn_weights,
            Some(phase_dir.clone()),
            Some(format!("{phase_dir}/summary.json")),
            state.remote_state_in_path.clone(),
            Some(phase_state_out.clone()),
        );
        let outcome = run_phase_remote(
            ctx.testnet,
            phase_args,
            &ctx.spammer_nodes,
            Some(&phase_csv_dir),
        )
        .await?;
        // Next phase reads this phase's state file as its --state-in.
        state.remote_state_in_path = Some(phase_state_out);
        outcome
    } else {
        let load = spammer::Spammer::new_resuming(
            ctx.target_ws_urls.clone(),
            state
                .generator_state
                .take()
                .expect("local mode keeps state"),
            &spammer::ResumeConfig::from(&spammer_config),
        )
        .await?;
        let (outcome, returned_state) = run_phase_capturing_state(load).await?;
        state.generator_state = Some(returned_state);
        outcome
    };
    let PhaseOutcome {
        rpc_errors,
        actual_offered_tps,
        actual_offered_bytes_per_sec,
        started_at_unix_ms: spammer_started_at_unix_ms,
        finished_at_unix_ms: spammer_finished_at_unix_ms,
    } = phase_outcome;
    let load_ended_at = Utc::now();
    let wall_clock_phase_secs =
        (load_ended_at - load_started_at).num_milliseconds() as f64 / 1000.0;
    // The spammer-reported window (started→finished, in unix ms) excludes
    // setup overhead (SSH session, nonce-warmup, CSV SCP-back in remote
    // mode). Using it for both the chain-rate denominator and the
    // Prometheus query window keeps numerator and denominator aligned.
    let spammer_elapsed =
        ((spammer_finished_at_unix_ms - spammer_started_at_unix_ms) as f64 / 1000.0).max(0.0);
    let phase_duration_secs = if spammer_elapsed > 0.0 {
        spammer_elapsed
    } else {
        wall_clock_phase_secs
    };

    // Prometheus snapshot after load (before cooldown to capture steady state).
    // Query EL CPU/memory concurrently — they use the Prometheus API, not a direct scrape.
    // Also query chain-side TPS and gas/s aligned with the spammer's exact
    // window (started_at..finished_at), so numerator and denominator share
    // the same time interval and we avoid wall-clock setup-overhead bias.
    let eval_time_secs = if spammer_finished_at_unix_ms > 0 {
        spammer_finished_at_unix_ms / 1000
    } else {
        load_ended_at.timestamp()
    };
    // Reth exposes per-subpool `_pool_transactions` (count) and
    // `_pool_size_bytes` (cumulative wire size) as Prometheus gauges. Read
    // all of them so the table can show "count(MB)" for every subpool —
    // pending/queued count is duplicated with the RPC sample, but having
    // size paired up matters for diagnosing which limit dimension is
    // actually binding when "txpool is full" fires.
    let pq = |pool: &'static str, metric: &'static str| {
        query_subpool_peak(
            &ctx.prometheus_url,
            pool,
            metric,
            phase_duration_secs,
            eval_time_secs,
        )
    };
    let aq = |pool: &'static str, metric: &'static str| {
        query_subpool_avg(
            &ctx.prometheus_url,
            pool,
            metric,
            phase_duration_secs,
            eval_time_secs,
        )
    };
    let (
        raw_after,
        (el_cpu_avg_pct, el_cpu_max_pct),
        (cl_cpu_avg_pct, cl_cpu_max_pct),
        (el_mem_avg_mb, el_mem_peak_mb),
        pool_evictions,
        chain_tps_aligned,
        chain_gas_per_sec_aligned,
        block_time_aligned,
        block_build_time_aligned_s,
        block_finalize_time_aligned_s,
        fill_pct_aligned,
        max_pending_mempool_prom,
        avg_pending_mempool_prom,
        max_queued_mempool_prom,
        avg_queued_mempool_prom,
        max_basefee_mempool,
        avg_basefee_mempool,
        max_blob_mempool,
        avg_blob_mempool,
        max_pending_size_bytes,
        avg_pending_size_bytes,
        max_basefee_size_bytes,
        avg_basefee_size_bytes,
        max_queued_size_bytes,
        avg_queued_size_bytes,
        max_blob_size_bytes,
        avg_blob_size_bytes,
    ) = join!(
        fetch_all_metrics(&ctx.metrics_urls),
        query_el_cpu_pct(&ctx.prometheus_url, phase_duration_secs, eval_time_secs),
        query_cl_cpu_pct(&ctx.prometheus_url, phase_duration_secs, eval_time_secs),
        query_el_memory_mb(&ctx.prometheus_url, eval_time_secs),
        query_pool_evictions(&ctx.prometheus_url, phase_duration_secs, eval_time_secs),
        query_chain_actual_tps(&ctx.prometheus_url, phase_duration_secs, eval_time_secs),
        query_chain_gas_per_sec(&ctx.prometheus_url, phase_duration_secs, eval_time_secs),
        query_block_histogram_avg(
            &ctx.prometheus_url,
            "arc_malachite_app_block_time",
            phase_duration_secs,
            eval_time_secs,
        ),
        query_block_histogram_avg(
            &ctx.prometheus_url,
            "arc_malachite_app_block_build_time",
            phase_duration_secs,
            eval_time_secs,
        ),
        query_block_histogram_avg(
            &ctx.prometheus_url,
            "arc_malachite_app_block_finalize_time",
            phase_duration_secs,
            eval_time_secs,
        ),
        query_block_fill_pct_aligned(
            &ctx.prometheus_url,
            phase_duration_secs,
            eval_time_secs,
            ctx.block_gas_limit,
        ),
        pq("pending", "transactions"),
        aq("pending", "transactions"),
        pq("queued", "transactions"),
        aq("queued", "transactions"),
        pq("basefee", "transactions"),
        aq("basefee", "transactions"),
        pq("blob", "transactions"),
        aq("blob", "transactions"),
        pq("pending", "size_bytes"),
        aq("pending", "size_bytes"),
        pq("basefee", "size_bytes"),
        aq("basefee", "size_bytes"),
        pq("queued", "size_bytes"),
        aq("queued", "size_bytes"),
        pq("blob", "size_bytes"),
        aq("blob", "size_bytes"),
    );
    const BYTES_PER_MB: f64 = 1_048_576.0;
    let to_mb = |b: Option<f64>| b.map(|v| v / BYTES_PER_MB);
    let max_pending_size_mb = to_mb(max_pending_size_bytes);
    let avg_pending_size_mb = to_mb(avg_pending_size_bytes);
    let max_basefee_size_mb = to_mb(max_basefee_size_bytes);
    let avg_basefee_size_mb = to_mb(avg_basefee_size_bytes);
    let max_queued_size_mb = to_mb(max_queued_size_bytes);
    let avg_queued_size_mb = to_mb(avg_queued_size_bytes);
    let max_blob_size_mb = to_mb(max_blob_size_bytes);
    let avg_blob_size_mb = to_mb(avg_blob_size_bytes);

    let perf_deltas = parse_perf_metrics_delta(&raw_before, &raw_after);
    let health_before = parse_all_health_metrics(&raw_before);
    let health_after = parse_all_health_metrics(&raw_after);
    let health_deltas = compute_health_deltas(&health_before, &health_after);

    let mut metrics = aggregate_phase_metrics(
        &perf_deltas,
        &health_deltas,
        ctx.block_gas_limit,
        phase_duration_secs,
    );
    // Override the rate-sensitive metrics with the time-aligned versions.
    // The snapshot deltas above cover a slightly wider wall-clock window
    // than the spammer's actual sending window; the Prometheus query is
    // restricted to that exact window via `increase(metric[duration])`.
    if let Some(tps) = chain_tps_aligned {
        metrics.actual_tps = Some(tps);
    }
    if let Some(gas) = chain_gas_per_sec_aligned {
        metrics.gas_per_sec = Some(gas);
    }
    // The snapshot delta spans `raw_before → raw_after` (~340 s including
    // setup + load + teardown), so its block-timing averages get diluted
    // by the fast pre-load and post-load blocks. The Prometheus rate
    // query is restricted to the spammer's actual 180 s load window,
    // matching what the chart shows for the same phase region.
    if let Some(bt) = block_time_aligned {
        metrics.avg_block_time_s = Some(bt);
    }
    if let Some(bld) = block_build_time_aligned_s {
        metrics.avg_block_build_time_ms = Some(bld * 1000.0);
    }
    if let Some(fin) = block_finalize_time_aligned_s {
        metrics.avg_block_finalize_time_ms = Some(fin * 1000.0);
    }
    if let Some(fp) = fill_pct_aligned {
        metrics.fill_pct = Some(fp);
    }
    // Pending and queued count come from Prometheus too, alongside size.
    // Sourcing them from the runner's in-loop `txpool_status` RPC poll
    // was unreliable under load — the busiest node's RPC handler stalls
    // exactly when it has the most queued txs, and the poll silently
    // logged it as zero. Using Prometheus keeps these on the same data
    // source as the per-subpool size and the chart in the HTML report.
    metrics.max_mempool = max_pending_mempool_prom;
    metrics.avg_pending_mempool = avg_pending_mempool_prom;
    metrics.max_queued_mempool = max_queued_mempool_prom;
    metrics.avg_queued_mempool = avg_queued_mempool_prom;
    metrics.max_basefee_mempool = max_basefee_mempool;
    metrics.avg_basefee_mempool = avg_basefee_mempool;
    metrics.max_blob_mempool = max_blob_mempool;
    metrics.avg_blob_mempool = avg_blob_mempool;
    metrics.max_pending_size_mb = max_pending_size_mb;
    metrics.avg_pending_size_mb = avg_pending_size_mb;
    metrics.max_basefee_size_mb = max_basefee_size_mb;
    metrics.avg_basefee_size_mb = avg_basefee_size_mb;
    metrics.max_queued_size_mb = max_queued_size_mb;
    metrics.avg_queued_size_mb = avg_queued_size_mb;
    metrics.max_blob_size_mb = max_blob_size_mb;
    metrics.avg_blob_size_mb = avg_blob_size_mb;
    metrics.pool_evictions = pool_evictions;
    metrics.el_cpu_avg_pct = el_cpu_avg_pct;
    metrics.el_cpu_max_pct = el_cpu_max_pct;
    metrics.cl_cpu_avg_pct = cl_cpu_avg_pct;
    metrics.cl_cpu_max_pct = cl_cpu_max_pct;
    metrics.el_mem_avg_mb = el_mem_avg_mb;
    metrics.el_mem_peak_mb = el_mem_peak_mb;
    metrics.rpc_errors = rpc_errors;
    metrics.actual_offered_tps = Some(actual_offered_tps);
    metrics.actual_offered_bytes_per_sec = Some(actual_offered_bytes_per_sec);

    // Wait for the chain to absorb the phase's in-flight backlog (up to
    // `cooldown`) so the next phase starts on an empty mempool. Clear any
    // residue if the drain times out; the next phase's unconditional
    // resync handles cache alignment either way. Latency CSV parsing runs
    // concurrently because it's local file IO and effectively free.
    info!(
        "Phase {rate} TPS: draining mempool (up to {}s)...",
        ctx.args.cooldown.as_secs()
    );
    let (latency_stats, drained) = join!(
        async {
            let Some(csv_path) = find_latest_csv(&phase_csv_dir) else {
                return (None, None, None, None);
            };
            match read_latency_csv(&csv_path) {
                Ok(latencies) => compute_latency_stats(&latencies),
                Err(e) => {
                    warn!("Could not read latency CSV {}: {e}", csv_path.display());
                    (None, None, None, None)
                }
            }
        },
        wait_for_mempool_drain(&ctx.rpc_urls, ctx.args.cooldown),
    );

    if !drained {
        warn!(
            "Phase {rate} TPS: mempool did not drain within {}s — clearing residue before next phase",
            ctx.args.cooldown.as_secs()
        );
        clear_mempools(&ctx.rpc_urls).await;
    }
    let (avg, stddev, p50, p95) = latency_stats;
    metrics.latency_avg_ms = avg;
    metrics.latency_stddev_ms = stddev;
    metrics.latency_p50_ms = p50;
    metrics.latency_p95_ms = p95;
    let ended_at = Utc::now();

    let mut record = PhaseRecord {
        offered_tps: rate,
        started_at: phase_started_at,
        load_ended_at,
        ended_at,
        metrics,
        signals: vec![],
    };
    record.signals = detect_saturation_signals(prev_phase, &record);
    Ok(record)
}

async fn finalize_experiment(
    ctx: &SaturationCtx<'_>,
    meta: &mut ExperimentMetadata,
    status: ExperimentStatus,
) -> Result<()> {
    meta.status = status;
    meta.ended_at = Some(Utc::now());

    download_metrics_snapshot(ctx.testnet, meta, &ctx.out_dir).await;

    save_experiment_json(&ctx.out_dir, meta)?;
    info!("Experiment complete. Results: {}", ctx.out_dir.display());
    Ok(())
}

// ── Phase lifecycle helpers ───────────────────────────────────────────────────

/// Call `admin_cleartxpool` on every node in parallel.
///
/// Best-effort: per-node failures are logged but never propagated, so a single
/// flaky node can't abort the experiment. The aggregate failure count is logged
/// at debug to keep the phase output clean when nodes briefly stop responding.
async fn clear_mempools(rpc_urls: &[(String, Url)]) {
    use alloy_provider::{Provider, ProviderBuilder};

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client");

    let calls = rpc_urls.iter().map(|(name, url)| {
        let client = client.clone();
        async move {
            let provider = ProviderBuilder::new().connect_reqwest(client, url.clone());
            match provider
                .raw_request::<_, bool>("admin_cleartxpool".into(), ())
                .await
            {
                Ok(_) => true,
                Err(e) => {
                    debug!("admin_cleartxpool on {name} failed: {e}");
                    false
                }
            }
        }
    });
    let results = futures::future::join_all(calls).await;
    let cleared = results.iter().filter(|ok| **ok).count();
    debug!("Cleared mempool on {cleared}/{} nodes", rpc_urls.len());
}

/// Poll `txpool_status` on every node until the aggregate mempool drains, or
/// `timeout` expires. Returns `true` if every reachable node reported
/// `pending == 0 && queued == 0` at the same poll, `false` if the timeout fired.
///
/// Saturation phases that overshoot the chain ceiling leave a residual
/// (queued-pool nonce gap or pending backlog) that would carry into the next
/// phase, biasing its mempool signals and silently diverging the spammer's
/// cached nonces from on-chain state. A natural drain confirms that every
/// in-flight tx executed (or was evicted) so the spammer's cache matches the
/// chain — letting the next phase skip the expensive per-account resync. A
/// timeout is the diagnostic signal that this phase exceeded capacity, and the
/// caller should clear + force a resync to recover before the next phase.
async fn wait_for_mempool_drain(rpc_urls: &[(String, Url)], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let poll_interval = Duration::from_millis(500);
    loop {
        if matches!(check_mempool(rpc_urls).await, Ok(r) if r.passed()) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        sleep(poll_interval.min(deadline - now)).await;
    }
}

/// Aggregated result of a single saturation phase, shared by the local
/// (`run_phase_capturing_state`) and remote (`run_phase_remote`) paths.
///
/// The spamming window (`started_at_unix_ms`..`finished_at_unix_ms`) is
/// surfaced so the caller can align chain-side metrics (Prometheus delta
/// queries) with the actual sending window — excludes the setup overhead
/// bled into wall-clock.
///
struct PhaseOutcome {
    rpc_errors: HashMap<String, u64>,
    actual_offered_tps: f64,
    actual_offered_bytes_per_sec: f64,
    started_at_unix_ms: i64,
    finished_at_unix_ms: i64,
}

/// Run the spammer for one phase and return the outcome plus the generator
/// state so the next phase can resume from it (local mode only —
/// remote-mode state lives in a JSON file on CC).
async fn run_phase_capturing_state(
    load: spammer::Spammer,
) -> Result<(PhaseOutcome, spammer::SpammerState)> {
    let result = load.run_capturing_state().await?;
    Ok((
        PhaseOutcome {
            rpc_errors: result.rpc_errors,
            actual_offered_tps: result.actual_offered_tps,
            actual_offered_bytes_per_sec: result.actual_offered_bytes_per_sec,
            started_at_unix_ms: result.started_at_unix_ms,
            finished_at_unix_ms: result.finished_at_unix_ms,
        },
        result.state,
    ))
}

/// Build `SpammerArgs` from a phase's `spammer::Config` for remote invocation.
#[allow(clippy::too_many_arguments)]
fn build_phase_spammer_args(
    config: &spammer::Config,
    tx_type_mix: spammer::TxTypeMix,
    guzzler_fn_weights: spammer::GuzzlerFnWeights,
    erc20_fn_weights: spammer::Erc20FnWeights,
    csv_dir: Option<String>,
    summary_json: Option<String>,
    state_in: Option<String>,
    state_out: Option<String>,
) -> SpammerArgs {
    SpammerArgs {
        num_generators: config.num_generators,
        max_num_accounts: config.max_num_accounts,
        partition_mode: config.partition_mode,
        num_txs: config.max_num_txs,
        rate: config.max_rate,
        time: config.max_time,
        tx_input_size: config.tx_input_size,
        max_txs_per_account: config.max_txs_per_account,
        preinit_accounts: config.preinit_accounts,
        query_latest_nonce: config.query_latest_nonce,
        silent: config.silent,
        show_pool_status: config.show_pool_status,
        tx_latency: config.tx_latency,
        csv_dir: csv_dir.map(PathBuf::from),
        summary_json: summary_json.map(PathBuf::from),
        state_in: state_in.map(PathBuf::from),
        state_out: state_out.map(PathBuf::from),
        wait_response: config.wait_response,
        reconnect_attempts: config.reconnect_attempts,
        reconnect_period: config.reconnect_period,
        tx_type_mix: Some(tx_type_mix),
        guzzler_fn_weights,
        erc20_fn_weights: Some(erc20_fn_weights),
    }
}

/// Run one phase on the Control Center via SSH. Mempool stats are sourced
/// from Prometheus post-phase, not from RPC sampling here — `txpool_status`
/// silently returns zero on stalled-under-load nodes, exactly the regime
/// we're trying to measure.
async fn run_phase_remote(
    testnet: &Testnet,
    spammer_args: SpammerArgs,
    spammer_nodes: &[String],
    local_csv_dir: Option<&Path>,
) -> Result<PhaseOutcome> {
    let infra = testnet.remote_infra()?;

    let mut cli_args = spammer_args.to_cli_args();
    if !spammer_nodes.is_empty() {
        cli_args.push("--targets".to_string());
        cli_args.push(spammer_nodes.join(","));
    }
    let cmd_parts = crate::load::build_remote_spammer_cmd(&testnet.manifest, &cli_args, true)?;
    let cmd = cmd_parts.join(" ");

    let remote_csv_dir = spammer_args
        .csv_dir
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());
    let remote_summary_path = spammer_args
        .summary_json
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());
    // Ensure the spammer can write its outputs — create the parent dir for
    // each configured path on CC. The summary file's parent is what the
    // spammer's main writes to; the CSV dir is what the latency CSV needs.
    let mut mkdir_paths: Vec<String> = Vec::new();
    if let Some(ref dir) = remote_csv_dir {
        mkdir_paths.push(dir.clone());
    }
    if let Some(ref path) = remote_summary_path {
        if let Some(parent) = Path::new(path).parent() {
            let p = parent.to_string_lossy().into_owned();
            if !p.is_empty() && !mkdir_paths.contains(&p) {
                mkdir_paths.push(p);
            }
        }
    }
    if !mkdir_paths.is_empty() {
        let cmd = format!("mkdir -p {}", mkdir_paths.join(" "));
        infra.ssh_cc(&cmd, false)?;
    }

    let phase_prefix = format!("spammer@{}TPS", spammer_args.rate);
    info!("{phase_prefix}: dispatching spammer subprocess on CC");
    let dispatch_start = Instant::now();

    let infra_for_ssh = infra.clone();
    let cmd_for_ssh = cmd.clone();
    let prefix_for_ssh = phase_prefix.clone();
    let stdout = tokio::task::spawn_blocking(move || {
        infra_for_ssh.ssh_cc_with_streaming_output(&cmd_for_ssh, &prefix_for_ssh)
    })
    .await
    .wrap_err("spawn_blocking for spammer SSH panicked")?
    .wrap_err("remote spammer exited with error")?;
    info!(
        "{phase_prefix}: subprocess returned after {:.1}s",
        dispatch_start.elapsed().as_secs_f64()
    );

    if let (Some(remote_dir), Some(local_dir)) = (remote_csv_dir.as_deref(), local_csv_dir) {
        if let Err(e) = std::fs::create_dir_all(local_dir) {
            warn!(
                "Could not create local CSV dir {}: {e}",
                local_dir.display()
            );
        }
        let pattern = format!("{remote_dir}/tx_latency_*.csv");
        let scp_csv_start = Instant::now();
        if let Err(e) = infra.scp_from_cc(&pattern, local_dir) {
            warn!("SCP of latency CSV from CC ({pattern}) failed: {e}");
        } else {
            info!(
                "{phase_prefix}: SCP'd latency CSVs in {:.1}s",
                scp_csv_start.elapsed().as_secs_f64()
            );
        }
    }

    // Warmup runs without a summary_json (the metrics it produces are
    // discarded), so there's no file to fetch. Real phases always set it.
    let (rpc_errors, actual_tps, actual_bps, started_at_unix_ms, finished_at_unix_ms) =
        match remote_summary_path.as_deref() {
            Some(path) => {
                let scp_summary_start = Instant::now();
                let summary = read_spammer_summary(&infra, path).wrap_err_with(|| {
                    format!(
                        "could not recover spammer summary from CC\n--- spammer stdout (tail) ---\n{}",
                        stdout_tail(&stdout, 60)
                    )
                })?;
                info!(
                    "{phase_prefix}: SCP'd summary in {:.1}s",
                    scp_summary_start.elapsed().as_secs_f64()
                );
                (
                    summary.rpc_errors,
                    summary.actual_offered_tps,
                    summary.actual_offered_bytes_per_sec,
                    summary.started_at_unix_ms,
                    summary.finished_at_unix_ms,
                )
            }
            None => (HashMap::new(), 0.0, 0.0, 0, 0),
        };

    Ok(PhaseOutcome {
        rpc_errors,
        actual_offered_tps: actual_tps,
        actual_offered_bytes_per_sec: actual_bps,
        started_at_unix_ms,
        finished_at_unix_ms,
    })
}

/// SCP `summary.json` from the spammer's CSV directory on CC and deserialize it.
///
/// Returns an error if the file is missing or malformed — the saturation
/// runner can't compute phase metrics without it, so failing loudly is the
/// right behavior. The wrapped stdout tail makes diagnosis easier when the
/// spammer crashed before writing the summary.
fn read_spammer_summary(
    infra: &crate::infra::remote::RemoteInfra,
    remote_summary_path: &str,
) -> Result<spammer::SpammerSummary> {
    let tmp = tempfile::Builder::new()
        .prefix("quake-spammer-summary-")
        .tempdir()
        .wrap_err("could not create temp dir for spammer summary")?;
    infra
        .scp_from_cc(remote_summary_path, tmp.path())
        .wrap_err_with(|| format!("scp summary from CC ({remote_summary_path}) failed"))?;
    let file_name = Path::new(remote_summary_path)
        .file_name()
        .ok_or_else(|| eyre!("remote summary path has no file name: {remote_summary_path}"))?;
    let local_path = tmp.path().join(file_name);
    let bytes = std::fs::read(&local_path)
        .wrap_err_with(|| format!("read {} failed", local_path.display()))?;
    serde_json::from_slice::<spammer::SpammerSummary>(&bytes)
        .wrap_err("deserialize spammer summary JSON")
}

fn stdout_tail(stdout: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = stdout.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

/// Download a targeted Prometheus metrics snapshot for the experiment window.
///
/// Works identically for local and remote testnets — both expose Prometheus
/// at `127.0.0.1:<port>` (Docker port locally, SSM-tunnelled port remotely),
/// so a single Rust HTTP path handles both. Best-effort: errors are logged
/// and swallowed so a missing tarball never fails the experiment.
async fn download_metrics_snapshot(testnet: &Testnet, meta: &ExperimentMetadata, out_dir: &Path) {
    let Some(ended_at) = meta.ended_at else {
        warn!("No ended_at timestamp — skipping metrics download");
        return;
    };
    let duration_secs = (ended_at - meta.started_at).num_seconds().max(1);
    let step_secs = (duration_secs / 1000).max(15);
    let step = if step_secs < 60 {
        format!("{step_secs}s")
    } else {
        format!("{}m", step_secs / 60)
    };
    let (prometheus_port, _, _) = testnet.infra_data.monitoring_ports();
    let prometheus_url = format!("http://127.0.0.1:{prometheus_port}");
    let dest = out_dir.join("metrics.tar.gz");

    info!("Downloading metrics...");
    if let Err(e) = crate::metrics::download_to_tarball(
        &prometheus_url,
        DOWNLOAD_METRICS,
        Some(meta.started_at.timestamp()),
        Some(ended_at.timestamp()),
        Some(&step),
        &dest,
    )
    .await
    {
        warn!("Could not download metrics: {e}");
    }
}

fn save_experiment_json(out_dir: &Path, meta: &ExperimentMetadata) -> Result<()> {
    let path = out_dir.join("experiment.json");
    let json = serde_json::to_string_pretty(meta)?;
    write_atomic(&json, &path).wrap_err_with(|| format!("write {}", path.display()))
}

/// Write a file via tempfile + rename so a crash mid-write can't leave a
/// truncated `experiment.json` behind. Mirrors the spammer's pattern for
/// `summary.json` / `state.json`.
fn write_atomic(contents: &str, path: &Path) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(p) = parent {
        fs::create_dir_all(p).wrap_err_with(|| format!("create parent dir {}", p.display()))?;
    }
    let dir = parent.unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .wrap_err_with(|| format!("create tempfile in {}", dir.display()))?;
    tmp.write_all(contents.as_bytes())
        .wrap_err("write tempfile")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))
            .wrap_err("chmod tempfile")?;
    }
    tmp.persist(path)
        .map_err(|e| eyre!("persist tempfile to {}: {}", path.display(), e))?;
    Ok(())
}

/// Return the path to the most recently modified `tx_latency_*.csv` in `dir`.
fn find_latest_csv(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("tx_latency_") && n.ends_with(".csv"))
                .unwrap_or(false)
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// Upper bound on the number of phases a parsed `--rates` may expand to.
/// Defense against typos like `1-1000000:1` that would produce a million
/// phases — saturation experiments are normally 10–30.
const MAX_RATES_EXPANDED: usize = 256;

/// Parse one comma-delimited token of `--rates`. Accepts either a bare
/// integer (`1500`) or an inclusive range with step (`1000-2000:100`).
///
/// Range invariants enforced: `start > 0`, `end > start` (strict — use the
/// bare integer form for a single rate), `step > 0`, `step <= end - start`
/// (step must fit in the range), and `(end - start) % step == 0` (so the
/// endpoint `end` is actually reached and isn't silently dropped).
fn parse_rate_token(token: &str) -> Result<Vec<u64>> {
    let token = token.trim();
    let Some((range_part, step_part)) = token.split_once(':') else {
        let v: u64 = token
            .parse()
            .map_err(|e| eyre!("invalid rate '{token}': {e}"))?;
        if v == 0 {
            bail!("rate '{token}' must be > 0");
        }
        return Ok(vec![v]);
    };
    let (start_str, end_str) = range_part
        .split_once('-')
        .ok_or_else(|| eyre!("range '{token}' must be START-END:STEP"))?;
    let start: u64 = start_str
        .trim()
        .parse()
        .map_err(|e| eyre!("invalid start '{start_str}' in range '{token}': {e}"))?;
    let end: u64 = end_str
        .trim()
        .parse()
        .map_err(|e| eyre!("invalid end '{end_str}' in range '{token}': {e}"))?;
    let step: u64 = step_part
        .trim()
        .parse()
        .map_err(|e| eyre!("invalid step '{step_part}' in range '{token}': {e}"))?;
    if start == 0 {
        bail!("range '{token}' has start = 0; rates must be > 0");
    }
    if step == 0 {
        bail!("range '{token}' has step = 0");
    }
    if end <= start {
        bail!(
            "range '{token}' has end ({end}) <= start ({start}); use the bare integer form for a single rate"
        );
    }
    let span = end - start;
    if step > span {
        bail!(
            "range '{token}' has step ({step}) > end - start ({span}); step must fit in the range"
        );
    }
    if !span.is_multiple_of(step) {
        bail!(
            "range '{token}' has step ({step}) that does not evenly divide end - start ({span}); endpoint {end} would be silently dropped"
        );
    }
    let mut out = Vec::new();
    let mut v = start;
    loop {
        out.push(v);
        let Some(next) = v.checked_add(step) else {
            break;
        };
        if next > end {
            break;
        }
        v = next;
    }
    Ok(out)
}

fn parse_rates(rates_str: &str) -> Result<Vec<u64>> {
    let mut rates: Vec<u64> = Vec::new();
    for token in rates_str.split(',') {
        rates.extend(parse_rate_token(token)?);
    }
    if rates.is_empty() {
        bail!("--rates must not be empty");
    }
    if rates.len() > MAX_RATES_EXPANDED {
        bail!(
            "--rates expanded to {} phases, capped at {MAX_RATES_EXPANDED}",
            rates.len()
        );
    }
    if rates.windows(2).any(|w| w[0] >= w[1]) {
        bail!("--rates must be strictly ascending, got: {rates_str}");
    }
    Ok(rates)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rates_accepts_comma_list() {
        assert_eq!(
            parse_rates("500,1000,2000,4000").unwrap(),
            vec![500, 1000, 2000, 4000]
        );
    }

    #[test]
    fn parse_rates_accepts_range_with_step() {
        assert_eq!(
            parse_rates("1000-2000:100").unwrap(),
            vec![1000, 1100, 1200, 1300, 1400, 1500, 1600, 1700, 1800, 1900, 2000]
        );
    }

    #[test]
    fn parse_rates_mixes_singles_and_ranges() {
        assert_eq!(
            parse_rates("500,1000-1200:100,4000").unwrap(),
            vec![500, 1000, 1100, 1200, 4000]
        );
    }

    #[test]
    fn parse_rates_rejects_step_not_dividing_range() {
        let err = parse_rates("1000-2050:100").unwrap_err().to_string();
        assert!(err.contains("evenly divide"), "unexpected: {err}");
    }

    #[test]
    fn parse_rates_rejects_start_equals_end() {
        let err = parse_rates("1000-1000:100").unwrap_err().to_string();
        assert!(err.contains("bare integer"), "unexpected: {err}");
    }

    #[test]
    fn parse_rates_rejects_step_larger_than_range() {
        let err = parse_rates("1000-2000:5000").unwrap_err().to_string();
        assert!(err.contains("step must fit"), "unexpected: {err}");
    }

    #[test]
    fn parse_rates_rejects_zero_rate() {
        let err = parse_rates("0").unwrap_err().to_string();
        assert!(err.contains("must be > 0"), "unexpected: {err}");
    }

    #[test]
    fn parse_rates_rejects_zero_start() {
        let err = parse_rates("0-100:10").unwrap_err().to_string();
        assert!(err.contains("start = 0"), "unexpected: {err}");
    }

    #[test]
    fn parse_rates_rejects_zero_step() {
        let err = parse_rates("1000-2000:0").unwrap_err().to_string();
        assert!(err.contains("step = 0"), "unexpected: {err}");
    }

    #[test]
    fn parse_rates_rejects_start_greater_than_end() {
        let err = parse_rates("2000-1000:100").unwrap_err().to_string();
        // Folded into the "end <= start" guard along with start == end.
        assert!(
            err.contains("end") && err.contains("start"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn parse_rates_rejects_too_many_phases() {
        // 1-300:1 would expand to 300 rates; cap is MAX_RATES_EXPANDED = 256.
        let err = parse_rates("1-300:1").unwrap_err().to_string();
        assert!(err.contains("capped"), "unexpected: {err}");
    }

    #[test]
    fn parse_rates_rejects_non_ascending_after_expansion() {
        // 1500 falls inside the expanded range 1000..=2000, so the final
        // sequence has a non-strict-ascending pair.
        let err = parse_rates("1000-2000:100,1500").unwrap_err().to_string();
        assert!(err.contains("ascending"), "unexpected: {err}");
    }

    #[test]
    fn parse_rates_rejects_malformed_range_missing_dash() {
        let err = parse_rates("1000:100").unwrap_err().to_string();
        assert!(err.contains("START-END:STEP"), "unexpected: {err}");
    }

    fn phase(
        offered: u64,
        gas: Option<f64>,
        tps: Option<f64>,
        p95: Option<f64>,
        mempool: Option<f64>,
    ) -> PhaseRecord {
        let now = Utc::now();
        PhaseRecord {
            offered_tps: offered,
            started_at: now,
            load_ended_at: now,
            ended_at: now,
            metrics: PhaseMetrics {
                gas_per_sec: gas,
                actual_tps: tps,
                max_mempool: mempool,
                latency_p95_ms: p95,
                ..Default::default()
            },
            signals: vec![],
        }
    }

    // ── detect_saturation_signals ─────────────────────────────────────────────

    #[test]
    fn first_phase_never_signals() {
        let current = phase(500, Some(14_000_000.0), Some(450.0), Some(40.0), Some(0.0));
        assert!(detect_saturation_signals(None, &current).is_empty());
    }

    #[test]
    fn healthy_growth_no_signals() {
        let prev = phase(500, Some(14_000_000.0), Some(450.0), Some(40.0), Some(0.0));
        let current = phase(1000, Some(28_000_000.0), Some(900.0), Some(45.0), Some(0.0));
        let sigs = detect_saturation_signals(Some(&prev), &current);
        assert!(sigs.is_empty(), "unexpected signals: {sigs:?}");
    }

    #[test]
    fn gas_plateaued_fires_when_gas_barely_grows() {
        let prev = phase(1000, Some(28_000_000.0), Some(900.0), Some(45.0), Some(0.0));
        // Gas grew by only 3% — below PLATEAU_THRESHOLD (10%)
        let current = phase(2000, Some(28_840_000.0), Some(920.0), Some(50.0), Some(0.0));
        let sigs = detect_saturation_signals(Some(&prev), &current);
        assert!(
            sigs.iter()
                .any(|s| matches!(s, SaturationSignal::GasPlateaued)),
            "GasPlateaued expected but not found in {sigs:?}"
        );
    }

    #[test]
    fn tps_plateaued_fires_when_tps_barely_grows() {
        let prev = phase(1000, Some(28_000_000.0), Some(900.0), Some(45.0), Some(0.0));
        // TPS grew by only 2%
        let current = phase(2000, Some(31_000_000.0), Some(918.0), Some(50.0), Some(0.0));
        let sigs = detect_saturation_signals(Some(&prev), &current);
        assert!(
            sigs.iter()
                .any(|s| matches!(s, SaturationSignal::TpsPlateaued)),
            "TpsPlateaued expected but not found in {sigs:?}"
        );
    }

    #[test]
    fn tps_ratio_drop_fires_when_ratio_falls_sharply() {
        // prev: 900 / 1000 = 0.90 ratio
        let prev = phase(1000, Some(28_000_000.0), Some(900.0), Some(45.0), Some(0.0));
        // current: 850 / 4000 = 0.2125 ratio — well below 0.90 * (1 - 0.15)
        let current = phase(4000, Some(30_000_000.0), Some(850.0), Some(60.0), Some(0.0));
        let sigs = detect_saturation_signals(Some(&prev), &current);
        assert!(
            sigs.iter()
                .any(|s| matches!(s, SaturationSignal::TpsRatioDrop)),
            "TpsRatioDrop expected but not found in {sigs:?}"
        );
    }

    #[test]
    fn latency_spike_fires_when_p95_doubles() {
        let prev = phase(
            1000,
            Some(28_000_000.0),
            Some(900.0),
            Some(100.0),
            Some(0.0),
        );
        // p95 went from 100ms to 250ms — more than 2×
        let current = phase(
            2000,
            Some(30_000_000.0),
            Some(910.0),
            Some(250.0),
            Some(0.0),
        );
        let sigs = detect_saturation_signals(Some(&prev), &current);
        assert!(
            sigs.iter()
                .any(|s| matches!(s, SaturationSignal::LatencySpike)),
            "LatencySpike expected but not found in {sigs:?}"
        );
    }

    #[test]
    fn latency_spike_does_not_fire_below_threshold() {
        let prev = phase(
            1000,
            Some(28_000_000.0),
            Some(900.0),
            Some(100.0),
            Some(0.0),
        );
        // p95 went from 100ms to 190ms — less than 2×
        let current = phase(
            2000,
            Some(30_000_000.0),
            Some(910.0),
            Some(190.0),
            Some(0.0),
        );
        let sigs = detect_saturation_signals(Some(&prev), &current);
        assert!(
            !sigs
                .iter()
                .any(|s| matches!(s, SaturationSignal::LatencySpike)),
            "LatencySpike should not fire at 1.9× growth"
        );
    }

    #[test]
    fn mempool_growth_fires_when_pool_accumulates() {
        let prev = phase(
            1000,
            Some(28_000_000.0),
            Some(900.0),
            Some(45.0),
            Some(50.0),
        );
        // Grew by 1800 — above MEMPOOL_GROWTH_MIN (100)
        let current = phase(
            2000,
            Some(29_000_000.0),
            Some(920.0),
            Some(60.0),
            Some(1850.0),
        );
        let sigs = detect_saturation_signals(Some(&prev), &current);
        assert!(
            sigs.iter()
                .any(|s| matches!(s, SaturationSignal::MempoolGrowth)),
            "MempoolGrowth expected but not found in {sigs:?}"
        );
    }

    #[test]
    fn mempool_growth_does_not_fire_on_small_increase() {
        let prev = phase(
            1000,
            Some(28_000_000.0),
            Some(900.0),
            Some(45.0),
            Some(50.0),
        );
        // Grew by 80 — below MEMPOOL_GROWTH_MIN (100)
        let current = phase(
            2000,
            Some(30_000_000.0),
            Some(950.0),
            Some(48.0),
            Some(130.0),
        );
        let sigs = detect_saturation_signals(Some(&prev), &current);
        assert!(
            !sigs
                .iter()
                .any(|s| matches!(s, SaturationSignal::MempoolGrowth)),
            "MempoolGrowth should not fire on small increase"
        );
    }

    #[test]
    fn multiple_signals_fire_at_saturation() {
        let prev = phase(
            1000,
            Some(28_000_000.0),
            Some(900.0),
            Some(60.0),
            Some(10.0),
        );
        let current = phase(
            2000,
            Some(28_500_000.0),
            Some(905.0),
            Some(250.0),
            Some(5000.0),
        );
        let sigs = detect_saturation_signals(Some(&prev), &current);
        let kinds: Vec<_> = sigs.iter().map(signal_abbrev).collect();
        assert!(
            sigs.len() >= 3,
            "expected ≥3 signals at saturation, got: {kinds:?}"
        );
    }

    // ── compute_latency_stats ───────────────────────────────────────────

    #[test]
    fn latency_empty_returns_none() {
        assert_eq!(compute_latency_stats(&[]), (None, None, None, None));
    }

    #[test]
    fn latency_single_record() {
        // Sample stddev needs n≥2, so for a single value it's None.
        assert_eq!(
            compute_latency_stats(&[150.0]),
            (Some(150.0), None, Some(150.0), Some(150.0))
        );
    }

    #[test]
    fn latency_stats_correct() {
        // 10 values: 10, 20, ..., 100 ms
        let latencies: Vec<f64> = (1..=10u64).map(|i| (i * 10) as f64).collect();
        let (avg, stddev, p50, p95) = compute_latency_stats(&latencies);
        // mean = (10+20+...+100)/10 = 55
        assert_eq!(avg, Some(55.0));
        // sample stddev: variance = sum((x-55)^2)/(10-1) = 825/9 ≈ 91.6667 → sqrt ≈ 9.574 * sqrt(...)
        // Computed directly: sqrt(8250/9) = sqrt(916.67) ≈ 30.276
        let sd = stddev.unwrap();
        assert!((sd - 30.276503).abs() < 1e-3, "stddev was {sd}");
        // p50 index = round(0.5 * 9) = round(4.5) = 5 → sorted[5] = 60
        assert_eq!(p50, Some(60.0));
        // p95 index = round(0.95 * 9) = round(8.55) = 9 → sorted[9] = 100
        assert_eq!(p95, Some(100.0));
    }

    #[test]
    fn latency_sorted_before_percentile() {
        let latencies = vec![300.0, 100.0, 200.0];
        let (_, _, p50, _) = compute_latency_stats(&latencies);
        assert_eq!(p50, Some(200.0));
    }
}
