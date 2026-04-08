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

//! Sync speed test: measures how fast a non-validator catches up.
//!
//! ```text
//! # Using defaults:
//! ./quake test sync:speed
//!
//! # With custom parameters:
//! ./quake test sync:speed \
//!   --set node=full-p2p \        # node to measure (default: full-p2p)
//!   --set reference=validator1 \  # reference node (default: validator1)
//!   --set min_bps=7 \            # minimum avg blocks/s to pass (default: 7)
//!   --set timeout_s=180 \        # max measurement duration; 0 = wait until caught up (default: 180)
//!   --set downtime_s=120         # seconds to keep node down before measuring, if not already down (default: 120)
//! ```

use std::time::Duration;

use color_eyre::eyre::ensure;
use tracing::info;

use super::{quake_test, RpcClientFactory, TestParams, TestResult};
use crate::testnet::Testnet;

const DEFAULT_MIN_BPS: f64 = 7.0;
const DEFAULT_TIMEOUT_SECS: u64 = 180;
const DEFAULT_DOWNTIME_SECS: u64 = 120;

#[quake_test(group = "sync", name = "speed")]
fn speed_test<'a>(
    testnet: &'a Testnet,
    _factory: &'a RpcClientFactory,
    params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        let node = params.get_or("node", "full-p2p");
        let reference = params.get_or("reference", "validator1");
        let min_bps: f64 = params
            .get_or("min_bps", &DEFAULT_MIN_BPS.to_string())
            .parse()
            .unwrap_or(DEFAULT_MIN_BPS);
        let timeout_s: u64 = params
            .get_or("timeout_s", &DEFAULT_TIMEOUT_SECS.to_string())
            .parse()
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let downtime_s: u64 = params
            .get_or("downtime_s", &DEFAULT_DOWNTIME_SECS.to_string())
            .parse()
            .unwrap_or(DEFAULT_DOWNTIME_SECS);

        let node_url = match testnet.nodes_metadata.execution_http_url(&node) {
            Some(url) => url,
            None => {
                info!("Skipping: node '{node}' not in manifest");
                return Ok(());
            }
        };
        let ref_url = match testnet.nodes_metadata.execution_http_url(&reference) {
            Some(url) => url,
            None => {
                info!("Skipping: reference node '{reference}' not in manifest");
                return Ok(());
            }
        };

        info!("Measuring sync speed: {node} -> {reference} (min {min_bps:.1} blk/s, timeout {timeout_s}s)");

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;

        // If the node is already running, stop it, let validators advance for `downtime_s`,
        // then restart so the node has a gap to catchup and measure.
        if arc_checks::poll_height(&http, &node_url).await.is_some() {
            info!("{node} is already up — stopping it for {downtime_s}s to build a gap");
            testnet.stop(vec![node.clone()]).await?;
            info!("Waiting {downtime_s}s for the network to advance...");
            tokio::time::sleep(Duration::from_secs(downtime_s)).await;
            info!("Starting {node}");
            testnet.start(vec![node.clone()]).await?;
        }

        let config = arc_checks::SyncSpeedConfig {
            node_name: node.clone(),
            node_url,
            reference_name: reference.clone(),
            reference_url: ref_url,
            max_duration: if timeout_s == 0 {
                Duration::MAX
            } else {
                Duration::from_secs(timeout_s)
            },
        };

        let result = arc_checks::collect_sync_speed(config).await?;

        info!("{result}");

        let report = arc_checks::check_sync_speed(&result, min_bps);
        for check in &report.checks {
            info!(
                "  {} {}",
                if check.passed { "pass" } else { "FAIL" },
                check.message
            );
        }
        ensure!(
            report.passed(),
            "Sync speed check failed for {node} (avg {:.1} blk/s < {min_bps:.1} required)",
            result.avg_bps,
        );

        info!("[DONE] sync:speed passed");
        Ok(())
    })
}
