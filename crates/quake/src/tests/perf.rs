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

use tracing::{debug, info};

use super::{quake_test, RpcClientFactory, TestOutcome, TestParams, TestResult};
use crate::testnet::Testnet;

const DEFAULT_P50_THRESHOLD_MS: &str = "550";
const DEFAULT_P95_THRESHOLD_MS: &str = "1000";
const DEFAULT_WARMUP_S: &str = "30";
const DEFAULT_OBSERVATION_S: &str = "60";

/// Assert that every node's block time p50 and p95 are within thresholds.
///
/// Supports two modes:
/// - `mode=interval` (default): two-scrape approach that isolates an
///   observation window, excluding startup noise from the percentiles.
/// - `mode=full`: single scrape after an observation period, measuring
///   the full histogram since the node process started.
///
/// # Parameters (via `--set key=value`)
///
/// | Key                 | Default       | Description                                        |
/// |---------------------|---------------|----------------------------------------------------|
/// | `mode`              | `interval`    | `interval` (two-scrape window) or `full`             |
/// | `warmup_s`          | `30`          | Seconds before first scrape (`interval` mode only)   |
/// | `observation_s`     | `60`          | Observation window (between scrapes / before scrape) |
/// | `block_time_p50_ms` | `550`         | Fail if any node's p50 exceeds this                 |
/// | `block_time_p95_ms` | `1000`        | Fail if any node's p95 exceeds this                 |
#[quake_test(group = "perf", name = "block_time")]
fn block_time_test<'a>(
    testnet: &'a Testnet,
    _factory: &'a RpcClientFactory,
    params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        let mode = params.get_or("mode", "interval");
        let warmup_s: u64 = params
            .get_or("warmup_s", DEFAULT_WARMUP_S)
            .parse()
            .unwrap_or(30);
        let observation_s: u64 = params
            .get_or("observation_s", DEFAULT_OBSERVATION_S)
            .parse()
            .unwrap_or(60);
        let p50_ms: u64 = params
            .get_or("block_time_p50_ms", DEFAULT_P50_THRESHOLD_MS)
            .parse()
            .unwrap_or(550);
        let p95_ms: u64 = params
            .get_or("block_time_p95_ms", DEFAULT_P95_THRESHOLD_MS)
            .parse()
            .unwrap_or(1000);

        if mode != "interval" && mode != "full" {
            color_eyre::eyre::bail!("invalid mode '{mode}': expected 'interval' or 'full'");
        }

        let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();

        let report = if mode == "full" {
            info!("Mode: full — waiting {observation_s}s for blocks to accumulate...");
            tokio::time::sleep(tokio::time::Duration::from_secs(observation_s)).await;

            arc_checks::check_block_time(&metrics_urls, p50_ms, p95_ms).await?
        } else {
            if warmup_s > 0 {
                info!("Warming up for {warmup_s}s...");
                tokio::time::sleep(tokio::time::Duration::from_secs(warmup_s)).await;
            }

            debug!("Taking first scrape...");
            let raw_before = arc_checks::fetch_all_metrics(&metrics_urls).await;

            info!("Mode: interval — observing for {observation_s}s...");
            tokio::time::sleep(tokio::time::Duration::from_secs(observation_s)).await;

            debug!("Taking second scrape...");
            let raw_after = arc_checks::fetch_all_metrics(&metrics_urls).await;

            arc_checks::check_block_time_delta(&raw_before, &raw_after, p50_ms, p95_ms)
        };

        let mut outcome = TestOutcome::new();
        for check in report.checks {
            outcome.add_check(check.into());
        }

        outcome
            .auto_summary(
                &format!("All nodes within block time thresholds (p50<{p50_ms}ms, p95<{p95_ms}ms)"),
                &format!(
                    "{{}} node(s) exceeded block time thresholds (p50<{p50_ms}ms, p95<{p95_ms}ms)"
                ),
            )
            .into_result()
    })
}
