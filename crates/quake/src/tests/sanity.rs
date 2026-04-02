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

//! Network validation test: mesh + performance + health under configurable load.
//!
//! # Overview
//!
//! Runs mesh connectivity checks, block-time performance assertions, and
//! consensus health checks in a single pass. Both performance and health use a
//! two-scrape delta approach, measuring only the observation window.
//! Transaction load is configurable (default: 50 TPS of native transfers for 60 seconds).
//!
//! Works on both local (Docker Compose) and remote (AWS EC2) testnets.
//!
//! **Group: `validation`** — This test is in the `validation` group (not `sanity`)
//! so that it sorts after `tx` in alphabetical execution order. It introduces
//! transaction load which can leave pending txs in the mempool, affecting
//! subsequent tests if run earlier. TODO: revisit test suite ordering strategy.
//!
//! # Parameters (via `--set key=value`)
//!
//! | Key                 | Default          | Description                                       |
//! |---------------------|------------------|---------------------------------------------------|
//! | `warmup_s`          | `30`             | Seconds to wait for network stabilization         |
//! | `duration_s`        | `60`             | Experiment window (load duration or sleep)        |
//! | `load_rate`         | `50`             | TPS sent during experiment (0 = no load)          |
//! | `load_targets`      | `""` (all nodes) | Comma-separated node names to send load to        |
//! | `load_mix`          | `transfer=100`   | Tx type mix (`--mix` format; `erc20`/`guzzler` need contracts in genesis) |
//! | `strict_mesh`       | `true`           | Enforce mesh tier expectations                    |
//! | `block_time_p50_ms` | `550`            | Fail if any node's p50 block time exceeds this    |
//! | `block_time_p95_ms` | `1000`           | Fail if any node's p95 block time exceeds this    |
//!
//! # Usage
//!
//! ```text
//! quake test validation:basic                                    # default (50 TPS, 60s)
//! quake test validation:basic --set load_rate=0                  # no load (baseline)
//! quake test validation:basic --set load_mix=transfer=70,erc20=30
//! quake test validation:basic --set load_rate=500 --set duration_s=120 --set load_targets=rpc1,rpc2
//! ```

use std::time::Duration;

use color_eyre::eyre::bail;
use tracing::{debug, warn};

use super::mesh::run_mesh_checks;
use super::{quake_test, CheckResult, RpcClientFactory, TestParams, TestResult};
use crate::testnet::Testnet;
use crate::RemoteSubcommand;

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

const DEFAULT_WARMUP_S: u64 = 30;
const DEFAULT_DURATION_S: u64 = 60;
const DEFAULT_LOAD_RATE: u64 = 50;
const DEFAULT_LOAD_MIX: &str = "transfer=100";
const DEFAULT_P50_MS: u64 = 550;
const DEFAULT_P95_MS: u64 = 1000;
const MIN_DURATION_WARNING_S: u64 = 30;

// ── Load helpers ────────────────────────────────────────────────────────

/// Build a spammer::Config for local load generation.
///
/// Uses SpammerArgs with CLI-matching defaults, overriding rate, time, and mix.
fn build_spammer_config(
    rate: u64,
    duration_s: u64,
    mix: &str,
) -> color_eyre::eyre::Result<spammer::Config> {
    let args = spammer::SpammerArgs {
        num_generators: 1,
        max_num_accounts: 100,
        partition_mode: spammer::PartitionMode::Linear,
        num_txs: 0,
        rate,
        time: duration_s,
        tx_input_size: 0,
        max_txs_per_account: 0,
        preinit_accounts: false,
        query_latest_nonce: false,
        show_pool_status: false,
        tx_latency: false,
        csv_dir: None,
        wait_response: false,
        reconnect_attempts: 3,
        reconnect_period: Duration::from_secs(3),
        tx_type_mix: Some(
            mix.parse()
                .map_err(|e: String| color_eyre::eyre::eyre!("invalid load_mix: {e}"))?,
        ),
        guzzler_fn_weights: "hash-loop=0,storage-write=0,storage-read=0,guzzle=0,guzzle2=0"
            .parse()
            .expect("default guzzler weights"),
        erc20_fn_weights: None,
    };
    let config = args.to_config(false, false);
    config.validate()?;
    Ok(config)
}

/// Build CLI args for remote load generation (passed to `quake remote load`).
fn build_remote_load_args(
    rate: u64,
    duration_s: u64,
    mix: &str,
    targets: &[String],
) -> Vec<String> {
    let mut args: Vec<String> = targets.to_vec();
    args.extend(["-r".into(), rate.to_string()]);
    args.extend(["-t".into(), duration_s.to_string()]);
    args.extend(["--mix".into(), mix.into()]);
    args
}

// ── Display helpers ─────────────────────────────────────────────────────

/// Format a single node's health delta as a one-liner.
fn format_health_line(d: &arc_checks::NodeHealthDelta) -> String {
    let rounds_gt0 = d.delta_round_1 + d.delta_round_gt1;
    let has_issues = rounds_gt0 > 0 || d.delta_height_restarts > 0 || d.delta_sync_fell_behind > 0;
    let marker = if has_issues { "✗" } else { "✓" };

    let round_info = if rounds_gt0 > 0 {
        format!("round>0: {rounds_gt0}")
    } else {
        "all round 0".to_string()
    };
    let restart_info = if d.delta_height_restarts > 0 {
        format!("restarts: {}", d.delta_height_restarts)
    } else {
        "no restarts".to_string()
    };
    let sync_info = if d.delta_sync_fell_behind > 0 {
        format!("sync behind: {}", d.delta_sync_fell_behind)
    } else {
        "no sync behind".to_string()
    };

    format!(
        "  {marker} {}: {} decisions, {round_info}, {restart_info}, {sync_info}",
        d.name, d.delta_decisions
    )
}

// ── Summary helpers ─────────────────────────────────────────────────────

fn print_section_summary(label: &str, checks: &[CheckResult]) {
    if checks.is_empty() {
        println!("  ⊘ {label}: skipped");
        return;
    }
    let total = checks.len();
    let failures: Vec<&CheckResult> = checks.iter().filter(|c| !c.success).collect();
    let passed = total - failures.len();

    if failures.is_empty() {
        println!("  {GREEN}✓ {label}: {passed}/{total} passed{RESET}");
    } else {
        println!(
            "  {RED}✗ {label}: {passed}/{total} passed ({} failed){RESET}",
            failures.len()
        );
        for f in &failures {
            println!("    {RED}✗ {}: {}{RESET}", f.name, f.message);
        }
    }
}

// ── Test ────────────────────────────────────────────────────────────────

/// Basic network validation test: mesh + perf + health under configurable load.
///
/// Runs in a single pass: warmup → scrape 1 → load/sleep(duration_s) →
/// scrape 2 → health delta + perf delta → mesh → grouped summary.
///
/// Works on both local and remote testnets.
#[quake_test(group = "validation", name = "basic")]
fn basic_test<'a>(
    testnet: &'a Testnet,
    _factory: &'a RpcClientFactory,
    params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        // ── Parse parameters ───────────────────────────────────────
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
        let strict_mesh = params.get_or("strict_mesh", "true") == "true";
        let p50_ms: u64 = params
            .get_or("block_time_p50_ms", &DEFAULT_P50_MS.to_string())
            .parse()
            .unwrap_or(DEFAULT_P50_MS);
        let p95_ms: u64 = params
            .get_or("block_time_p95_ms", &DEFAULT_P95_MS.to_string())
            .parse()
            .unwrap_or(DEFAULT_P95_MS);

        let load_targets: Vec<String> = load_targets_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if duration_s < MIN_DURATION_WARNING_S {
            warn!(
                "duration_s={duration_s} is short; the observation window may not \
                 contain enough blocks for meaningful block time percentiles or \
                 enough consensus decisions for reliable health deltas"
            );
        }

        let load_desc = if load_rate > 0 {
            format!("{load_rate} TPS ({load_mix})")
        } else {
            "none".to_string()
        };

        println!("── validation:basic ──────────────────────────────────");
        println!("  warmup:     {warmup_s}s");
        println!("  duration:   {duration_s}s");
        println!("  load:       {load_desc}");
        if !load_targets.is_empty() {
            println!("  targets:    {}", load_targets.join(", "));
        }
        println!("  mesh:       strict={strict_mesh}");
        println!("  perf:       p50 < {p50_ms}ms, p95 < {p95_ms}ms");
        println!("─────────────────────────────────────────────────────\n");

        let mut health_checks: Vec<CheckResult> = Vec::new();
        let mut perf_checks: Vec<CheckResult> = Vec::new();

        // ── Warmup ─────────────────────────────────────────────────
        if warmup_s > 0 {
            println!("Warming up ({warmup_s}s)...");
            tokio::time::sleep(Duration::from_secs(warmup_s)).await;
        }

        // ── Health baseline (silent scrape) ────────────────────────
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
        debug!("Health baseline: {} nodes", health_before.len());

        // ── Load ───────────────────────────────────────────────────
        if load_rate > 0 {
            println!("\n── Introducing load ─────────────────────────────────");
            println!("  {load_desc} for {duration_s}s\n");
            if testnet.is_remote() {
                let args = build_remote_load_args(load_rate, duration_s, &load_mix, &load_targets);
                testnet.remote(RemoteSubcommand::Load { args }).await?;
            } else {
                let config = build_spammer_config(load_rate, duration_s, &load_mix)?;
                testnet.load(load_targets.clone(), &config).await?;
            }
        } else {
            println!("\n── Observation ({duration_s}s, no load) ──────────────");
            tokio::time::sleep(Duration::from_secs(duration_s)).await;
        }

        // ── Health ─────────────────────────────────────────────────
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

        let deltas = arc_checks::compute_health_deltas(&health_before, &health_after);
        let health_report = arc_checks::check_health_deltas(&deltas);

        println!("\n── Health check ─────────────────────────────────────");
        for d in &deltas {
            println!("{}", format_health_line(d));
        }

        for check in health_report.checks {
            health_checks.push(check.into());
        }

        // ── Performance (delta between scrapes) ────────────────────
        let perf_report =
            arc_checks::check_block_time_delta(&raw_before, &raw_after, p50_ms, p95_ms);
        println!("\n── Performance check ────────────────────────────────");
        for check in &perf_report.checks {
            let marker = if check.passed { "✓" } else { "✗" };
            println!("  {marker} {}: {}", check.name, check.message);
        }

        for check in perf_report.checks {
            perf_checks.push(check.into());
        }

        // ── Mesh ───────────────────────────────────────────────────
        let mesh_checks = run_mesh_checks(testnet, strict_mesh, "mesh", false).await?;

        // ── Summary ────────────────────────────────────────────────
        println!("\n── Summary ──────────────────────────────────────────");
        let all_checks: Vec<&CheckResult> = mesh_checks
            .iter()
            .chain(health_checks.iter())
            .chain(perf_checks.iter())
            .collect();
        let total = all_checks.len();
        let failed: Vec<&&CheckResult> = all_checks.iter().filter(|c| !c.success).collect();
        let passed = total - failed.len();

        print_section_summary("Mesh", &mesh_checks);
        print_section_summary("Health", &health_checks);
        print_section_summary("Perf", &perf_checks);

        println!();
        if failed.is_empty() {
            println!("  {BOLD}{GREEN}All {total} checks passed{RESET}");
        } else {
            println!(
                "  {BOLD}Total: {passed}/{total} passed, {RED}{} failed{RESET}",
                failed.len()
            );
        }
        println!("─────────────────────────────────────────────────────");

        if failed.is_empty() {
            Ok(())
        } else {
            bail!("{} validation check(s) failed", failed.len())
        }
    })
}
