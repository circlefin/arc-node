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

use std::collections::BTreeMap;
use std::time::Duration;

use alloy_provider::{Provider, ProviderBuilder};
use alloy_transport_ws::WsConnect;
use color_eyre::eyre::{bail, Result};
use serde::Deserialize;
use tokio::time::Instant;
use tracing::{debug, info, warn};
use url::Url;

use crate::{node::NodeName, rpc::RpcClient};

/// Wait for all given nodes to reach a certain height.
pub(crate) async fn wait_for_nodes(
    node_urls: Vec<(NodeName, Url)>,
    height: u64,
    timeout: Duration,
) -> Result<()> {
    let mut handles = Vec::new();

    let num_nodes = node_urls.len();
    debug!("⏳ Waiting for {num_nodes} nodes to reach height {height}...");
    for (node_name, url) in node_urls.into_iter() {
        let node_name = node_name.clone();
        handles.push(tokio::spawn(async move {
            wait_for_node(node_name, url, height, timeout).await
        }));
    }

    for handle in handles {
        handle.await??;
    }
    debug!("✅ All {num_nodes} nodes reached height {height}");
    Ok(())
}

/// Wait until a node reaches the given height by polling its block number.
pub(crate) async fn wait_for_node(
    node: NodeName,
    url: Url,
    expected_height: u64,
    timeout: Duration,
) -> Result<()> {
    let url_str = url.to_string();
    // The timeout of several seconds for each RPC request is mainly required in
    // remote mode, where the connection might be slow.
    let client = RpcClient::new(url, Duration::from_secs(10));

    // We make the RPC request with retries to handle the case where the node is not up yet.
    let mut height = client.get_latest_block_number_with_retries(10).await?;

    if height < expected_height {
        debug!("⏳ Waiting for {node} ({url_str}) at height {height} to reach {expected_height}");
        let mut last_update_time = Instant::now();
        let mut last_height = height;

        while height < expected_height {
            tokio::time::sleep(Duration::from_secs(1)).await;

            height = client.get_latest_block_number_with_retries(10).await?;

            if height > last_height {
                // Node has progressed, reset the timeout
                last_height = height;
                last_update_time = Instant::now();

                continue;
            }

            if last_update_time.elapsed() > timeout {
                bail!("⌛️ Timeout waiting for {node} ({url_str}) to reach {expected_height}");
            }
        }
    }

    debug!("🎯 {node} ({url_str}) is at height {height}");
    Ok(())
}

/// Wait for all given nodes to finish syncing (eth_syncing returns false).
pub(crate) async fn wait_for_nodes_sync(
    node_urls: Vec<(NodeName, Url)>,
    timeout: Duration,
    max_retries: u32,
) -> Result<()> {
    let mut handles = Vec::new();

    let num_nodes = node_urls.len();
    debug!("⏳ Waiting for {num_nodes} nodes to finish syncing...");
    for (node_name, url) in node_urls.into_iter() {
        let node_name = node_name.clone();
        handles.push(tokio::spawn(async move {
            wait_for_node_sync(node_name, url, timeout, max_retries).await
        }));
    }

    for handle in handles {
        handle.await??;
    }
    debug!("✅ All {num_nodes} nodes are synced");
    Ok(())
}

/// Wait until a node finishes syncing by polling eth_syncing.
/// Retries failed RPC calls up to max_retries times to handle node restarts.
pub(crate) async fn wait_for_node_sync(
    node: NodeName,
    url: Url,
    timeout: Duration,
    max_retries: u32,
) -> Result<()> {
    let url_str = url.to_string();
    let client = RpcClient::new(url, Duration::from_secs(10));

    debug!("⏳ Waiting for {node} ({url_str}) to finish syncing (max retries: {max_retries})");
    let start_time = Instant::now();
    let mut consecutive_failures = 0u32;

    loop {
        // Check if we've exceeded the timeout
        if start_time.elapsed() > timeout {
            bail!("⌛️ Timeout waiting for {node} ({url_str}) to finish syncing");
        }

        // Try to check sync status
        match client.is_syncing().await {
            Ok(is_syncing) => {
                // Reset failure count on success
                consecutive_failures = 0;

                if !is_syncing {
                    debug!("🎯 {node} ({url_str}) is synced");
                    return Ok(());
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                debug!(
                    "⚠️  {node} ({url_str}) RPC call failed (attempt {}/{}): {}",
                    consecutive_failures,
                    max_retries + 1,
                    e
                );

                if consecutive_failures > max_retries {
                    bail!(
                        "❌ {node} ({url_str}) failed after {} attempts: {}",
                        max_retries + 1,
                        e
                    );
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Response from the consensus layer /commit endpoint (subset of fields).
#[derive(Debug, Deserialize)]
struct CommitCertificate {
    round: i64,
}

/// Try to connect to the EL WebSocket within a short timeout.
/// Returns Ok(()) on success, Err on failure. Used to probe node liveness
/// before committing to a full `wait_for_rounds` call.
pub(crate) async fn check_ws_connectable(ws_url: &Url, node_name: &str) -> Result<()> {
    let connect_timeout = Duration::from_secs(10);
    let ws = WsConnect::new(ws_url.to_string());
    tokio::time::timeout(connect_timeout, ProviderBuilder::new().connect_ws(ws))
        .await
        .map_err(|_| color_eyre::eyre::eyre!("Timeout connecting to {node_name} at {ws_url}"))?
        .map_err(|e| {
            color_eyre::eyre::eyre!("Failed to connect to {node_name} at {ws_url}: {e}")
        })?;
    Ok(())
}

/// Subscribe to new block headers via WebSocket, then for each block fetch the decided
/// round from the consensus layer's `/commit?height=N` endpoint. Exits successfully
/// once `consecutive` blocks in a row settle at round 0, or fails on timeout.
pub(crate) async fn wait_for_rounds(
    ws_url: Url,
    cl_url: Url,
    node_name: &str,
    consecutive: u64,
    timeout: Duration,
) -> Result<()> {
    if consecutive == 0 {
        bail!("consecutive must be > 0");
    }

    info!(
        %node_name, %ws_url, %cl_url,
        consecutive, timeout_secs = timeout.as_secs(),
        "Subscribing to block headers to observe consensus rounds"
    );

    // Connect to EL WebSocket
    let ws = WsConnect::new(ws_url.to_string());
    let provider = ProviderBuilder::new().connect_ws(ws).await?;
    let mut subscription = provider.subscribe_blocks().await?;

    let client = reqwest::Client::new();
    let fetch_timeout = Duration::from_secs(5);
    let deadline = Instant::now() + timeout;

    let mut streak: u64 = 0;
    let mut round_histogram: BTreeMap<i64, u64> = BTreeMap::new();
    let mut total_blocks: u64 = 0;

    loop {
        tokio::select! {
            result = subscription.recv() => {
                let header = result?;
                let height = header.number;
                total_blocks += 1;

                // Fetch decided round from CL
                let commit_url = cl_url.join(&format!("commit?height={height}"))?;
                match client.get(commit_url).timeout(fetch_timeout).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.json::<CommitCertificate>().await {
                            Ok(cert) => {
                                *round_histogram.entry(cert.round).or_insert(0) += 1;

                                if cert.round == 0 {
                                    streak += 1;
                                } else {
                                    streak = 0;
                                }

                                info!(
                                    height, round = cert.round,
                                    "height={height} round={} (streak: {streak}/{consecutive})",
                                    cert.round
                                );

                                if streak >= consecutive {
                                    print_round_summary(&round_histogram, total_blocks);
                                    info!("Consensus rounds settled: {consecutive} consecutive blocks at round 0");
                                    return Ok(());
                                }
                            }
                            Err(e) => {
                                warn!(height, streak, "Failed to parse commit certificate (resetting streak): {e}");
                                streak = 0;
                            }
                        }
                    }
                    Ok(resp) => {
                        warn!(height, streak, status = %resp.status(), "Failed to fetch commit certificate (resetting streak)");
                        streak = 0;
                    }
                    Err(e) => {
                        warn!(height, streak, "Failed to fetch commit certificate (resetting streak): {e}");
                        streak = 0;
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                print_round_summary(&round_histogram, total_blocks);
                bail!(
                    "Timeout after {}s waiting for {consecutive} consecutive round-0 blocks \
                     (best streak before timeout: {streak})",
                    timeout.as_secs()
                );
            }
        }
    }
}

fn print_round_summary(histogram: &BTreeMap<i64, u64>, total: u64) {
    info!("Round summary ({total} blocks observed):");
    for (&round, &count) in histogram {
        let pct = if total > 0 {
            (count as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        info!("  round {round}: {count} blocks ({pct:.1}%)");
    }
}
