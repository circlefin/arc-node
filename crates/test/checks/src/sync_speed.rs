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

//! Sync speed measurement: polls two EL RPC endpoints (node + reference)
//! and tracks blocks/s as the node catches up.

use std::fmt;
use std::time::Duration;

use color_eyre::eyre::{self, Result};
use serde_json::json;
use tokio::time::Instant;
use url::Url;

use crate::types::{CheckResult, Report};

/// Final result of a sync speed measurement.
#[derive(Debug, Clone)]
pub struct SyncSpeedResult {
    pub node_name: String,
    pub reference_name: String,
    pub start_height: u64,
    pub target_height: u64,
    pub final_height: u64,
    pub total_blocks: u64,
    pub elapsed: Duration,
    pub avg_bps: f64,
    /// Whether the node reached the target before the timeout.
    pub caught_up: bool,
}

impl fmt::Display for SyncSpeedResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.total_blocks == 0 {
            return write!(
                f,
                "{} is already at or ahead of {}, nothing to measure",
                self.node_name, self.reference_name
            );
        }

        let gap = self.target_height.saturating_sub(self.start_height);
        writeln!(
            f,
            "{} at block {}, {} at {} (gap: {gap} blocks)",
            self.node_name, self.start_height, self.reference_name, self.target_height,
        )?;

        let status = if self.caught_up {
            "caught up"
        } else {
            "measured syncing"
        };

        let elapsed = format_secs(self.elapsed.as_secs());

        write!(
            f,
            "{} {status} to {} in {elapsed} ({} blocks, avg {:.1} blk/s)",
            self.node_name, self.target_height, self.total_blocks, self.avg_bps,
        )
    }
}

/// Configuration for a sync speed measurement.
pub struct SyncSpeedConfig {
    pub node_name: String,
    pub node_url: Url,
    pub reference_name: String,
    pub reference_url: Url,
    /// Maximum wall-clock time to measure sync speed.
    pub max_duration: Duration,
}

fn format_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Poll `eth_blockNumber` on a URL. Returns `None` on any error.
pub async fn poll_height(client: &reqwest::Client, url: &Url) -> Option<u64> {
    let body = json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });
    let resp = client.post(url.as_str()).json(&body).send().await.ok()?;
    let val: serde_json::Value = resp.json().await.ok()?;
    let hex = val.get("result")?.as_str()?;
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    u64::from_str_radix(hex, 16).ok()
}

/// Poll `eth_blockNumber` with retries.
async fn poll_height_with_retries(
    client: &reqwest::Client,
    url: &Url,
    retries: u32,
) -> Option<u64> {
    for _ in 0..=retries {
        if let Some(h) = poll_height(client, url).await {
            return Some(h);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    None
}

/// Measure sync speed of a node catching up to a reference node.
///
/// 1. Waits for `node_url` to respond with height > 0 (EL up + CL committed).
/// 2. Records the reference node's current height as the target.
/// 3. Polls every second until the node reaches the target or `max_duration` has passed.
pub async fn collect_sync_speed(config: SyncSpeedConfig) -> Result<SyncSpeedResult> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    let deadline = Instant::now().checked_add(config.max_duration);

    println!("Waiting for {} to respond...", config.node_name);

    // Wait for node to respond with height > 0
    let start_height = loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(eyre::eyre!(
                "Max duration reached waiting for {} to respond",
                config.node_name
            ));
        }
        match poll_height(&http, &config.node_url).await {
            Some(h) if h > 0 => break h,
            Some(_) => {
                println!("  EL up, waiting for CL to commit first block...");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            None => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    };

    let target_height = poll_height_with_retries(&http, &config.reference_url, 3)
        .await
        .ok_or_else(|| eyre::eyre!("Failed to get {} height", config.reference_name))?;

    if start_height >= target_height {
        return Ok(SyncSpeedResult {
            node_name: config.node_name,
            reference_name: config.reference_name,
            start_height,
            target_height,
            final_height: start_height,
            total_blocks: 0,
            elapsed: Duration::ZERO,
            avg_bps: 0.0,
            caught_up: true,
        });
    }

    println!(
        "{:>8} {:>10} {:>10} {:>10} {:>12} {:>8}",
        "elapsed", "height", "gap", "blk/s", "avg blk/s", "ETA"
    );

    let wall_start = Instant::now();
    let mut prev_height = start_height;
    let mut prev_time = wall_start;

    let (final_height, caught_up) = loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        if deadline.is_some_and(|d| Instant::now() >= d) {
            break (prev_height, false);
        }

        let height = match poll_height(&http, &config.node_url).await {
            Some(h) => h,
            None => continue,
        };

        let now = Instant::now();
        let interval = now.duration_since(prev_time).as_secs_f64();
        let instant_bps = if interval > 0.0 {
            (height.saturating_sub(prev_height)) as f64 / interval
        } else {
            0.0
        };

        let elapsed = now.duration_since(wall_start).as_secs_f64();
        let total_synced = height.saturating_sub(start_height);
        let avg_bps = if elapsed > 0.0 {
            total_synced as f64 / elapsed
        } else {
            0.0
        };

        let remaining = target_height.saturating_sub(height);

        let eta = if avg_bps > 0.0 {
            format_secs((remaining as f64 / avg_bps) as u64)
        } else {
            "---".into()
        };
        println!(
            "{:>7.1}s {:>10} {:>10} {:>10.1} {:>12.1} {:>8}",
            elapsed, height, remaining, instant_bps, avg_bps, eta
        );

        prev_height = height;
        prev_time = now;

        if height >= target_height {
            break (height, true);
        }
    };

    let elapsed = wall_start.elapsed();
    let total_blocks = final_height.saturating_sub(start_height);
    let avg_bps = if elapsed.as_secs_f64() > 0.0 {
        total_blocks as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    Ok(SyncSpeedResult {
        node_name: config.node_name,
        reference_name: config.reference_name,
        start_height,
        target_height,
        final_height,
        total_blocks,
        elapsed,
        avg_bps,
        caught_up,
    })
}

/// Check that the measured sync speed meets a minimum threshold.
///
/// Uses `avg_bps` from the collected samples regardless of whether the node
/// fully caught up. The test passes as long as the sync rate is fast enough.
pub fn check_sync_speed(result: &SyncSpeedResult, min_bps: f64) -> Report {
    let passed = (result.caught_up && result.total_blocks == 0) || result.avg_bps >= min_bps;
    let status = if result.caught_up {
        "caught up".to_string()
    } else {
        format!(
            "reached {} / {} within measurement window",
            result.final_height, result.target_height
        )
    };

    let checks = vec![CheckResult {
        name: result.node_name.clone(),
        passed,
        message: format!(
            "synced {} blocks in {:.1}s (avg {:.1} blk/s, min required: {:.1}, {status})",
            result.total_blocks,
            result.elapsed.as_secs_f64(),
            result.avg_bps,
            min_bps,
        ),
    }];

    Report { checks }
}
