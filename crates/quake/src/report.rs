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

//! Network testing report generator.
//!
//! Collects mesh, health, performance, and sanity data from a running testnet
//! and produces a structured markdown report.
//!
//! By default the sanity section runs the full `sanity:arc_node` test phases
//! (snapshot recovery, MEV protection, mempool checks, tx forwarding) and the
//! sync speed measurement (destructive — kills and restarts a node).
//! Disable with `--set sanity=false` and/or `--set sync_speed=false` for faster
//! or non-destructive reports.
//!
//! **Remote testnets** (via `quake remote import`): Most sections work via SSM
//! tunnels. The sanity section adapts to remote mode: snapshot recovery is
//! skipped (requires local Docker Compose), tx forwarding uses `quake remote
//! load`, and mempool checks report warnings instead of failures (shared
//! testnets have real traffic).
//!
//! # Usage
//! # Report parameters (via `--set key=value`)
//!
//! | Key                  | Default          | Description                                                |
//! |----------------------|------------------|------------------------------------------------------------|
//! | **Observation & Load** |                |                                                            |
//! | `warmup_s`           | `30`             | Seconds to wait before first Prometheus scrape             |
//! | `duration_s`         | `60`             | Observation window (load duration or quiet sleep)          |
//! | `load_rate`          | `50`             | TPS during observation (0 = no load)                       |
//! | `load_targets`       | `RPC_NODES`      | Comma-separated load selectors (node names and/or manifest `[node_groups]` names, same as `quake load`); default group must exist in the manifest (e.g. `RPC_NODES` on mainnet) |
//! | `load_mix`           | `transfer=100`   | Tx type mix (e.g. `transfer=70,erc20=30`)                  |
//! | **Perf thresholds**  |                  |                                                            |
//! | `block_time_p50_ms`  | `550`            | Fail if any validator node's p50 block time exceeds this             |
//! | `block_time_p99_ms`  | `1000`           | Fail if any validator node's p99 block time exceeds this             |
//! | **Section toggles**  |                  |                                                            |
//! | `sanity`             | `true`           | Run snapshot recovery + MEV + mempool + tx forwarding      |
//! | `sync_speed`         | `true`           | Run sync speed measurement (destructive)                   |
//! | **Sanity**           |                  |                                                            |
//! | `arc_nodes`          | `ARC_NODES` group  | Target nodes for all sanity phases (comma-separated node names and/or `[node_groups]`; default uses the `ARC_NODES` group if defined, else skips) |
//! |                      |                  | *Snapshot recovery*: each arc-node is restored from snapshot|
//! |                      |                  | *MEV protection*: checks `follow_endpoints` of arc-nodes   |
//! |                      |                  | *Mempool empty*: checks all nodes except arc-nodes + relays|
//! |                      |                  | *Tx forwarding*: sends txs to each arc-node, verifies inclusion |
//! | `snapshot_provider`  | `full-circle-5`  | Node to take the snapshot from (Phase 1)                   |
//! | `reference`          | `validator-blue` | Reference node for tip height (sanity + sync speed)   |
//! | **Sync speed**       |                  |                                                            |
//! | `sync_nodes`         | `full-quicknode-1` | Comma-separated node names to stop/restart and measure   |
//! | `sync_min_bps`       | `7.0`            | Min avg blocks/sec to pass                                 |
//! | `sync_timeout_s`     | `180`            | Max measurement duration                                   |
//! | `sync_downtime_s`    | `120`            | Seconds to keep node down before restart                   |
//! | **Store**            |                  |                                                            |
//! | `store_nodes`        | (pruned nodes)   | Comma-separated node names for storage size lookup (malachite) |
//!
//! "follow nodes" (sanity / MEV docs) = manifest nodes with `follow = true`.
//! "pruned nodes" = nodes for which `Node::cl_store_pruning_configured()` is true (CL `store.db`).
//!
//! # Hardcoded (not configurable)
//!
//! These values are fixed in the sanity phase implementations (`arc_node.rs`):
//!
//! | Constant             | Value       | Used by                  | Description                                  |
//! |----------------------|-------------|--------------------------|----------------------------------------------|
//! | `TARGET_HEIGHT`      | `120`       | Snapshot recovery        | Min block height before snapshotting         |
//! | `WAIT_TIMEOUT`       | `600s`      | Snapshot recovery        | Max wait for validators/provider to reach height |
//! | `CATCHUP_TIMEOUT`    | `120s`      | Snapshot recovery        | Max wait for arc-node to catch up after restore |
//! | `RESTART_SETTLE`     | `10s`       | Snapshot recovery        | Sleep after restarting a node                |
//! | `LOAD_NUM_TXS`       | `10`        | Tx forwarding            | Number of transactions sent per arc-node     |
//! | `MEV addr`           | `0xf39F..2266` | MEV protection        | Address used for pending-state RPC checks    |
//! | `pruning_window`     | `100`       | CL store pruning check   | Max certificate records expected             |
//! | `pruning_margin`     | `50`        | CL store pruning check   | Added to window for threshold (total 150)    |
//!
//! # Example commands
//!
//! ```text
//! quake report                                            # defaults: 60s, 50 TPS, all sections
//! quake report --set sanity=false --set sync_speed=false  # metrics only (fast)
//! quake report --set duration_s=600 --set load_rate=0     # 10 min observation, no load
//! quake report --set arc_nodes=arc-1,arc-2                # custom sanity targets
//! quake report --set sync_nodes=arc-node,rpc-full         # measure sync on specific nodes
//! quake report --set store_nodes=snapshot,arc-node        # storage size information for specific nodes
//! quake report -o my-report.md                            # custom output path
//! ```

use std::fmt::Write as _;
use std::time::Duration;

use chrono::Utc;
use color_eyre::eyre::{bail, Result};
use tracing::info;

use url::Url;

use crate::mesh::{analyze, classify_all, fetch_all_metrics as fetch_mesh_metrics, MeshTier};
use crate::testnet::Testnet;
use crate::tests::mesh::{categorize_node, check_strict, NodeCategory};
use crate::tests::TestParams;
use crate::RemoteSubcommand;

const DEFAULT_WARMUP_S: u64 = 30;
const DEFAULT_DURATION_S: u64 = 60;
const DEFAULT_LOAD_RATE: u64 = 50;
const DEFAULT_LOAD_MIX: &str = "transfer=100";
const DEFAULT_P50_MS: u64 = 550;
const DEFAULT_P99_MS: u64 = 1000;
const DEFAULT_REFERENCE: &str = "validator-blue";
const DEFAULT_SNAPSHOT_PROVIDER: &str = "full-circle-5";
const DEFAULT_SYNC_NODE: &str = "full-quicknode-1";
const DEFAULT_SYNC_MIN_BPS: f64 = 7.0;
const DEFAULT_SYNC_TIMEOUT_S: u64 = 180;
const DEFAULT_SYNC_DOWNTIME_S: u64 = 120;

/// When `load_targets` is unset, `quake report` uses this manifest `[node_groups]` name.
const DEFAULT_REPORT_LOAD_TARGETS: &str = "RPC_NODES";

// ── Data structures ─────────────────────────────────────────────────────

/// Top-level report data collected from the testnet.
struct ReportData {
    // Metadata
    quake_version: String,
    quake_commit: String,
    node_versions: Vec<NodeVersion>,
    manifest_name: String,
    manifest_path: String,
    manifest_content: String,
    timestamp: String,
    output_path: String,

    // Effective parameters (resolved defaults + overrides)
    effective_params: Vec<(String, String)>,

    // Section results
    mesh: MeshSection,
    health: HealthSection,
    perf: PerfSection,
    sanity: Option<SanitySection>,
    sync_speed: Vec<SyncSpeedSection>,

    // Node liveness (names of nodes that were down)
    liveness: LivenessSection,

    // Raw info outputs for appendices
    info_mesh: String,
    /// `quake info perf`-style text: delta between two scrapes (observation window).
    info_perf_observation: String,
    /// `quake info perf`-style text: cumulative since process start (final scrape).
    info_perf_cumulative: String,
    /// `quake info health`-style text: cumulative counters at end of window (final scrape).
    info_health_cumulative: String,
    info_store: String,
}

struct NodeVersion {
    name: String,
    arc_version: String,
    reth_version: String,
}

struct LivenessSection {
    total_nodes: usize,
    down_before: Vec<String>,
    down_after: Vec<String>,
}

struct MeshSection {
    entries: Vec<MeshEntry>,
    max_hops: usize,
    max_duplicate_pct: f64,
    passed: bool,
    failures: Vec<String>,
}

struct MeshEntry {
    name: String,
    category: String,
    tier: String,
    passed: bool,
    status_detail: String,
}

struct HealthSection {
    nodes: Vec<HealthEntry>,
    max_decisions: i64,
    any_round_gt0: bool,
    any_restarts: bool,
    any_sync_behind: bool,
    passed: bool,
    failures: Vec<String>,
}

struct HealthEntry {
    name: String,
    decisions: i64,
    round_gt0: i64,
    restarts: i64,
    sync_behind: i64,
    /// Per-node result from `check_health_deltas` (same rules as pass/fail for Health).
    passed: bool,
    /// Short explanation (same as `CheckResult::message` in arc-checks).
    check_detail: String,
}

struct PerfSection {
    p50_threshold_ms: u64,
    p99_threshold_ms: u64,
    nodes: Vec<PerfEntry>,
    validator_names: std::collections::HashSet<String>,
    passed: bool,
    failures: Vec<String>,
}

struct PerfEntry {
    name: String,
    group: String,
    block_time_p50_ms: f64,
    block_time_p95_ms: f64,
    block_time_p99_ms: f64,
    block_count: u64,
    avg_tx_per_block: f64,
    avg_gas_used: f64,
    /// `None` if non-validator (not subject to p50/p99 thresholds in this report).
    threshold_pass: Option<bool>,
    threshold_detail: String,
}

struct SanitySection {
    phases: Vec<SanityPhase>,
    passed: bool,
    failures: Vec<String>,
}

struct SanityPhase {
    name: String,
    passed: bool,
    duration: Duration,
    detail: String,
}

fn skipped_sanity_section(detail: impl Into<String>) -> SanitySection {
    let detail = detail.into();
    SanitySection {
        phases: vec![SanityPhase {
            name: "prerequisites".to_string(),
            passed: true,
            duration: Duration::ZERO,
            detail,
        }],
        passed: true,
        failures: Vec::new(),
    }
}

fn failed_sanity_section(detail: impl Into<String>) -> SanitySection {
    let detail = detail.into();
    SanitySection {
        phases: vec![SanityPhase {
            name: "prerequisites".to_string(),
            passed: false,
            duration: Duration::ZERO,
            detail: detail.clone(),
        }],
        passed: false,
        failures: vec![detail],
    }
}

struct SyncSpeedSection {
    node: String,
    reference: String,
    result: Option<arc_checks::SyncSpeedResult>,
    min_bps: f64,
    passed: bool,
    detail: String,
    duration: Duration,
}

// ── Collection ──────────────────────────────────────────────────────────

async fn collect_report_data(
    testnet: &Testnet,
    params: &TestParams,
    output: &std::path::Path,
) -> Result<ReportData> {
    let warmup_s: u64 = params
        .get_or("warmup_s", &DEFAULT_WARMUP_S.to_string())
        .parse()
        .unwrap_or(DEFAULT_WARMUP_S);
    let duration_s: u64 = params
        .get_or("duration_s", &DEFAULT_DURATION_S.to_string())
        .parse()
        .unwrap_or(DEFAULT_DURATION_S);
    let load_rate: u64 = params
        .get_or("load_rate", &DEFAULT_LOAD_RATE.to_string())
        .parse()
        .unwrap_or(DEFAULT_LOAD_RATE);
    let load_targets_str = params.get_or("load_targets", "");
    let load_mix = params.get_or("load_mix", DEFAULT_LOAD_MIX);
    let p50_ms: u64 = params
        .get_or("block_time_p50_ms", &DEFAULT_P50_MS.to_string())
        .parse()
        .unwrap_or(DEFAULT_P50_MS);
    let p99_ms: u64 = params
        .get_or("block_time_p99_ms", &DEFAULT_P99_MS.to_string())
        .parse()
        .unwrap_or(DEFAULT_P99_MS);

    let load_target_selectors: Vec<String> = if load_targets_str.is_empty() {
        vec![DEFAULT_REPORT_LOAD_TARGETS.to_string()]
    } else {
        load_targets_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let load_desc = if load_rate > 0 {
        let resolved = testnet
            .manifest
            .resolve_node_selectors(&load_target_selectors)?;
        let s = load_target_selectors.join(", ");
        format!(
            "{load_rate} TPS ({load_mix}) → {s} ({} node(s) after group expansion)",
            resolved.len()
        )
    } else {
        "none".to_string()
    };

    let manifest_content = std::fs::read_to_string(&testnet.manifest_path)
        .unwrap_or_else(|_| "(could not read manifest file)".to_string());

    let manifest_name = testnet
        .manifest
        .name
        .clone()
        .unwrap_or_else(|| testnet.name.clone());

    info!(
        warmup_s,
        duration_s, load_rate, %load_desc,
        "Collecting report data"
    );

    // ── Liveness check (before) ────────────────────────────────────
    let node_urls = testnet.nodes_metadata.all_execution_urls();
    let heights_before = crate::rpc::fetch_latest_heights(&node_urls).await;
    let down_before: Vec<String> = heights_before
        .iter()
        .filter(|(_, r)| r.is_err())
        .map(|(name, _)| name.clone())
        .collect();
    if !down_before.is_empty() {
        info!(
            "⚠ {} node(s) unreachable before report: {}",
            down_before.len(),
            down_before.join(", ")
        );
    }

    // ── Warmup ──────────────────────────────────────────────────────
    if warmup_s > 0 {
        info!("Warming up ({warmup_s}s)...");
        tokio::time::sleep(Duration::from_secs(warmup_s)).await;
    }

    // ── Health baseline ─────────────────────────────────────────────
    let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();
    let raw_before = arc_checks::fetch_all_metrics(&metrics_urls).await;
    let mut health_before = arc_checks::parse_all_health_metrics(&raw_before);

    if health_before.is_empty() {
        bail!("No health metrics collected from any node (scrape 1)");
    }
    crate::util::assign_node_groups(
        health_before
            .iter_mut()
            .map(|n| (n.name.as_str(), &mut n.group)),
        &testnet.manifest.nodes,
    );

    // ── Load / observation ──────────────────────────────────────────
    if load_rate > 0 {
        info!("Introducing load: {load_desc} for {duration_s}s");
        if testnet.is_remote() {
            let args = crate::tests::sanity::build_remote_load_args(
                load_rate,
                duration_s,
                &load_mix,
                &load_target_selectors,
            );
            testnet.remote(RemoteSubcommand::Load { args }).await?;
        } else {
            let config =
                crate::tests::sanity::build_spammer_config(load_rate, duration_s, &load_mix)?;
            crate::load::load(testnet, load_target_selectors.clone(), &config).await?;
        }
    } else {
        info!("Observing for {duration_s}s (no load)");
        tokio::time::sleep(Duration::from_secs(duration_s)).await;
    }

    // ── Health scrape 2 ─────────────────────────────────────────────
    let raw_after = arc_checks::fetch_all_metrics(&metrics_urls).await;
    let mut health_after = arc_checks::parse_all_health_metrics(&raw_after);

    if health_after.is_empty() {
        bail!("No health metrics collected from any node (scrape 2)");
    }
    crate::util::assign_node_groups(
        health_after
            .iter_mut()
            .map(|n| (n.name.as_str(), &mut n.group)),
        &testnet.manifest.nodes,
    );

    let health_section = build_health_section(&health_before, &health_after);

    // ── Performance ─────────────────────────────────────────────────
    let perf_section = build_perf_section(&raw_before, &raw_after, testnet, p50_ms, p99_ms);

    // ── Mesh ────────────────────────────────────────────────────────
    let mesh_section = build_mesh_section(testnet).await?;

    // ── Sanity (snapshot recovery + MEV + mempool + tx forwarding) ──
    let run_sanity = params.get_or("sanity", "true") == "true";
    let sanity_section = if run_sanity {
        info!("Running sanity phases (snapshot recovery, MEV, mempool, tx forwarding)...");
        Some(run_sanity_phases(testnet, params).await)
    } else {
        info!("Sanity phases skipped (--set sanity=false)");
        None
    };

    // ── Sync speed (destructive — stops/restarts a node) ────────────
    let run_sync = params.get_or("sync_speed", "true") == "true";
    let sync_speed_sections = if run_sync {
        info!("Running sync speed tests...");
        run_sync_speed_all(testnet, params).await
    } else {
        info!("Sync speed skipped (--set sync_speed=false)");
        Vec::new()
    };

    // ── Liveness check (after) ─────────────────────────────────────
    let heights_after = crate::rpc::fetch_latest_heights(&node_urls).await;
    let down_after: Vec<String> = heights_after
        .iter()
        .filter(|(_, r)| r.is_err())
        .map(|(name, _)| name.clone())
        .collect();
    if !down_after.is_empty() {
        info!(
            "⚠ {} node(s) unreachable after report: {}",
            down_after.len(),
            down_after.join(", ")
        );
    }
    {
        let newly_down: Vec<_> = down_after
            .iter()
            .filter(|n| !down_before.contains(n))
            .map(|s| s.as_str())
            .collect();
        if !newly_down.is_empty() {
            info!(
                "⚠ {} node(s) went down during the report: {}",
                newly_down.len(),
                newly_down.join(", ")
            );
        }
    }

    let liveness = LivenessSection {
        total_nodes: node_urls.len(),
        down_before,
        down_after,
    };

    // ── Raw info outputs for appendices ──────────────────────────────
    let (
        info_mesh,
        info_perf_observation,
        info_perf_cumulative,
        info_health_cumulative,
        info_store,
    ) = collect_info_appendices(testnet, params, &raw_before, &raw_after, duration_s).await;

    // Resolve effective values for all parameters so the report is reproducible.
    let reference = params.get_or("reference", DEFAULT_REFERENCE);
    let arc_nodes_resolved = crate::tests::arc_node::resolve_arc_nodes(testnet, params)
        .map(|names| names.join(","))
        .unwrap_or_default();
    let sync_nodes_resolved = {
        let p = params.get_or("sync_nodes", "");
        if p.is_empty() {
            default_sync_nodes().join(",")
        } else {
            p
        }
    };
    let store_nodes_resolved = {
        let p = params.get_or("store_nodes", "");
        if p.is_empty() {
            testnet
                .manifest
                .nodes
                .iter()
                .filter(|(_, node)| node.cl_store_pruning_configured())
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
                .join(",")
        } else {
            p
        }
    };
    let snapshot_provider = params.get_or("snapshot_provider", DEFAULT_SNAPSHOT_PROVIDER);

    let mut effective_params: Vec<(String, String)> = vec![
        ("warmup_s".into(), warmup_s.to_string()),
        ("duration_s".into(), duration_s.to_string()),
        ("load_rate".into(), load_rate.to_string()),
        ("load_targets".into(), load_target_selectors.join(",")),
        ("load_mix".into(), load_mix.clone()),
        ("block_time_p50_ms".into(), p50_ms.to_string()),
        ("block_time_p99_ms".into(), p99_ms.to_string()),
        ("sanity".into(), run_sanity.to_string()),
    ];
    if run_sanity || run_sync {
        effective_params.push(("reference".into(), reference.clone()));
    }
    if run_sanity {
        effective_params.push(("arc_nodes".into(), arc_nodes_resolved));
        effective_params.push(("snapshot_provider".into(), snapshot_provider));
    }
    effective_params.push(("sync_speed".into(), run_sync.to_string()));
    if run_sync {
        effective_params.push(("sync_nodes".into(), sync_nodes_resolved));
        effective_params.push((
            "sync_min_bps".into(),
            params.get_or("sync_min_bps", &DEFAULT_SYNC_MIN_BPS.to_string()),
        ));
        effective_params.push((
            "sync_timeout_s".into(),
            params.get_or("sync_timeout_s", &DEFAULT_SYNC_TIMEOUT_S.to_string()),
        ));
        effective_params.push((
            "sync_downtime_s".into(),
            params.get_or("sync_downtime_s", &DEFAULT_SYNC_DOWNTIME_S.to_string()),
        ));
    }
    if !store_nodes_resolved.is_empty() {
        effective_params.push(("store_nodes".into(), store_nodes_resolved));
    }

    // ── Collect per-node versions (EL + CL) ────────────────────────
    let node_versions = collect_node_versions(testnet).await;

    Ok(ReportData {
        quake_version: arc_version::GIT_VERSION.to_string(),
        quake_commit: arc_version::GIT_COMMIT_HASH[..8].to_string(),
        node_versions,
        manifest_name,
        manifest_path: testnet.manifest_path.display().to_string(),
        manifest_content,
        timestamp: Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        output_path: output.display().to_string(),
        effective_params,
        mesh: mesh_section,
        health: health_section,
        perf: perf_section,
        sanity: sanity_section,
        sync_speed: sync_speed_sections,
        liveness,
        info_mesh,
        info_perf_observation,
        info_perf_cumulative,
        info_health_cumulative,
        info_store,
    })
}

fn build_health_section(
    before: &[arc_checks::NodeHealthData],
    after: &[arc_checks::NodeHealthData],
) -> HealthSection {
    let deltas = arc_checks::compute_health_deltas(before, after);
    let report = arc_checks::check_health_deltas(&deltas);

    let mut max_decisions = 0i64;
    let mut any_round_gt0 = false;
    let mut any_restarts = false;
    let mut any_sync_behind = false;
    let mut nodes = Vec::new();

    for d in &deltas {
        let round_gt0 = d.delta_round_1 + d.delta_round_gt1;
        max_decisions = max_decisions.max(d.delta_decisions);
        if round_gt0 > 0 {
            any_round_gt0 = true;
        }
        if d.delta_height_restarts > 0 {
            any_restarts = true;
        }
        if d.delta_sync_fell_behind > 0 {
            any_sync_behind = true;
        }
        let check = report.checks.iter().find(|c| c.name == d.name);
        let (passed, check_detail) = if let Some(c) = check {
            (c.passed, c.message.clone())
        } else {
            (true, "internal: no matching check row for node".to_string())
        };
        nodes.push(HealthEntry {
            name: d.name.clone(),
            decisions: d.delta_decisions,
            round_gt0,
            restarts: d.delta_height_restarts,
            sync_behind: d.delta_sync_fell_behind,
            passed,
            check_detail,
        });
    }

    let failures: Vec<String> = report
        .checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| format!("{}: {}", c.name, c.message))
        .collect();

    HealthSection {
        nodes,
        max_decisions,
        any_round_gt0,
        any_restarts,
        any_sync_behind,
        passed: report.passed(),
        failures,
    }
}

fn build_perf_section(
    raw_before: &[(String, String)],
    raw_after: &[(String, String)],
    testnet: &Testnet,
    p50_ms: u64,
    p99_ms: u64,
) -> PerfSection {
    let perf_nodes = crate::util::parse_perf_metrics_delta_with_groups(
        raw_before,
        raw_after,
        &testnet.manifest.nodes,
    );

    let validator_names: std::collections::HashSet<String> = testnet
        .manifest
        .validator_names()
        .into_iter()
        .map(|n| n.to_string())
        .collect();
    let val_before: Vec<_> = raw_before
        .iter()
        .filter(|(name, _)| validator_names.contains(name.as_str()))
        .cloned()
        .collect();
    let val_after: Vec<_> = raw_after
        .iter()
        .filter(|(name, _)| validator_names.contains(name.as_str()))
        .cloned()
        .collect();

    let perf_report = arc_checks::check_block_time_delta(&val_before, &val_after, p50_ms, p99_ms);

    let check_by_name: std::collections::HashMap<&str, &arc_checks::CheckResult> = perf_report
        .checks
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();

    let failures: Vec<String> = perf_report
        .checks
        .iter()
        .filter(|c| !c.passed)
        .map(|c| format!("{}: {}", c.name, c.message))
        .collect();

    let nodes: Vec<PerfEntry> = perf_nodes
        .iter()
        .map(|n| {
            let bt = n.block_time.as_ref();
            let tx = n.block_tx_count.as_ref();
            let gas = n.block_gas_used.as_ref();
            let is_validator = validator_names.contains(n.name.as_str());
            let (threshold_pass, threshold_detail) = if is_validator {
                if let Some(c) = check_by_name.get(n.name.as_str()) {
                    (Some(c.passed), c.message.clone())
                } else {
                    (
                        Some(true),
                        "no threshold check row (unexpected for validator)".to_string(),
                    )
                }
            } else {
                (
                    None,
                    "not thresholded (non-validator; shown for context)".to_string(),
                )
            };
            PerfEntry {
                name: n.name.clone(),
                group: n.group.clone().unwrap_or_default(),
                block_time_p50_ms: bt.map(|s| s.p50 * 1000.0).unwrap_or(0.0),
                block_time_p95_ms: bt.map(|s| s.p95 * 1000.0).unwrap_or(0.0),
                block_time_p99_ms: bt.map(|s| s.p99 * 1000.0).unwrap_or(0.0),
                block_count: bt.map(|s| s.count).unwrap_or(0),
                avg_tx_per_block: tx.map(|s| s.avg).unwrap_or(0.0),
                avg_gas_used: gas.map(|s| s.avg).unwrap_or(0.0),
                threshold_pass,
                threshold_detail,
            }
        })
        .collect();

    PerfSection {
        p50_threshold_ms: p50_ms,
        p99_threshold_ms: p99_ms,
        passed: perf_report.passed(),
        failures,
        nodes,
        validator_names,
    }
}

async fn build_mesh_section(testnet: &Testnet) -> Result<MeshSection> {
    let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();
    let raw_metrics = fetch_mesh_metrics(&metrics_urls).await;
    let nodes_data = crate::mesh::parse_and_classify_metrics(&raw_metrics, &testnet.manifest.nodes);
    if nodes_data.is_empty() {
        bail!("No mesh metrics collected from any node");
    }

    let analysis = analyze(&nodes_data);
    let classifications = classify_all(&analysis);

    let has_external_validators = classifications.iter().any(|(moniker, _, _)| {
        testnet
            .manifest
            .nodes
            .get(moniker.as_str())
            .map(|n| categorize_node(moniker, n, testnet) == NodeCategory::ExternalValidator)
            .unwrap_or(false)
    });

    let mut max_hops: usize = 0;
    let mut max_duplicate_pct: f64 = 0.0;
    let mut entries = Vec::new();
    let mut failures = Vec::new();

    for (moniker, _lib_node_type, tier) in &classifications {
        let category = testnet
            .manifest
            .nodes
            .get(moniker.as_str())
            .map(|n| categorize_node(moniker, n, testnet))
            .unwrap_or(NodeCategory::Excluded);

        let (passed, status_detail) = if category == NodeCategory::Excluded {
            (true, "skipped".to_string())
        } else {
            match check_strict(category, *tier, has_external_validators) {
                Ok(detail) => (true, detail),
                Err(reason) => {
                    failures.push(format!("{moniker} ({category}): {reason}"));
                    (false, reason)
                }
            }
        };

        if *tier == MeshTier::MultiHop {
            max_hops = max_hops.max(2);
        }

        entries.push(MeshEntry {
            name: moniker.clone(),
            category: category.to_string(),
            tier: tier.to_string(),
            passed,
            status_detail,
        });
    }

    for node in &nodes_data {
        let pct = node.message_counts.duplicate_pct();
        if pct > max_duplicate_pct {
            max_duplicate_pct = pct;
        }
    }

    // Compute max diameter from validator connectivity
    for vc in &analysis.validator_connectivity {
        max_hops = max_hops.max(vc.max_diameter);
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(MeshSection {
        passed: failures.is_empty(),
        entries,
        max_hops,
        max_duplicate_pct,
        failures,
    })
}

// ── Version summary rendering ────────────────────────────────────────────

/// Render node versions as a compact summary: groups by (arc, reth) version pair,
/// shows the majority as "All nodes on version ..." and lists outliers individually.
fn render_node_versions_summary(out: &mut String, versions: &[NodeVersion]) {
    use std::collections::HashMap;

    let mut groups: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
    for nv in versions {
        groups
            .entry((&nv.arc_version, &nv.reth_version))
            .or_default()
            .push(&nv.name);
    }

    if groups.len() == 1 {
        let ((arc, reth), _) = groups.into_iter().next().unwrap();
        let _ = writeln!(
            out,
            "All {} nodes on version `{arc}` (reth `{reth}`)",
            versions.len()
        );
        return;
    }

    let mut sorted: Vec<_> = groups.into_iter().collect();
    sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let ((maj_arc, maj_reth), maj_nodes) = sorted.remove(0);
    let _ = writeln!(
        out,
        "{} node(s) on version `{maj_arc}` (reth `{maj_reth}`)",
        maj_nodes.len()
    );
    let _ = writeln!(out, "\nExceptions:\n");
    let _ = writeln!(out, "| Node | Arc Version | Reth Version |");
    let _ = writeln!(out, "|------|-------------|--------------|");
    for ((arc, reth), nodes) in &sorted {
        for name in nodes {
            let _ = writeln!(out, "| {name} | {arc} | {reth} |");
        }
    }
}

// ── Markdown rendering ──────────────────────────────────────────────────

fn render_markdown(data: &ReportData) -> String {
    let mut out = String::with_capacity(8192);

    // ── Header ──────────────────────────────────────────────────────
    let _ = writeln!(out, "# Network Testing Report\n");
    let _ = writeln!(out, "| Field | Value |");
    let _ = writeln!(out, "|-------|-------|");
    let _ = writeln!(out, "| Date | {} |", data.timestamp);
    let _ = writeln!(out, "| Quake Version | `{}` |", data.quake_version);
    let _ = writeln!(out, "| Quake Commit | `{}` |", data.quake_commit);
    let _ = writeln!(out, "| Manifest | `{}` |", data.manifest_name);

    if !data.node_versions.is_empty() {
        let _ = writeln!(out, "\n### Node Versions\n");
        render_node_versions_summary(&mut out, &data.node_versions);
    }

    let _ = writeln!(out, "\n### Parameters\n");
    let _ = writeln!(out, "| Parameter | Value |");
    let _ = writeln!(out, "|-----------|-------|");
    for (key, value) in &data.effective_params {
        let _ = writeln!(out, "| `{key}` | {value} |");
    }

    // ── Summary ─────────────────────────────────────────────────────
    let _ = writeln!(out, "\n## Summary\n");
    let _ = writeln!(out, "| Section | Result | Details |");
    let _ = writeln!(out, "|---------|--------|---------|");

    let mesh_detail = format_summary_with_failures(
        &format_mesh_summary(&data.mesh),
        data.mesh.passed,
        &data.mesh.failures,
    );
    let health_detail = format_summary_with_failures(
        &format_health_summary(&data.health),
        data.health.passed,
        &data.health.failures,
    );
    let perf_detail = format_summary_with_failures(
        &format_perf_summary(&data.perf),
        data.perf.passed,
        &data.perf.failures,
    );

    let _ = writeln!(
        out,
        "| Mesh | {} | {} |",
        status_icon(data.mesh.passed),
        mesh_detail
    );
    let _ = writeln!(
        out,
        "| Health | {} | {} |",
        status_icon(data.health.passed),
        health_detail
    );
    let _ = writeln!(
        out,
        "| Performance | {} | {} |",
        status_icon(data.perf.passed),
        perf_detail
    );
    if let Some(ref sanity) = data.sanity {
        for phase in &sanity.phases {
            let _ = writeln!(
                out,
                "| Sanity: {} | {} | {} ({:.0}s) |",
                phase.name,
                status_icon(phase.passed),
                phase.detail,
                phase.duration.as_secs_f64(),
            );
        }
    }
    for sync in &data.sync_speed {
        let _ = writeln!(
            out,
            "| Sync Speed ({}) | {} | {} |",
            sync.node,
            status_icon(sync.passed),
            format_sync_speed_summary(sync),
        );
    }

    // ── Liveness note ───────────────────────────────────────────────
    render_liveness_note(&mut out, &data.liveness);

    // ── Mesh section ────────────────────────────────────────────────
    let _ = writeln!(
        out,
        "\n## Mesh Topology — {}\n",
        status_icon(data.mesh.passed)
    );
    render_mesh_details(&mut out, &data.mesh);

    // ── Health section ──────────────────────────────────────────────
    let _ = writeln!(out, "\n## Health — {}\n", status_icon(data.health.passed));
    render_health_details(&mut out, &data.health);

    // ── Performance section ─────────────────────────────────────────
    let _ = writeln!(
        out,
        "\n## Performance — {}\n",
        status_icon(data.perf.passed)
    );
    let _ = writeln!(
        out,
        "Pass/fail uses **validator** block-time thresholds only (see table below). Other nodes are listed for context.\n"
    );
    render_perf_details(&mut out, &data.perf);

    // ── Sanity section ────────────────────────────────────────────────
    if let Some(ref sanity) = data.sanity {
        let _ = writeln!(
            out,
            "\n## Sanity (Arc Node) — {}\n",
            status_icon(sanity.passed)
        );
        render_sanity_details(&mut out, sanity);
    }

    // ── Sync Speed section ─────────────────────────────────────────
    if !data.sync_speed.is_empty() {
        let all_sync_passed = data.sync_speed.iter().all(|s| s.passed);
        let _ = writeln!(out, "\n## Sync Speed — {}\n", status_icon(all_sync_passed));
        for sync in &data.sync_speed {
            let _ = writeln!(out, "### {} — {}\n", sync.node, status_icon(sync.passed));
            render_sync_speed_details(&mut out, sync);
            let _ = writeln!(out);
        }
    }

    // ── Failures ────────────────────────────────────────────────────
    let mut all_failures: Vec<(&str, &[String])> = vec![
        ("Mesh", data.mesh.failures.as_slice()),
        ("Health", data.health.failures.as_slice()),
        ("Performance", data.perf.failures.as_slice()),
    ];
    if let Some(ref sanity) = data.sanity {
        all_failures.push(("Sanity", sanity.failures.as_slice()));
    }
    let sync_failures: Vec<String> = data
        .sync_speed
        .iter()
        .filter(|s| !s.passed)
        .map(|s| format!("{}: {}", s.node, s.detail))
        .collect();
    if !sync_failures.is_empty() {
        all_failures.push(("Sync Speed", &sync_failures));
    }
    let all_failures: Vec<(&str, &[String])> = all_failures
        .into_iter()
        .filter(|(_, f)| !f.is_empty())
        .collect();

    if !all_failures.is_empty() {
        let _ = writeln!(out, "\n## Failures\n");
        for (section, failures) in all_failures {
            let _ = writeln!(out, "### {section}\n");
            for f in failures {
                let _ = writeln!(out, "- {f}");
            }
            let _ = writeln!(out);
        }
    }

    // ── Appendices ───────────────────────────────────────────────────
    let _ = writeln!(out, "\n---\n");
    let _ = writeln!(out, "# Appendices\n");

    let _ = writeln!(out, "## Appendix A: Mesh (`quake info mesh`)\n");
    let _ = writeln!(out, "```\n{}\n```", data.info_mesh.trim());

    let _ = writeln!(out, "\n## Appendix B: Performance (`quake info perf`)\n");
    let _ = writeln!(
        out,
        "> The **Performance** section above uses **observation-window** (delta) data for pass/fail. \
         This appendix includes both the window view and **cumulative since process start** so you can compare two reports (e.g. different `duration_s`, load, or manifests).\n"
    );
    let _ = writeln!(out, "### B.1 Observation window (delta between scrapes)\n");
    let _ = writeln!(out, "```\n{}\n```", data.info_perf_observation.trim());
    let _ = writeln!(out, "\n### B.2 Cumulative (since process start)\n");
    let _ = writeln!(out, "```\n{}\n```", data.info_perf_cumulative.trim());

    let _ = writeln!(out, "\n## Appendix C: Health (`quake info health`)\n");
    let _ = writeln!(
        out,
        "> The **Health** section above uses **observation-window** (delta) data for pass/fail. \
         This appendix shows **cumulative** counters at the end of the run (since process start), \
         so you can compare two reports (e.g. different `duration_s`, load, or manifests).\n"
    );
    let _ = writeln!(out, "```\n{}\n```", data.info_health_cumulative.trim());

    let _ = writeln!(out, "\n## Appendix D: Store (`quake info store`)\n");
    let _ = writeln!(out, "{}", data.info_store.trim());

    let _ = writeln!(out, "\n## Appendix E: Manifest\n");
    let _ = writeln!(out, "**Path:** `{}`\n", data.manifest_path);
    let _ = writeln!(out, "```toml\n{}\n```", data.manifest_content.trim());

    let _ = writeln!(out, "\n## Appendix F: Reproduce\n");
    let _ = write!(out, "```\nquake report");
    for (key, value) in &data.effective_params {
        if value.contains(' ') || value.contains(',') {
            let _ = write!(out, " \\\n  --set '{key}={value}'");
        } else {
            let _ = write!(out, " \\\n  --set {key}={value}");
        }
    }
    let _ = writeln!(out, " \\\n  -o {}", data.output_path);
    let _ = writeln!(out, "```");

    out
}

fn status_icon(passed: bool) -> &'static str {
    if passed {
        "PASS"
    } else {
        "FAIL"
    }
}

fn render_liveness_note(out: &mut String, liveness: &LivenessSection) {
    let all_healthy = liveness.down_before.is_empty() && liveness.down_after.is_empty();
    if all_healthy {
        let _ = writeln!(
            out,
            "\n> **Node Liveness**: All {} nodes healthy\n",
            liveness.total_nodes
        );
        return;
    }

    let _ = writeln!(out, "\n> **Node Liveness**");

    if !liveness.down_before.is_empty() {
        let _ = writeln!(
            out,
            "> - Down **before** report: {}",
            liveness.down_before.join(", "),
        );
    }
    if !liveness.down_after.is_empty() {
        let newly_down: Vec<_> = liveness
            .down_after
            .iter()
            .filter(|n| !liveness.down_before.contains(n))
            .map(|s| s.as_str())
            .collect();
        let still_down: Vec<_> = liveness
            .down_after
            .iter()
            .filter(|n| liveness.down_before.contains(n))
            .map(|s| s.as_str())
            .collect();
        if !newly_down.is_empty() {
            let _ = writeln!(
                out,
                "> - Went down **during** report: {}",
                newly_down.join(", "),
            );
        }
        if !still_down.is_empty() {
            let _ = writeln!(
                out,
                "> - Still down **after** report: {}",
                still_down.join(", "),
            );
        }
    }
    let recovered: Vec<_> = liveness
        .down_before
        .iter()
        .filter(|n| !liveness.down_after.contains(n))
        .map(|s| s.as_str())
        .collect();
    if !recovered.is_empty() {
        let _ = writeln!(
            out,
            "> - **Recovered** during report: {}",
            recovered.join(", "),
        );
    }
    let _ = writeln!(out);
}

/// When a section failed, append the first few [`failures`] entries so the Summary
/// table explains *why* without reading the `## Failures` section. Values are
/// single-lined and `|`-safe for markdown tables.
fn format_summary_with_failures(summary: &str, passed: bool, failures: &[String]) -> String {
    if passed || failures.is_empty() {
        return summary.to_string();
    }
    let mut note: String = failures
        .iter()
        .take(2)
        .map(|f| f.replace('\n', " ").replace('|', "/"))
        .collect::<Vec<_>>()
        .join("; ");
    if failures.len() > 2 {
        use std::fmt::Write;
        let _ = write!(
            &mut note,
            "; +{} more (see ## Failures)",
            failures.len() - 2
        );
    }
    format!("{summary} — {note}")
}

fn format_mesh_summary(mesh: &MeshSection) -> String {
    let total = mesh.entries.len();
    let excluded = mesh
        .entries
        .iter()
        .filter(|e| e.category == "excluded")
        .count();
    let validators = mesh
        .entries
        .iter()
        .filter(|e| e.category.contains("validator"))
        .count();
    let fully_connected = mesh
        .entries
        .iter()
        .filter(|e| e.tier == "fully-connected" && e.category != "excluded")
        .count();
    let active = total - excluded;

    format!(
        "{fully_connected}/{active} fully connected, {validators} validators, max {max_hops} hops, dup {dup:.1}%",
        max_hops = mesh.max_hops,
        dup = mesh.max_duplicate_pct,
    )
}

fn format_health_summary(health: &HealthSection) -> String {
    let node_count = health.nodes.len();
    let mut parts = Vec::new();
    parts.push(format!(
        "{} heights observed across {node_count} nodes",
        health.max_decisions
    ));
    if health.any_round_gt0 {
        let count = health.nodes.iter().filter(|n| n.round_gt0 > 0).count();
        parts.push(format!("{count} node(s) with round>0"));
    } else {
        parts.push("all round-0".into());
    }
    if health.any_restarts {
        let count = health.nodes.iter().filter(|n| n.restarts > 0).count();
        parts.push(format!("{count} node(s) restarted"));
    } else {
        parts.push("no restarts".into());
    }
    if health.any_sync_behind {
        let count = health.nodes.iter().filter(|n| n.sync_behind > 0).count();
        parts.push(format!("{count} node(s) fell behind"));
    } else {
        parts.push("no sync-behind".into());
    }
    parts.join(", ")
}

fn format_perf_summary(perf: &PerfSection) -> String {
    let validators = perf
        .nodes
        .iter()
        .filter(|n| perf.validator_names.contains(&n.name));
    let worst_p50 = validators
        .clone()
        .map(|n| n.block_time_p50_ms)
        .fold(0.0f64, f64::max);
    let worst_p99 = validators
        .map(|n| n.block_time_p99_ms)
        .fold(0.0f64, f64::max);
    format!(
        "validators worst p50={:.0}ms (threshold {}ms), worst p99={:.0}ms (threshold {}ms)",
        worst_p50, perf.p50_threshold_ms, worst_p99, perf.p99_threshold_ms
    )
}

fn render_sanity_details(out: &mut String, sanity: &SanitySection) {
    let _ = writeln!(out, "| Phase | Result | Duration | Details |");
    let _ = writeln!(out, "|-------|--------|----------|---------|");
    for phase in &sanity.phases {
        let _ = writeln!(
            out,
            "| {} | {} | {:.1}s | {} |",
            phase.name,
            status_icon(phase.passed),
            phase.duration.as_secs_f64(),
            phase.detail,
        );
    }
}

fn format_sync_speed_summary(sync: &SyncSpeedSection) -> String {
    match &sync.result {
        Some(r) => {
            let status = if r.caught_up { "caught up" } else { "partial" };
            format!(
                "{}: {:.1} blk/s ({status}, {} blocks in {:.0}s, min {:.1})",
                sync.node,
                r.avg_bps,
                r.total_blocks,
                r.elapsed.as_secs_f64(),
                sync.min_bps,
            )
        }
        None => sync.detail.clone(),
    }
}

fn render_sync_speed_details(out: &mut String, sync: &SyncSpeedSection) {
    let _ = writeln!(out, "| Field | Value |");
    let _ = writeln!(out, "|-------|-------|");
    let _ = writeln!(out, "| Node | {} |", sync.node);
    let _ = writeln!(out, "| Reference | {} |", sync.reference);
    let _ = writeln!(out, "| Min required | {:.1} blk/s |", sync.min_bps);
    let _ = writeln!(
        out,
        "| Total duration | {:.1}s |",
        sync.duration.as_secs_f64()
    );

    if let Some(ref r) = sync.result {
        let _ = writeln!(out, "| Start height | {} |", r.start_height);
        let _ = writeln!(out, "| Target height | {} |", r.target_height);
        let _ = writeln!(out, "| Final height | {} |", r.final_height);
        let _ = writeln!(out, "| Blocks synced | {} |", r.total_blocks);
        let _ = writeln!(out, "| Sync time | {:.1}s |", r.elapsed.as_secs_f64());
        let _ = writeln!(out, "| Avg speed | {:.1} blk/s |", r.avg_bps);
        let _ = writeln!(
            out,
            "| Caught up | {} |",
            if r.caught_up { "yes" } else { "no" }
        );
        let _ = writeln!(out, "| Result | {} |", status_icon(sync.passed));
    } else {
        let _ = writeln!(out, "| Result | {} — {} |", status_icon(false), sync.detail);
    }
}

fn render_mesh_details(out: &mut String, mesh: &MeshSection) {
    let _ = writeln!(out, "### Checks\n");
    let _ = writeln!(out, "- **Circle validators** must be **FullyConnected** unless any **external** validator is in the manifest, in which case **MultiHop** is allowed (indirect paths to external validators).");
    let _ = writeln!(out, "- **External validators** must be reachable (not `NotConnected`); **MultiHop** is ok behind a sentry.");
    let _ = writeln!(
        out,
        "- **Other consensus** (non-validator) nodes should be connected (not `NotConnected`)."
    );
    let _ = writeln!(
        out,
        "- **Excluded** nodes are not considered for mesh health.\n"
    );

    // Group by category for a compact summary
    let mut by_category: Vec<(&str, Vec<&MeshEntry>)> = Vec::new();
    for entry in &mesh.entries {
        if let Some((_, vec)) = by_category
            .iter_mut()
            .find(|(c, _)| *c == entry.category.as_str())
        {
            vec.push(entry);
        } else {
            by_category.push((&entry.category, vec![entry]));
        }
    }

    for (category, entries) in &by_category {
        let tier_summary: Vec<String> = {
            let mut counts: Vec<(&str, usize)> = Vec::new();
            for e in entries {
                if let Some((_, c)) = counts.iter_mut().find(|(t, _)| *t == e.tier.as_str()) {
                    *c += 1;
                } else {
                    counts.push((&e.tier, 1));
                }
            }
            counts
                .iter()
                .map(|(tier, count)| format!("{count} {tier}"))
                .collect()
        };
        let _ = writeln!(
            out,
            "- **{category}** ({} nodes): {}",
            entries.len(),
            tier_summary.join(", ")
        );
    }

    let _ = writeln!(out, "- **Max hop distance**: {}", mesh.max_hops);
    let _ = writeln!(
        out,
        "- **Max duplicate rate**: {:.1}%",
        mesh.max_duplicate_pct
    );

    let _ = writeln!(out, "\n### Per-Node Classification\n");
    let _ = writeln!(out, "| Node | Category | Tier | Result | Detail |");
    let _ = writeln!(out, "|------|----------|------|--------|--------|");
    for entry in &mesh.entries {
        let (result, detail) = if entry.passed {
            (status_icon(true), entry.status_detail.clone())
        } else {
            (status_icon(false), entry.status_detail.clone())
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            entry.name, entry.category, entry.tier, result, detail
        );
    }
}

fn render_health_details(out: &mut String, health: &HealthSection) {
    let _ = writeln!(out, "### Checks\n");
    let _ = writeln!(out, "Per node, deltas over the observation window: **R>0 = 0** (all decisions at round 0), **height restarts = 0**, **sync fell behind = 0**. A negative `decisions` delta is treated as pass/skipped (likely restart or counter reset).\n");
    let _ = writeln!(
        out,
        "Largest `decisions` delta in the window (any node): **{}**.\n",
        health.max_decisions
    );

    let _ = writeln!(
        out,
        "| Node | Decisions (Δ) | Round >0 (Δ) | Restarts (Δ) | Sync behind (Δ) | Result | Notes |"
    );
    let _ = writeln!(
        out,
        "|------|--------------:|-------------:|-------------:|----------------:|--------|-------|"
    );
    for entry in &health.nodes {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} |",
            entry.name,
            entry.decisions,
            entry.round_gt0,
            entry.restarts,
            entry.sync_behind,
            status_icon(entry.passed),
            entry.check_detail
        );
    }
}

fn render_perf_details(out: &mut String, perf: &PerfSection) {
    let _ = writeln!(out, "### Checks\n");
    let _ = writeln!(
        out,
        "Validators only (same as report pass/fail): block time **p50** < **{}** ms and **p99** < **{}** ms, using **deltas** between the two scrapes. Other nodes: **N/A** in the result column (not thresholded here).\n",
        perf.p50_threshold_ms, perf.p99_threshold_ms
    );

    let _ = writeln!(out, "#### Block Time\n");
    let _ = writeln!(
        out,
        "| Node | Group | p50 (ms) | p95 (ms) | p99 (ms) | Blocks | Result | Notes |"
    );
    let _ = writeln!(
        out,
        "|------|-------|----------:|----------:|----------:|-------:|--------|-------|"
    );
    for entry in &perf.nodes {
        let (res_col, note) = match entry.threshold_pass {
            None => ("N/A".to_string(), entry.threshold_detail.clone()),
            Some(true) => (
                status_icon(true).to_string(),
                entry.threshold_detail.clone(),
            ),
            Some(false) => (
                status_icon(false).to_string(),
                entry.threshold_detail.clone(),
            ),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {:.0} | {:.0} | {:.0} | {} | {} | {} |",
            entry.name,
            entry.group,
            entry.block_time_p50_ms,
            entry.block_time_p95_ms,
            entry.block_time_p99_ms,
            entry.block_count,
            res_col,
            note
        );
    }

    let has_throughput = perf.nodes.iter().any(|n| n.avg_tx_per_block > 0.0);
    if has_throughput {
        let _ = writeln!(out, "\n#### Throughput (informational)\n");
        let _ = writeln!(out, "| Node | Avg Tx/Block | Avg Gas Used |");
        let _ = writeln!(out, "|------|-------------:|-------------:|");
        for entry in &perf.nodes {
            let _ = writeln!(
                out,
                "| {} | {:.1} | {:.0} |",
                entry.name, entry.avg_tx_per_block, entry.avg_gas_used
            );
        }
    }
}

// ── Sanity (snapshot recovery + MEV + mempool + tx forwarding) ──────────

async fn run_sanity_phases(testnet: &Testnet, params: &TestParams) -> SanitySection {
    use crate::tests::RpcClientFactory;

    let factory = RpcClientFactory::new(Duration::from_secs(10));
    let mut phases = Vec::new();
    let mut failures = Vec::new();

    // Resolve arc-node names: explicit param (names and/or node-group names) or
    // the manifest's `ARC_NODES` group when unset.
    let arc_node_names = match crate::tests::arc_node::resolve_arc_nodes(testnet, params) {
        Ok(names) => names,
        Err(e) => {
            let msg = format!("Failed to resolve arc nodes: {e:#}");
            info!("Sanity: {msg}");
            return failed_sanity_section(msg);
        }
    };
    if arc_node_names.is_empty() {
        let msg = format!(
            "Skipped: no arc nodes resolved (set --set arc_nodes=… or define the \
             {} node-group in the manifest)",
            crate::tests::arc_node::DEFAULT_ARC_NODES_GROUP
        );
        info!("Sanity: {msg}");
        return skipped_sanity_section(msg);
    }
    let snapshot_provider = params.get_or("snapshot_provider", DEFAULT_SNAPSHOT_PROVIDER);
    let reference = params.get_or("reference", DEFAULT_REFERENCE);

    let arc_node_urls: Vec<_> = testnet
        .nodes_metadata
        .all_execution_urls()
        .into_iter()
        .filter(|(name, _)| arc_node_names.contains(name))
        .collect();

    let required_nodes: Vec<&str> = arc_node_names
        .iter()
        .map(|s| s.as_str())
        .chain(std::iter::once(reference.as_str()))
        .chain(std::iter::once(snapshot_provider.as_str()))
        .collect();
    let missing: Vec<&&str> = required_nodes
        .iter()
        .filter(|n| testnet.nodes_metadata.execution_http_url(n).is_none())
        .collect();
    if !missing.is_empty() {
        let msg = format!(
            "Skipped: missing nodes {}",
            missing.iter().map(|n| **n).collect::<Vec<_>>().join(", ")
        );
        info!("Sanity: {msg}");
        return skipped_sanity_section(msg);
    }

    // Phase 1: Snapshot recovery (auto-skips in remote mode inside arc_node.rs)
    let t0 = std::time::Instant::now();
    info!("[Sanity Phase 1] Snapshot recovery");
    let phase1 = crate::tests::arc_node::snapshot_recovery(
        testnet,
        &factory,
        &reference,
        &snapshot_provider,
        &arc_node_urls,
    )
    .await;
    let d1 = t0.elapsed();
    let detail = if testnet.is_remote() {
        "Skipped (remote mode)".to_string()
    } else {
        format!(
            "Snapshot from \"{snapshot_provider}\", restored [{}]",
            arc_node_names
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    match &phase1 {
        Ok(()) => {
            phases.push(SanityPhase {
                name: "Snapshot recovery".to_string(),
                passed: true,
                duration: d1,
                detail,
            });
        }
        Err(e) => {
            let msg = format!("Snapshot recovery: {e:#}");
            phases.push(SanityPhase {
                name: "Snapshot recovery".to_string(),
                passed: false,
                duration: d1,
                detail: msg.clone(),
            });
            failures.push(msg);
        }
    }

    // Phase 2: MEV protection
    let t0 = std::time::Instant::now();
    info!("[Sanity Phase 2] MEV protection");
    let phase2 = crate::tests::arc_node::mev_protection(
        testnet,
        &arc_node_names,
        arc_checks::mev::DEFAULT_ADDR,
    )
    .await;
    let d2 = t0.elapsed();
    match &phase2 {
        Ok(()) => {
            phases.push(SanityPhase {
                name: "MEV protection".to_string(),
                passed: true,
                duration: d2,
                detail: format!(
                    "Pending state blocked on [{}]",
                    arc_node_names
                        .iter()
                        .map(|n| format!("\"{n}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        Err(e) => {
            let msg = format!("MEV protection: {e:#}");
            phases.push(SanityPhase {
                name: "MEV protection".to_string(),
                passed: false,
                duration: d2,
                detail: msg.clone(),
            });
            failures.push(msg);
        }
    }

    // Phase 3: Mempool empty
    let t0 = std::time::Instant::now();
    info!("[Sanity Phase 3] Mempool empty");
    let phase3 =
        crate::tests::arc_node::mempool_empty(testnet, &arc_node_names, &arc_node_urls).await;
    let d3 = t0.elapsed();
    match &phase3 {
        Ok(()) => {
            phases.push(SanityPhase {
                name: "Mempool empty".to_string(),
                passed: true,
                duration: d3,
                detail: format!(
                    "Mempools empty on [{}]",
                    arc_node_names
                        .iter()
                        .map(|n| format!("\"{n}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        Err(e) => {
            let msg = format!("Mempool empty: {e:#}");
            phases.push(SanityPhase {
                name: "Mempool empty".to_string(),
                passed: false,
                duration: d3,
                detail: msg.clone(),
            });
            failures.push(msg);
        }
    }

    // Phase 4: Transaction forwarding
    let t0 = std::time::Instant::now();
    info!("[Sanity Phase 4] Transaction forwarding");
    let phase4 =
        crate::tests::arc_node::tx_forwarding(testnet, &factory, &reference, &arc_node_urls).await;
    let d4 = t0.elapsed();
    match &phase4 {
        Ok(()) => {
            phases.push(SanityPhase {
                name: "Tx forwarding".to_string(),
                passed: true,
                duration: d4,
                detail: format!(
                    "Txs forwarded and included on [{}]",
                    arc_node_names
                        .iter()
                        .map(|n| format!("\"{n}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        Err(e) => {
            let msg = format!("Tx forwarding: {e:#}");
            phases.push(SanityPhase {
                name: "Tx forwarding".to_string(),
                passed: false,
                duration: d4,
                detail: msg.clone(),
            });
            failures.push(msg);
        }
    }

    let passed = failures.is_empty();
    SanitySection {
        phases,
        passed,
        failures,
    }
}

// ── Sync speed ──────────────────────────────────────────────────────────

fn default_sync_nodes() -> Vec<String> {
    vec![DEFAULT_SYNC_NODE.to_string()]
}

async fn run_sync_speed_all(testnet: &Testnet, params: &TestParams) -> Vec<SyncSpeedSection> {
    let node_param = params.get_or("sync_nodes", "");
    let nodes: Vec<String> = if node_param.is_empty() {
        default_sync_nodes()
    } else {
        node_param
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let reference = params.get_or("reference", DEFAULT_REFERENCE);
    let min_bps: f64 = params
        .get_or("sync_min_bps", &DEFAULT_SYNC_MIN_BPS.to_string())
        .parse()
        .unwrap_or(DEFAULT_SYNC_MIN_BPS);
    let timeout_s: u64 = params
        .get_or("sync_timeout_s", &DEFAULT_SYNC_TIMEOUT_S.to_string())
        .parse()
        .unwrap_or(DEFAULT_SYNC_TIMEOUT_S);
    let downtime_s: u64 = params
        .get_or("sync_downtime_s", &DEFAULT_SYNC_DOWNTIME_S.to_string())
        .parse()
        .unwrap_or(DEFAULT_SYNC_DOWNTIME_S);

    let mut results = Vec::new();
    for node in &nodes {
        info!("Running sync speed for {node}...");
        results.push(
            run_sync_speed_single(testnet, node, &reference, min_bps, timeout_s, downtime_s).await,
        );
    }
    results
}

async fn run_sync_speed_single(
    testnet: &Testnet,
    node: &str,
    reference: &str,
    min_bps: f64,
    timeout_s: u64,
    downtime_s: u64,
) -> SyncSpeedSection {
    let node_url = match testnet.nodes_metadata.execution_http_url(node) {
        Some(url) => url,
        None => {
            let msg = format!("Skipped: node '{node}' not in manifest");
            info!("Sync speed: {msg}");
            return SyncSpeedSection {
                node: node.to_string(),
                reference: reference.to_string(),
                result: None,
                min_bps,
                passed: false,
                detail: msg,
                duration: Duration::ZERO,
            };
        }
    };
    let ref_url = match testnet.nodes_metadata.execution_http_url(reference) {
        Some(url) => url,
        None => {
            let msg = format!("Skipped: reference node '{reference}' not in manifest");
            info!("Sync speed: {msg}");
            return SyncSpeedSection {
                node: node.to_string(),
                reference: reference.to_string(),
                result: None,
                min_bps,
                passed: false,
                detail: msg,
                duration: Duration::ZERO,
            };
        }
    };

    info!("Sync speed: {node} → {reference} (min {min_bps:.1} blk/s, timeout {timeout_s}s, downtime {downtime_s}s)");

    let t0 = std::time::Instant::now();

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("http client");

    if arc_checks::poll_height(&http, &node_url).await.is_some() {
        info!("{node} is up — stopping for {downtime_s}s to build a gap");
        if let Err(e) = testnet.stop(vec![node.to_string()]).await {
            let msg = format!("Failed to stop {node}: {e:#}");
            return SyncSpeedSection {
                node: node.to_string(),
                reference: reference.to_string(),
                result: None,
                min_bps,
                passed: false,
                detail: msg,
                duration: t0.elapsed(),
            };
        }
        tokio::time::sleep(Duration::from_secs(downtime_s)).await;
        info!("Starting {node}");
        if let Err(e) = testnet.start(vec![node.to_string()], false).await {
            let msg = format!("Failed to start {node}: {e:#}");
            return SyncSpeedSection {
                node: node.to_string(),
                reference: reference.to_string(),
                result: None,
                min_bps,
                passed: false,
                detail: msg,
                duration: t0.elapsed(),
            };
        }
    }

    let config = arc_checks::SyncSpeedConfig {
        node_name: node.to_string(),
        node_url,
        reference_name: reference.to_string(),
        reference_url: ref_url,
        max_duration: if timeout_s == 0 {
            Duration::MAX
        } else {
            Duration::from_secs(timeout_s)
        },
    };

    match arc_checks::collect_sync_speed(config).await {
        Ok(result) => {
            let report = arc_checks::check_sync_speed(&result, min_bps);
            let passed = report.passed();
            let detail = format!("{result}");
            let duration = t0.elapsed();

            SyncSpeedSection {
                node: node.to_string(),
                reference: reference.to_string(),
                result: Some(result),
                min_bps,
                passed,
                detail,
                duration,
            }
        }
        Err(e) => {
            let msg = format!("Sync speed measurement failed: {e:#}");
            SyncSpeedSection {
                node: node.to_string(),
                reference: reference.to_string(),
                result: None,
                min_bps,
                passed: false,
                detail: msg,
                duration: t0.elapsed(),
            }
        }
    }
}

// ── Node version collection ─────────────────────────────────────────────

/// Query each node's EL for arc_getVersion (Arc version) and web3_clientVersion (Reth version).
/// CL version is not queried separately — it is always built from the same commit as the EL.
async fn collect_node_versions(testnet: &Testnet) -> Vec<NodeVersion> {
    use futures::future::join_all;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let tasks: Vec<_> = testnet
        .nodes_metadata
        .nodes
        .iter()
        .map(|(name, meta)| {
            let name = name.clone();
            let el_url = meta.execution.http_url.clone();
            let http = http.clone();
            async move {
                let (arc_version, reth_version) = fetch_node_versions(&http, &el_url).await;
                NodeVersion {
                    name,
                    arc_version,
                    reth_version,
                }
            }
        })
        .collect();

    join_all(tasks).await
}

/// Fetch both arc_getVersion and web3_clientVersion from a single node.
async fn fetch_node_versions(http: &reqwest::Client, url: &Url) -> (String, String) {
    let arc_ver = async {
        let body = serde_json::json!({
            "jsonrpc": "2.0", "method": "arc_getVersion", "params": [], "id": 1
        });
        match http.post(url.as_str()).json(&body).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(json) => json
                    .get("result")
                    .and_then(|v| v.get("git_version"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("(error)")
                    .to_string(),
                Err(_) => "(parse error)".to_string(),
            },
            Err(_) => "(unreachable)".to_string(),
        }
    };

    let reth_ver = async {
        let body = serde_json::json!({
            "jsonrpc": "2.0", "method": "web3_clientVersion", "params": [], "id": 2
        });
        match http.post(url.as_str()).json(&body).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(json) => json
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(error)")
                    .to_string(),
                Err(_) => "(parse error)".to_string(),
            },
            Err(_) => "(unreachable)".to_string(),
        }
    };

    tokio::join!(arc_ver, reth_ver)
}

// ── Info appendices ─────────────────────────────────────────────────────

/// Capture the text output of `quake info mesh`, `perf` (observation + cumulative), `health`
/// (cumulative), and `store`.
async fn collect_info_appendices(
    testnet: &Testnet,
    params: &TestParams,
    raw_before: &[(String, String)],
    raw_after: &[(String, String)],
    observation_secs: u64,
) -> (String, String, String, String, String) {
    // Mesh
    let info_mesh = {
        let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();
        let raw_metrics = crate::mesh::fetch_all_metrics(&metrics_urls).await;
        let nodes_data =
            crate::mesh::parse_and_classify_metrics(&raw_metrics, &testnet.manifest.nodes);
        if nodes_data.is_empty() {
            "No mesh metrics available.\n".to_string()
        } else {
            let analysis = crate::mesh::analyze(&nodes_data);
            let options = crate::mesh::MeshDisplayOptions {
                show_counts: true,
                show_mesh: true,
                show_peers: false,
                show_peers_full: false,
                show_duplicates: true,
            };
            crate::mesh::format_report(&analysis, &options)
        }
    };

    // Perf: observation window (delta — matches main Performance section)
    let info_perf_observation = {
        let nodes = crate::util::parse_perf_metrics_delta_with_groups(
            raw_before,
            raw_after,
            &testnet.manifest.nodes,
        );
        if nodes.is_empty() {
            "No perf metrics available.\n".to_string()
        } else {
            let options = arc_checks::PerfDisplayOptions {
                show_latency: true,
                show_throughput: true,
                show_summary: true,
            };
            arc_checks::format_perf_report(
                &nodes,
                &options,
                arc_checks::PerfReportKind::Interval { observation_secs },
            )
        }
    };

    // Perf: cumulative since process start (final scrape)
    let info_perf_cumulative = {
        let nodes = crate::util::parse_perf_metrics_with_groups(raw_after, &testnet.manifest.nodes);
        if nodes.is_empty() {
            "No perf metrics available.\n".to_string()
        } else {
            let options = arc_checks::PerfDisplayOptions {
                show_latency: true,
                show_throughput: true,
                show_summary: true,
            };
            arc_checks::format_perf_report(
                &nodes,
                &options,
                arc_checks::PerfReportKind::CumulativeSinceStart,
            )
        }
    };

    // Health: cumulative (final scrape). The observation-window delta view is
    // already fully represented in the main Health section table, so it is not
    // duplicated here.
    let info_health_cumulative = {
        let mut nodes_data = arc_checks::parse_all_health_metrics(raw_after);
        crate::util::assign_node_groups(
            nodes_data
                .iter_mut()
                .map(|n| (n.name.as_str(), &mut n.group)),
            &testnet.manifest.nodes,
        );
        arc_checks::format_health_report(&nodes_data)
    };

    // Store — explicit list or auto-derive from nodes with CL pruning configured
    let info_store = {
        let store_param = params.get_or("store_nodes", "");
        let pruned_nodes: Vec<String> = if store_param.is_empty() {
            testnet
                .manifest
                .nodes
                .iter()
                .filter(|(_, node)| node.cl_store_pruning_configured())
                .map(|(name, _)| name.clone())
                .collect()
        } else {
            store_param
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        };

        let mut out = String::new();
        for node_name in &pruned_nodes {
            let store_path = testnet
                .dir
                .join(node_name.as_str())
                .join("malachite")
                .join("store.db");
            if !store_path.exists() {
                continue;
            }
            match arc_checks::collect_store_info(&store_path) {
                Ok(info) => {
                    let size_mb = info.size_bytes as f64 / (1024.0 * 1024.0);
                    let _ = writeln!(out, "### {node_name}\n");
                    let _ = writeln!(out, "Size: {size_mb:.2} MB\n");
                    let _ = writeln!(out, "| Table | Records | Min Key | Max Key |");
                    let _ = writeln!(out, "|-------|--------:|--------:|--------:|");
                    for t in &info.height_tables {
                        let min = t.min_height.map(|h| h.to_string()).unwrap_or("-".into());
                        let max = t.max_height.map(|h| h.to_string()).unwrap_or("-".into());
                        let _ = writeln!(out, "| {} | {} | {} | {} |", t.name, t.records, min, max);
                    }
                    for t in &info.composite_tables {
                        let _ = writeln!(out, "| {} | {} | | |", t.name, t.records);
                    }
                    let _ = writeln!(out);
                }
                Err(e) => {
                    let _ = writeln!(out, "### {node_name}\n\nerror: {e}\n");
                }
            }
        }
        if out.is_empty() {
            "No pruned nodes found.\n".to_string()
        } else {
            out
        }
    };

    (
        info_mesh,
        info_perf_observation,
        info_perf_cumulative,
        info_health_cumulative,
        info_store,
    )
}

// ── Load helpers (shared with sanity.rs) ────────────────────────────────

// ── Public entry point ──────────────────────────────────────────────────

pub(crate) async fn run_report(
    testnet: &Testnet,
    params: &TestParams,
    output: &std::path::Path,
) -> Result<()> {
    info!("Generating network testing report...");

    let data = collect_report_data(testnet, params, output).await?;
    let markdown = render_markdown(&data);

    std::fs::write(output, &markdown)?;
    info!("Report written to {}", output.display());

    // Print one-line summary to stdout
    let sanity_passed = data.sanity.as_ref().is_none_or(|s| s.passed);
    let sync_passed = data.sync_speed.iter().all(|s| s.passed);
    let overall =
        data.mesh.passed && data.health.passed && data.perf.passed && sanity_passed && sync_passed;
    let sanity_checks = data.sanity.as_ref().map_or(0, |s| s.phases.len());
    let sanity_failures = data.sanity.as_ref().map_or(0, |s| s.failures.len());
    let sync_checks = data.sync_speed.len();
    let sync_fail_count = data.sync_speed.iter().filter(|s| !s.passed).count();
    let total_checks = data.mesh.entries.len()
        + data.health.nodes.len()
        + data.perf.nodes.len()
        + sanity_checks
        + sync_checks;
    let total_failures = data.mesh.failures.len()
        + data.health.failures.len()
        + data.perf.failures.len()
        + sanity_failures
        + sync_fail_count;

    if overall {
        println!(
            "\n✅ All checks passed ({total_checks} checks). Report: {}",
            output.display()
        );
    } else {
        println!(
            "\n❌ {total_failures} failure(s) out of {total_checks} checks. Report: {}",
            output.display()
        );
    }

    Ok(())
}
