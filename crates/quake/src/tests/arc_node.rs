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
//! # Using defaults (arc_nodes=ARC_NODES group from manifest, snapshot_provider=snapshot):
//! ./quake test sanity:arc_node
//!
//! # With custom parameters (comma-separated; accepts node names and node groups):
//! ./quake test sanity:arc_node \
//!   --set arc_nodes=ARC_NODES_CONSENSUS \
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
use crate::RemoteSubcommand;

/// Manifest node-group name used as the default when `--set arc_nodes=…` is not
/// provided. If the manifest does not define this group, the default is empty
/// and the caller should skip gracefully.
pub(crate) const DEFAULT_ARC_NODES_GROUP: &str = "ARC_NODES";

/// Resolve the `arc_nodes` test parameter into an explicit list of node names.
///
/// Behavior:
/// - If `--set arc_nodes=…` is provided, split on `,` and run each token
///   through [`crate::manifest::Manifest::resolve_node_selectors`] so users can
///   mix explicit node names and node-group names (e.g.
///   `arc_nodes=ARC_NODES_CONSENSUS,arc-extra`).
/// - If the parameter is not set, return the contents of the
///   [`DEFAULT_ARC_NODES_GROUP`] node-group if it exists in the manifest,
///   otherwise an empty vector (callers should skip gracefully).
pub(crate) fn resolve_arc_nodes(testnet: &Testnet, params: &TestParams) -> Result<Vec<String>> {
    let raw = params.get_or("arc_nodes", "");
    if raw.trim().is_empty() {
        return Ok(testnet
            .manifest
            .runtime_node_groups()
            .get(DEFAULT_ARC_NODES_GROUP)
            .cloned()
            .unwrap_or_default());
    }

    let selectors: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    testnet.manifest.resolve_node_selectors(&selectors)
}

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
        let arc_node_names = resolve_arc_nodes(testnet, params)?;
        if arc_node_names.is_empty() {
            info!(
                "Skipping: no arc nodes resolved (set --set arc_nodes=… or define the \
                 {DEFAULT_ARC_NODES_GROUP} node-group in the manifest)"
            );
            return Ok(());
        }
        let snapshot_provider = params.get_or("snapshot_provider", "snapshot");
        let reference = params.get_or("reference", "validator1");
        let addr = params.get_or("addr", arc_checks::mev::DEFAULT_ADDR);

        // Skip if any required node isn't in the manifest
        let required_singles = [reference.as_str(), snapshot_provider.as_str()];
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
        snapshot_recovery(
            testnet,
            factory,
            &reference,
            &snapshot_provider,
            &arc_node_urls,
        )
        .await?;

        info!("[Phase 2] MEV protection");
        mev_protection(testnet, &arc_node_names, &addr).await?;

        info!("[Phase 3] Mempool empty check (trusted nodes)");
        mempool_empty(testnet, &arc_node_names, &arc_node_urls).await?;

        info!("[Phase 4] Transaction forwarding");
        tx_forwarding(testnet, factory, &reference, &arc_node_urls).await?;

        info!("[DONE] sanity:arc_node passed");
        Ok(())
    })
}

/// Snapshot a provider, restore each arc-node from the snapshot, wait
/// for it to catch up, and verify historical queries succeed.
///
/// Skipped in remote mode, currently requires local Docker Compose and filesystem access.
pub(crate) async fn snapshot_recovery(
    testnet: &Testnet,
    factory: &RpcClientFactory,
    reference: &str,
    snapshot_provider: &str,
    arc_node_urls: &[(NodeName, Url)],
) -> Result<()> {
    if testnet.is_remote() {
        info!("[Phase 1] Snapshot recovery skipped (remote mode)");
        return Ok(());
    }

    info!("Waiting for {reference} to reach height {TARGET_HEIGHT}");
    testnet
        .wait(TARGET_HEIGHT, &[reference.to_string()], WAIT_TIMEOUT)
        .await
        .wrap_err("Reference node did not reach target height")?;

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
            reference,
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
    reference: &str,
    arc_node: &str,
    arc_node_url: &Url,
    snapshot_provider: &str,
    archive_path: &Path,
) -> Result<()> {
    info!("Restoring {arc_node} from snapshot");

    super::snapshot::restore_from_snapshot(testnet, arc_node, snapshot_provider, archive_path)
        .await?;

    tokio::time::sleep(RESTART_SETTLE).await;

    let ref_url = testnet
        .nodes_metadata
        .execution_http_url(reference)
        .ok_or_else(|| color_eyre::eyre::eyre!("{reference} URL not in metadata"))?;
    let ref_client = factory.create(ref_url);
    let current_tip = ref_client
        .get_latest_block_number_with_retries(3)
        .await
        .wrap_err_with(|| format!("Failed to get {reference} block number"))?;

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
pub(crate) async fn mev_protection(
    testnet: &Testnet,
    arc_node_names: &[String],
    addr: &str,
) -> Result<()> {
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
pub(crate) async fn mempool_empty(
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
    if testnet.is_remote() {
        for check in &report.checks {
            if !check.passed {
                info!(
                    "  WARN (remote): mempool not empty on {}: {}",
                    check.name, check.message
                );
            }
        }
        info!("[Phase 3] Mempool check done (warnings only in remote mode)");
    } else {
        for check in &report.checks {
            ensure!(
                check.passed,
                "Mempool check failed on {}: {}",
                check.name,
                check.message
            );
        }
        info!("[Phase 3] All trusted node mempools empty");
    }
    Ok(())
}

/// Phase 4: Send transactions to each arc-node and verify they are forwarded
/// to validators, included in blocks, and the arc-node mempools drain.
///
/// In remote mode, uses `quake remote load` instead of local `testnet.load()`
/// (which requires genesis.json on the local filesystem).
pub(crate) async fn tx_forwarding(
    testnet: &Testnet,
    factory: &RpcClientFactory,
    reference: &str,
    arc_node_urls: &[(NodeName, Url)],
) -> Result<()> {
    let ref_url = testnet
        .nodes_metadata
        .execution_http_url(reference)
        .ok_or_else(|| color_eyre::eyre::eyre!("{reference} URL not in metadata"))?;
    let ref_client = factory.create(ref_url);

    for (arc_node, arc_node_url) in arc_node_urls {
        let client = factory.create(arc_node_url.clone());

        let tip = ref_client
            .get_latest_block_number_with_retries(3)
            .await
            .wrap_err_with(|| format!("Failed to get {reference} height"))?;
        info!("Waiting for {arc_node} to sync to reference tip ({tip})");
        testnet
            .wait(tip, &[arc_node.to_string()], Duration::from_secs(60))
            .await
            .wrap_err_with(|| format!("{arc_node} did not sync to validator tip"))?;

        info!("Sending {LOAD_NUM_TXS} transactions to {arc_node}");
        if testnet.is_remote() {
            let mut args: Vec<String> = vec!["--targets".into(), arc_node.clone()];
            args.extend(["-n".into(), LOAD_NUM_TXS.to_string()]);
            args.extend(["--rate".into(), "10".into()]);
            args.extend(["--mix".into(), "transfer=100".into()]);
            testnet
                .remote(RemoteSubcommand::Load { args })
                .await
                .wrap_err_with(|| format!("Remote load to {arc_node} failed"))?;
        } else {
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
            crate::load::load(testnet, vec![arc_node.clone()], &load_config)
                .await
                .wrap_err_with(|| format!("Failed to send load to {arc_node}"))?;
        }

        let height_after_send = client
            .get_latest_block_number_with_retries(3)
            .await
            .wrap_err_with(|| format!("Failed to get {arc_node} height after send"))?;
        info!("{arc_node} at height {height_after_send} after sending txs");

        // The send takes ~1s; some txs may already be included in the 2-3
        // blocks produced during that window. Look back slightly to cover them.
        let scan_start = height_after_send.saturating_sub(2);
        let scan_end = height_after_send + 6;
        info!("Waiting for {arc_node} to reach height {scan_end}");
        testnet
            .wait(scan_end, &[arc_node.to_string()], Duration::from_secs(30))
            .await
            .wrap_err_with(|| format!("{arc_node} did not advance after load"))?;
        let mut total_txs = 0u64;
        for h in scan_start..=scan_end {
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

        if total_txs < LOAD_NUM_TXS {
            // Diagnostic: check if txs are stuck in arc-node's mempool
            let node_urls = vec![(arc_node.clone(), arc_node_url.clone())];
            match arc_checks::check_mempool(&node_urls).await {
                Ok(report) => {
                    for check in &report.checks {
                        info!("  mempool {arc_node}: {}", check.message);
                    }
                    if !report.passed() {
                        ensure!(
                            false,
                            "Transactions stuck in {arc_node} mempool (not forwarded). \
                             Expected {LOAD_NUM_TXS} in blocks {scan_start}-{scan_end}, found {total_txs}",
                        );
                    }
                }
                Err(e) => {
                    info!("  mempool check unavailable on {arc_node}: {e:#}");
                }
            }
            ensure!(
                false,
                "Expected at least {LOAD_NUM_TXS} transactions in blocks {scan_start}-{scan_end} on {arc_node}, \
                 found {total_txs} (mempool was empty — txs forwarded but landed outside scan window)"
            );
        }
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

/// Standalone tx forwarding test.
///
/// Sends transactions to follow-mode arc-nodes and verifies they are forwarded
/// to validators and included in blocks. Unlike `sanity:arc_node`, this does
/// NOT run snapshot recovery first — the arc-nodes must already be running
/// and in sync.
///
/// # Parameters
///
/// | Key         | Default                        | Description                                                            |
/// |-------------|--------------------------------|------------------------------------------------------------------------|
/// | `arc_nodes` | `ARC_NODES` group              | Comma-separated follow-mode nodes (names or node-group names) to test  |
/// | `reference` | `validator1`                   | Reference node for tip height                                          |
///
/// # Usage
///
/// ```text
/// quake test tx:forward                                   # default: ARC_NODES group
/// quake test tx:forward --set arc_nodes=ARC_NODES_CONSENSUS
/// quake test tx:forward --set arc_nodes=arc-node,rpc-full
/// quake test tx:forward --set reference=validator2
/// ```
#[quake_test(group = "tx", name = "forward")]
fn forward_test<'a>(
    testnet: &'a Testnet,
    factory: &'a RpcClientFactory,
    params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        let arc_node_names = resolve_arc_nodes(testnet, params)?;
        let reference = params.get_or("reference", "validator1");

        let arc_node_urls: Vec<_> = testnet
            .nodes_metadata
            .all_execution_urls()
            .into_iter()
            .filter(|(name, _)| arc_node_names.contains(name))
            .collect();

        if arc_node_urls.is_empty() {
            info!(
                "Skipping: no arc nodes resolved (set --set arc_nodes=… or define the \
                 {DEFAULT_ARC_NODES_GROUP} node-group in the manifest)"
            );
            return Ok(());
        }

        if testnet
            .nodes_metadata
            .execution_http_url(&reference)
            .is_none()
        {
            info!("Skipping: reference node {reference} not in manifest");
            return Ok(());
        }

        tx_forwarding(testnet, factory, &reference, &arc_node_urls).await
    })
}
