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

//! Untrusted-perimeter arc-node sanity test.
//!
//! Combines snapshot recovery, MEV protection, mempool checks, and transaction
//! forwarding into a single end-to-end test for nodes running in the untrusted
//! perimeter.
//!
//! ```text
//! # Using defaults (arc_node=arc-node, snapshot_provider=snapshot):
//! ./quake test sanity:arc_node
//!
//! # With custom parameters:
//! ./quake test sanity:arc_node \
//!   --set arc_node=arc-node \
//!   --set snapshot_provider=snapshot
//! ```
//!
//! # Phases
//!
//! 1. **Snapshot recovery** — snapshot a provider, restore the arc-node, verify
//!    it catches up and serves historical queries.
//! 2. **MEV protection** — verify that relay nodes (derived from
//!    `follow_endpoints`) have MEV protection enabled.
//! 3. **Mempool empty** — assert trusted node mempools have zero pending and zero
//!    queued transactions (delegates to [`arc_checks::check_mempool`]).
//! 4. **Transaction forwarding** — send transactions to each arc-node and verify
//!    they are forwarded, included in blocks, and mempools drain to zero.

use std::path::Path;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre::{ensure, Result, WrapErr};
use tracing::info;
use url::Url;

use super::historical_queries;
use super::{quake_test, RpcClientFactory, TestParams, TestResult};
use crate::node::NodeName;
use crate::testnet::Testnet;

const TARGET_HEIGHT: u64 = 120;
const CATCHUP_TIMEOUT: Duration = Duration::from_secs(120);
const WAIT_TIMEOUT: Duration = Duration::from_secs(600);
const RESTART_SETTLE: Duration = Duration::from_secs(10);
const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";
const LOAD_NUM_TXS: u64 = 10;
#[derive(Parser)]
struct SpammerWrapper {
    #[command(flatten)]
    args: spammer::SpammerArgs,
}

#[quake_test(group = "sanity", name = "arc_node")]
fn arc_node_test<'a>(
    testnet: &'a Testnet,
    factory: &'a RpcClientFactory,
    params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        let arc_node_names: Vec<String> = params
            .get_or("arc_node", "arc-node")
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let snapshot_provider = params.get_or("snapshot_provider", "snapshot");
        let addr = params.get_or("addr", arc_checks::mev::DEFAULT_ADDR);

        // Skip if any required node isn't in the manifest
        let required_singles = ["validator1", snapshot_provider.as_str()];
        let missing_arc: Vec<_> = arc_node_names
            .iter()
            .filter(|n| testnet.nodes_metadata.execution_http_url(n).is_none())
            .map(|n| n.as_str())
            .collect();
        let missing_other: Vec<_> = required_singles
            .iter()
            .filter(|n| testnet.nodes_metadata.execution_http_url(n).is_none())
            .copied()
            .collect();
        if !missing_arc.is_empty() || !missing_other.is_empty() {
            info!(
                "Skipping: missing nodes in manifest: {:?}",
                missing_arc
                    .iter()
                    .chain(missing_other.iter())
                    .collect::<Vec<_>>()
            );
            return Ok(());
        }

        let arc_node_urls: Vec<_> = testnet
            .nodes_metadata
            .all_execution_urls()
            .into_iter()
            .filter(|(name, _)| arc_node_names.contains(name))
            .collect();

        info!("[Phase 1] Snapshot recovery");
        snapshot_recovery(testnet, factory, &snapshot_provider, &arc_node_urls).await?;

        info!("[Phase 2] MEV protection");
        mev_protection(testnet, &arc_node_names, &addr).await?;

        info!("[Phase 3] Mempool empty check (trusted nodes)");
        mempool_empty(testnet, &arc_node_names, &arc_node_urls).await?;

        info!("[Phase 4] Transaction forwarding");
        tx_forwarding(testnet, factory, &arc_node_urls).await?;

        info!("[DONE] sanity:arc_node passed");
        Ok(())
    })
}

/// Snapshot a provider, restore each arc-node from the snapshot, wait
/// for it to catch up, and verify historical queries succeed.
async fn snapshot_recovery(
    testnet: &Testnet,
    factory: &RpcClientFactory,
    snapshot_provider: &str,
    arc_node_urls: &[(NodeName, Url)],
) -> Result<()> {
    info!("Waiting for validator1 to reach height {TARGET_HEIGHT}");
    testnet
        .wait(TARGET_HEIGHT, &["validator1".to_string()], WAIT_TIMEOUT)
        .await
        .wrap_err("Validators did not reach target height")?;

    info!("Waiting for {snapshot_provider} to sync to {TARGET_HEIGHT}");
    testnet
        .wait(
            TARGET_HEIGHT,
            &[snapshot_provider.to_string()],
            WAIT_TIMEOUT,
        )
        .await
        .wrap_err_with(|| format!("{snapshot_provider} did not reach target height"))?;

    let snapshot_dest = testnet.dir.join("snapshots");
    std::fs::create_dir_all(&snapshot_dest)
        .wrap_err("Failed to create snapshot destination directory")?;
    let archive_path =
        super::snapshot::create_snapshot(testnet, snapshot_provider, &snapshot_dest).await?;

    for (arc_node, arc_node_url) in arc_node_urls {
        restore_and_verify(
            testnet,
            factory,
            arc_node,
            arc_node_url,
            snapshot_provider,
            &archive_path,
        )
        .await?;
    }

    verify_cl_store_pruning(testnet, snapshot_provider)?;

    info!("[Phase 1] Snapshot recovery passed");
    Ok(())
}

async fn restore_and_verify(
    testnet: &Testnet,
    factory: &RpcClientFactory,
    arc_node: &str,
    arc_node_url: &Url,
    snapshot_provider: &str,
    archive_path: &Path,
) -> Result<()> {
    info!("Restoring {arc_node} from snapshot");

    super::snapshot::restore_from_snapshot(testnet, arc_node, snapshot_provider, archive_path)
        .await?;

    tokio::time::sleep(RESTART_SETTLE).await;

    let validator_url = testnet
        .nodes_metadata
        .execution_http_url("validator1")
        .ok_or_else(|| color_eyre::eyre::eyre!("validator1 URL not in metadata"))?;
    let validator_client = factory.create(validator_url);
    let current_tip = validator_client
        .get_latest_block_number_with_retries(3)
        .await
        .wrap_err("Failed to get validator1 block number")?;

    info!("Waiting for {arc_node} to catch up to block {current_tip}");
    testnet
        .wait(current_tip, &[arc_node.to_string()], CATCHUP_TIMEOUT)
        .await
        .wrap_err_with(|| format!("{arc_node} did not catch up"))?;

    let client = factory.create(arc_node_url.clone());
    let height = client
        .get_latest_block_number_with_retries(3)
        .await
        .wrap_err_with(|| format!("Failed to get {arc_node} block number"))?;
    info!("{arc_node} at block {height}");
    ensure!(height >= current_tip, "{arc_node} is behind the tip");

    let query_block = height.saturating_sub(10);

    historical_queries::get_block_with_txs(factory, arc_node_url, query_block).await?;
    historical_queries::get_balance_latest(factory, arc_node_url, ZERO_ADDR).await?;
    historical_queries::get_balance(factory, arc_node_url, ZERO_ADDR, query_block).await?;
    historical_queries::get_logs(factory, arc_node_url, height.saturating_sub(5), height).await?;

    info!("{arc_node} snapshot recovery passed");
    Ok(())
}

/// Verify the snapshot provider's CL store.db has been properly pruned.
fn verify_cl_store_pruning(testnet: &Testnet, snapshot_provider: &str) -> Result<()> {
    let store_path = testnet
        .dir
        .join(snapshot_provider)
        .join("malachite")
        .join("store.db");

    if !store_path.exists() {
        info!("CL store.db not found for {snapshot_provider}, skipping pruning check");
        return Ok(());
    }

    info!("Checking CL store pruning on {snapshot_provider}");
    let store_info =
        arc_checks::collect_store_info(&store_path).wrap_err("Failed to collect store info")?;
    info!("CL store:\n{store_info}");

    let pruning_window = 100;
    let margin = 50;
    let report = arc_checks::check_store_pruning(&store_info, pruning_window + margin);
    for check in &report.checks {
        info!(
            "  {} {}",
            if check.passed { "pass" } else { "FAIL" },
            check.message
        );
    }
    ensure!(report.passed(), "CL store pruning check failed");
    Ok(())
}

/// Collect relay nodes that arc-nodes connect to via `follow_endpoints`
/// and verify they have MEV protection enabled.
async fn mev_protection(testnet: &Testnet, arc_node_names: &[String], addr: &str) -> Result<()> {
    let mut relay_names: Vec<String> = Vec::new();
    for name in arc_node_names {
        if let Some(node) = testnet.manifest.nodes.get(name.as_str()) {
            for ep in &node.follow_endpoints {
                if !relay_names.contains(ep) {
                    relay_names.push(ep.clone());
                }
            }
        }
    }

    let relay_urls: Vec<_> = testnet
        .nodes_metadata
        .all_execution_urls()
        .into_iter()
        .filter(|(name, _)| relay_names.contains(name))
        .collect();

    if relay_urls.is_empty() {
        info!("[Phase 2] No relay nodes found (skipped)");
        return Ok(());
    }

    info!(
        "[Phase 2] Checking relay nodes (expect protected): {}",
        relay_urls
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let report = arc_checks::check_pending_state(&relay_urls, addr).await?;
    for check in &report.checks {
        info!(
            "  {} {}",
            if check.passed { "pass" } else { "FAIL" },
            check.message
        );
    }
    ensure!(
        report.passed(),
        "MEV protection missing on relay nodes ({} failures)",
        report.checks.iter().filter(|c| !c.passed).count()
    );
    info!("[Phase 2] MEV protection passed");
    Ok(())
}

/// Phase 3: Check that trusted-perimeter node mempools are empty.
/// Excludes arc-nodes and their relay nodes (which have txpool disabled).
async fn mempool_empty(
    testnet: &Testnet,
    arc_node_names: &[String],
    arc_node_urls: &[(NodeName, Url)],
) -> Result<()> {
    let mut skip: Vec<String> = arc_node_names.to_vec();
    for name in arc_node_names {
        if let Some(node) = testnet.manifest.nodes.get(name.as_str()) {
            for ep in &node.follow_endpoints {
                if !skip.contains(ep) {
                    skip.push(ep.clone());
                }
            }
        }
    }
    // Also skip arc-node URLs that might not be in follow_endpoints
    for (name, _) in arc_node_urls {
        if !skip.contains(name) {
            skip.push(name.clone());
        }
    }

    let trusted_urls: Vec<_> = testnet
        .nodes_metadata
        .all_execution_urls()
        .into_iter()
        .filter(|(name, _)| !skip.contains(name))
        .collect();

    if trusted_urls.is_empty() {
        info!("No trusted nodes to check mempools on");
        return Ok(());
    }

    info!(
        "Checking mempools on: {}",
        trusted_urls
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let report = arc_checks::check_mempool(&trusted_urls).await?;
    for check in &report.checks {
        ensure!(
            check.passed,
            "Mempool check failed on {}: {}",
            check.name,
            check.message
        );
    }
    info!("[Phase 3] All trusted node mempools empty");
    Ok(())
}

/// Phase 4: Send transactions to each arc-node and verify they are forwarded
/// to validators, included in blocks, and the arc-node mempools drain.
async fn tx_forwarding(
    testnet: &Testnet,
    factory: &RpcClientFactory,
    arc_node_urls: &[(NodeName, Url)],
) -> Result<()> {
    let load_config = SpammerWrapper::parse_from([
        "test",
        "-n",
        &LOAD_NUM_TXS.to_string(),
        "--rate",
        "10",
        "--mix",
        "transfer=100",
    ])
    .args
    .to_config(true, false);

    for (arc_node, arc_node_url) in arc_node_urls {
        let client = factory.create(arc_node_url.clone());
        let height_before = client
            .get_latest_block_number_with_retries(3)
            .await
            .wrap_err_with(|| format!("Failed to get {arc_node} block number before load"))?;
        info!("{arc_node} at height {height_before} before load");

        info!("Sending {LOAD_NUM_TXS} transactions to {arc_node}");
        testnet
            .load(vec![arc_node.clone()], &load_config)
            .await
            .wrap_err_with(|| format!("Failed to send load to {arc_node}"))?;

        info!("Waiting for new blocks after load");
        testnet
            .wait(
                height_before + 2,
                &[arc_node.to_string()],
                Duration::from_secs(30),
            )
            .await
            .wrap_err_with(|| format!("{arc_node} did not advance after load"))?;

        let height_after = client
            .get_latest_block_number_with_retries(3)
            .await
            .wrap_err_with(|| format!("Failed to get {arc_node} block number after load"))?;
        info!("{arc_node} at height {height_after} after load");

        let mut total_txs = 0u64;
        for h in (height_before + 1)..=height_after {
            let block =
                super::historical_queries::get_block_with_txs(factory, arc_node_url, h).await?;
            let tx_count = block
                .get("transactions")
                .and_then(|t| t.as_array())
                .map(|a| a.len() as u64)
                .unwrap_or(0);
            if tx_count > 0 {
                info!("  Block {h}: {tx_count} txs");
            }
            total_txs += tx_count;
        }

        ensure!(
            total_txs >= LOAD_NUM_TXS,
            "Expected at least {LOAD_NUM_TXS} transactions in blocks {}-{} on {arc_node}, found {total_txs}",
            height_before + 1,
            height_after
        );
        info!("{arc_node}: {total_txs} transactions included in blocks");

        let node_urls = vec![(arc_node.clone(), arc_node_url.clone())];
        let report = arc_checks::check_mempool(&node_urls).await?;
        ensure!(
            report.passed(),
            "Mempool check failed: {arc_node} mempool is not empty"
        );
        info!("{arc_node} mempool is empty");
    }

    info!("[Phase 4] Transaction forwarding passed");
    Ok(())
}
