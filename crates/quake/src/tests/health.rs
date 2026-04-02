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

//! Consensus health stability test.
//!
//! # Overview
//!
//! Supports two modes:
//! - `mode=interval` (default): two-scrape delta approach — only events that
//!   occurred during the observation window are asserted, excluding startup noise.
//! - `mode=full`: single scrape after an observation period, checking absolute
//!   counters since the node process started.
//!
//! # Parameters (via `--set key=value`)
//!
//! | Key          | Default    | Description                                          |
//! |--------------|------------|------------------------------------------------------|
//! | `mode`       | `interval` | `interval` (two-scrape window) or `full`             |
//! | `warmup_s`   | `30`       | Seconds before first scrape (`interval` mode only)   |
//! | `duration_s` | `60`       | Observation window (between scrapes / before scrape)  |
//!
//! # Assertions
//!
//! For every node:
//! - No consensus decisions in round > 0 (`delta R>0 == 0`)
//! - No height restarts (`delta height_restart_count == 0`)
//! - No sync-fell-behind events (`delta sync_fell_behind_count == 0`)
//!
//! # Usage
//!
//! ```text
//! quake test health                                  # interval (default)
//! quake test health --set mode=full                  # full history
//! quake test health --set warmup_s=30 duration_s=120 # custom timings
//! ```

use tracing::{debug, info};

use super::{quake_test, RpcClientFactory, TestOutcome, TestParams, TestResult};
use crate::testnet::Testnet;

const DEFAULT_WARMUP_S: u64 = 30;
const DEFAULT_DURATION_S: u64 = 60;

/// Consensus health stability test with configurable mode.
#[quake_test(group = "health", name = "stability")]
fn stability_test<'a>(
    testnet: &'a Testnet,
    _factory: &'a RpcClientFactory,
    params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        let mode = params.get_or("mode", "interval");
        let warmup_s: u64 = params
            .get_or("warmup_s", &DEFAULT_WARMUP_S.to_string())
            .parse()
            .unwrap_or(DEFAULT_WARMUP_S);
        let duration_s: u64 = params
            .get_or("duration_s", &DEFAULT_DURATION_S.to_string())
            .parse()
            .unwrap_or(DEFAULT_DURATION_S);

        if mode != "interval" && mode != "full" {
            color_eyre::eyre::bail!("invalid mode '{mode}': expected 'interval' or 'full'");
        }

        let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();

        let (before, after) = if mode == "full" {
            info!("Mode: full — waiting {duration_s}s before scraping counters...");
            tokio::time::sleep(tokio::time::Duration::from_secs(duration_s)).await;

            debug!("Taking scrape...");
            let raw = arc_checks::fetch_all_metrics(&metrics_urls).await;
            let mut nodes = arc_checks::parse_all_health_metrics(&raw);

            if nodes.is_empty() {
                color_eyre::eyre::bail!("No health metrics collected from any node");
            }

            crate::util::assign_node_groups(
                nodes.iter_mut().map(|n| (n.name.as_str(), &mut n.group)),
                &testnet.manifest.nodes,
            );

            let zeroed: Vec<arc_checks::NodeHealthData> = nodes
                .iter()
                .map(|n| arc_checks::NodeHealthData {
                    name: n.name.clone(),
                    group: n.group.clone(),
                    round_0: 0,
                    round_1: 0,
                    round_gt1: 0,
                    total_decisions: 0,
                    height_restarts: 0,
                    sync_fell_behind: 0,
                })
                .collect();

            (zeroed, nodes)
        } else {
            if warmup_s > 0 {
                info!("Warming up for {warmup_s}s before first scrape...");
                tokio::time::sleep(tokio::time::Duration::from_secs(warmup_s)).await;
            }

            debug!("Taking first health metrics scrape...");
            let raw_before = arc_checks::fetch_all_metrics(&metrics_urls).await;
            let mut before = arc_checks::parse_all_health_metrics(&raw_before);

            if before.is_empty() {
                color_eyre::eyre::bail!("No health metrics collected from any node (first scrape)");
            }

            crate::util::assign_node_groups(
                before.iter_mut().map(|n| (n.name.as_str(), &mut n.group)),
                &testnet.manifest.nodes,
            );

            info!(
                "Mode: interval — {} nodes, observing for {duration_s}s...",
                before.len()
            );
            tokio::time::sleep(tokio::time::Duration::from_secs(duration_s)).await;

            debug!("Taking second health metrics scrape...");
            let raw_after = arc_checks::fetch_all_metrics(&metrics_urls).await;
            let mut after = arc_checks::parse_all_health_metrics(&raw_after);

            if after.is_empty() {
                color_eyre::eyre::bail!(
                    "No health metrics collected from any node (second scrape)"
                );
            }

            crate::util::assign_node_groups(
                after.iter_mut().map(|n| (n.name.as_str(), &mut n.group)),
                &testnet.manifest.nodes,
            );

            (before, after)
        };

        let mode_label = if mode == "full" {
            format!("full ({duration_s}s)")
        } else {
            format!("interval ({warmup_s}s warmup, {duration_s}s observation)")
        };

        let deltas = arc_checks::compute_health_deltas(&before, &after);
        let report = arc_checks::check_health_deltas(&deltas);

        println!("Health stability ({mode_label}):");
        println!();
        print!("{}", arc_checks::format_health_delta_report(&deltas));
        println!();

        let mut outcome = TestOutcome::new();
        for check in report.checks {
            outcome.add_check(check.into());
        }

        outcome
            .auto_summary(
                "All nodes healthy during observation window",
                "{} check(s) failed",
            )
            .into_result()
    })
}
