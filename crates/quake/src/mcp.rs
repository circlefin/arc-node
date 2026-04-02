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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, ResourcesCapability,
    ServerCapabilities, ServerInfo, ToolsCapability,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::tool;
use rmcp::{ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::info;

use crate::infra::remote;
use crate::perturb::Perturbation;
use crate::rpc;
use crate::testnet::{Testnet, LAST_MANIFEST_FILENAME};
use crate::valset::ValidatorPowerUpdate;

/// Overall timeout for RPC-based observability queries. Prevents tools from
/// hanging when the proxy or SSM tunnel is degraded in remote mode.
const RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Timeout for SSH commands via `remote_ssh`. Longer than RPC_TIMEOUT because
/// SSH involves connection setup, nested hops through CC, and arbitrary commands.
const SSH_TIMEOUT: Duration = Duration::from_secs(60);

/// MCP server that exposes observability and management tools for a running Quake testnet.
pub(crate) struct QuakeMcpServer {
    testnet: Arc<RwLock<Testnet>>,
    tool_router: ToolRouter<Self>,
}

#[rmcp::tool_router]
impl QuakeMcpServer {
    /// Returns an overview of the testnet: block heights, peer counts, and voting power per node.
    #[tool(
        name = "testnet_status",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn testnet_status(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let (node_urls, assets_dir) = {
            let testnet = self.testnet.read().await;
            (
                testnet.nodes_metadata.all_execution_urls(),
                testnet.dir.join("assets"),
            )
        };

        let controllers = rpc::Controllers::load_from_file(&assets_dir).ok();

        let mut lines = Vec::new();
        lines.push(format!(
            "{:<20} {:>8} {:>7} {:>14}",
            "Node", "Height", "Peers", "Voting Power"
        ));
        lines.push("-".repeat(55));

        match tokio::time::timeout(RPC_TIMEOUT, async {
            let mut data_lines = Vec::new();
            if let Some(ref ctrl) = controllers {
                let data = rpc::fetch_latest_data(&node_urls, ctrl).await;
                for (name, (height, peers, contract_validator)) in data {
                    let height_str = height.unwrap_or_else(|e| format!("err: {e}"));
                    let num_peers = peers
                        .map(|p| p.len().to_string())
                        .unwrap_or_else(|_| "?".into());
                    let vp = contract_validator
                        .map(|v| v.votingPower.to_string())
                        .unwrap_or_else(|_| "?".into());
                    data_lines.push(format!(
                        "{:<20} {:>8} {:>7} {:>14}",
                        name, height_str, num_peers, vp
                    ));
                }
            } else {
                let heights = rpc::fetch_latest_heights(&node_urls).await;
                let peers_info = rpc::fetch_peers_info(&node_urls).await;
                let peers_map: HashMap<_, _> =
                    peers_info.into_iter().map(|(n, p)| (n, p.len())).collect();

                for (name, height_result) in heights {
                    let height_str = height_result
                        .map(|h| h.to_string())
                        .unwrap_or_else(|e| format!("err: {e}"));
                    let num_peers = peers_map
                        .get(&name)
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".into());
                    data_lines.push(format!(
                        "{:<20} {:>8} {:>7} {:>14}",
                        name, height_str, num_peers, "n/a"
                    ));
                }
            }
            data_lines
        })
        .await
        {
            Ok(data_lines) => lines.extend(data_lines),
            Err(_) => lines.push(format!(
                "(timed out after {}s fetching node data)",
                RPC_TIMEOUT.as_secs()
            )),
        }

        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }

    /// Lists all nodes in the testnet with their names, IPs, ports, and URLs.
    #[tool(
        name = "list_nodes",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_nodes(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let testnet = self.testnet.read().await;
        let nodes = testnet.nodes_metadata.values();

        let mut entries = Vec::new();
        for node in &nodes {
            let entry = serde_json::json!({
                "name": node.name,
                "public_ip": node.public_ip,
                "execution_http_url": node.execution.http_url.to_string(),
                "execution_ws_url": node.execution.ws_url.to_string(),
                "execution_http_port": node.execution.http_port,
                "execution_ws_port": node.execution.ws_port,
                "consensus_port": node.consensus.consensus_port,
                "consensus_rpc_port": node.consensus.rpc_port,
                "consensus_enabled": node.consensus_enabled,
                "follow": node.follow,
            });
            entries.push(entry);
        }

        let output = serde_json::to_string_pretty(&entries).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Failed to serialize node list: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// Returns the latest block height for each node in the testnet.
    #[tool(
        name = "get_block_heights",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_block_heights(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let node_urls = {
            let testnet = self.testnet.read().await;
            testnet.nodes_metadata.all_execution_urls()
        };

        let mut lines = Vec::new();
        lines.push(format!("{:<20} {:>10}", "Node", "Height"));
        lines.push("-".repeat(32));

        match tokio::time::timeout(RPC_TIMEOUT, rpc::fetch_latest_heights(&node_urls)).await {
            Ok(heights) => {
                for (name, result) in heights {
                    let height_str = result
                        .map(|h| h.to_string())
                        .unwrap_or_else(|e| format!("err: {e}"));
                    lines.push(format!("{:<20} {:>10}", name, height_str));
                }
            }
            Err(_) => lines.push(format!(
                "(timed out after {}s fetching block heights)",
                RPC_TIMEOUT.as_secs()
            )),
        }

        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }

    /// Returns the pending and queued transaction counts in the mempool for each node.
    #[tool(
        name = "get_mempool",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_mempool(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let node_urls = {
            let testnet = self.testnet.read().await;
            testnet.nodes_metadata.all_execution_urls()
        };

        let mut lines = Vec::new();
        lines.push(format!("{:<20} {:>10} {:>10}", "Node", "Pending", "Queued"));
        lines.push("-".repeat(44));

        match tokio::time::timeout(RPC_TIMEOUT, rpc::fetch_mempool_status(&node_urls)).await {
            Ok(mempool) => {
                for (name, (pending, queued)) in mempool {
                    let pending_str = if pending < 0 {
                        "err".to_string()
                    } else {
                        pending.to_string()
                    };
                    let queued_str = if queued < 0 {
                        "err".to_string()
                    } else {
                        queued.to_string()
                    };
                    lines.push(format!(
                        "{:<20} {:>10} {:>10}",
                        name, pending_str, queued_str
                    ));
                }
            }
            Err(_) => lines.push(format!(
                "(timed out after {}s fetching mempool status)",
                RPC_TIMEOUT.as_secs()
            )),
        }

        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }

    /// Returns the connected peers for each node in the testnet.
    #[tool(
        name = "get_peers",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_peers(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let (node_urls, ip_to_name) = {
            let testnet = self.testnet.read().await;
            let node_urls = testnet.nodes_metadata.all_execution_urls();
            let mut ip_to_name: HashMap<String, String> = HashMap::new();
            for (node_name, node_metadata) in testnet.nodes_metadata.nodes.iter() {
                for ip in node_metadata.execution.private_ip_addresses() {
                    ip_to_name.insert(ip, node_name.clone());
                }
            }
            (node_urls, ip_to_name)
        };

        let mut lines = Vec::new();

        match tokio::time::timeout(RPC_TIMEOUT, rpc::fetch_peers_info(&node_urls)).await {
            Ok(peers_info) => {
                for (name, peers) in &peers_info {
                    lines.push(format!("{} ({} peers)", name, peers.len()));
                    if peers.is_empty() {
                        lines.push("  (no peers connected)".to_string());
                    }
                    for peer in peers {
                        let enode_host = peer.enode.split('@').nth(1).unwrap_or("?");
                        let enode_ip = enode_host.split(':').next().unwrap_or("?");
                        let peer_name = ip_to_name
                            .get(enode_ip)
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string());
                        lines.push(format!(
                            "  - {}: local={}, remote={}, inbound={}, trusted={}, static={}",
                            peer_name,
                            peer.network.local_address,
                            peer.network.remote_address,
                            peer.network.inbound,
                            peer.network.trusted,
                            peer.network.static_node,
                        ));
                    }
                }
            }
            Err(_) => lines.push(format!(
                "(timed out after {}s fetching peer info)",
                RPC_TIMEOUT.as_secs()
            )),
        }

        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }

    /// Analyzes gossipsub mesh connectivity across testnet nodes. Returns mesh peer counts,
    /// partition detection, validator connectivity analysis, and explicit peering status.
    #[tool(
        name = "get_mesh",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_mesh(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let testnet = self.testnet.read().await;
        let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();
        let raw_metrics = crate::mesh::fetch_all_metrics(&metrics_urls).await;
        let nodes_data = crate::mesh::parse_all_metrics(&raw_metrics);

        if nodes_data.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No nodes responded to metrics requests. Is the testnet running?",
            )]));
        }

        let analysis = crate::mesh::analyze(&nodes_data);
        let options = crate::mesh::MeshDisplayOptions {
            show_counts: true,
            show_mesh: true,
            show_peers: false,
            show_peers_full: false,
        };
        let report = crate::mesh::format_report(&analysis, &options);
        Ok(CallToolResult::success(vec![Content::text(report)]))
    }

    /// Returns performance metrics (block latency and throughput) for all testnet nodes.
    /// Shows cumulative statistics since each node's process start.
    #[tool(
        name = "get_perf",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_perf(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let testnet = self.testnet.read().await;
        let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();
        let raw_metrics = arc_checks::fetch_all_metrics(&metrics_urls).await;
        let mut nodes = arc_checks::parse_perf_metrics(&raw_metrics);

        if nodes.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No nodes responded to metrics requests. Is the testnet running?",
            )]));
        }

        crate::util::assign_node_groups(
            nodes.iter_mut().map(|n| (n.name.as_str(), &mut n.group)),
            &testnet.manifest.nodes,
        );

        let options = arc_checks::PerfDisplayOptions::default();
        let report = arc_checks::format_perf_report(&nodes, &options);
        Ok(CallToolResult::success(vec![Content::text(report)]))
    }

    /// Reports consensus health metrics: round breakdown (R0/R1/R>1), height restart counts,
    /// and sync-fell-behind counts per node. Useful for assessing network stability.
    #[tool(
        name = "get_health",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_health(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let testnet = self.testnet.read().await;
        let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();
        let raw_metrics = arc_checks::fetch_all_metrics(&metrics_urls).await;
        let mut nodes_data = arc_checks::parse_all_health_metrics(&raw_metrics);

        if nodes_data.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No nodes responded to metrics requests. Is the testnet running?",
            )]));
        }

        crate::util::assign_node_groups(
            nodes_data
                .iter_mut()
                .map(|n| (n.name.as_str(), &mut n.group)),
            &testnet.manifest.nodes,
        );

        let report = arc_checks::format_health_report(&nodes_data);
        Ok(CallToolResult::success(vec![Content::text(report)]))
    }

    // ── Lifecycle tools ─────────────────────────────────────────────────

    /// Starts testnet nodes. If specific node names are provided, only those nodes are started;
    /// otherwise all nodes are started.
    #[tool(
        name = "start_nodes",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn start_nodes(
        &self,
        params: Parameters<NodeNamesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let names = params.0.nodes.unwrap_or_default();
        let label = if names.is_empty() {
            "all nodes".to_string()
        } else {
            names.join(", ")
        };
        let testnet = self.testnet.read().await;
        testnet.start(names).await.map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Failed to start nodes: {e}"), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Started: {label}"
        ))]))
    }

    /// Stops testnet nodes. If specific node names are provided, only those nodes are stopped;
    /// otherwise all nodes are stopped.
    #[tool(
        name = "stop_nodes",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn stop_nodes(
        &self,
        params: Parameters<NodeNamesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let names = params.0.nodes.unwrap_or_default();
        let label = if names.is_empty() {
            "all nodes".to_string()
        } else {
            names.join(", ")
        };
        let testnet = self.testnet.read().await;
        testnet.stop(names).await.map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Failed to stop nodes: {e}"), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Stopped: {label}"
        ))]))
    }

    /// Cleans up testnet data and infrastructure.
    ///
    /// Both modes remove testnet data (databases and generated files). If `all` is true,
    /// monitoring services are also stopped and their data is removed.
    #[tool(
        name = "clean_testnet",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn clean_testnet(
        &self,
        params: Parameters<CleanParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let all = params.0.all.unwrap_or(false);
        let testnet = self.testnet.read().await;
        testnet.clean(all).await.map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Failed to clean testnet: {e}"), None)
        })?;
        let scope = if all { "full" } else { "partial" };
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Testnet cleaned ({scope})"
        ))]))
    }

    /// Restarts testnet nodes by stopping then starting them. If specific node names are
    /// provided, only those nodes are restarted; otherwise all nodes are restarted.
    /// Data (databases, config) is preserved. For a full reset, use clean_testnet + start_nodes.
    ///
    /// If the stop succeeds but start fails, nodes remain stopped — use start_nodes to recover.
    #[tool(
        name = "restart_testnet",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn restart_testnet(
        &self,
        params: Parameters<NodeNamesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let names = params.0.nodes.unwrap_or_default();
        let label = if names.is_empty() {
            "all nodes".to_string()
        } else {
            names.join(", ")
        };
        {
            let testnet = self.testnet.read().await;
            testnet.stop(names.clone()).await.map_err(|e| {
                rmcp::ErrorData::internal_error(format!("Failed to stop nodes: {e}"), None)
            })?;
        }
        {
            let testnet = self.testnet.read().await;
            testnet.start(names).await.map_err(|e| {
                rmcp::ErrorData::internal_error(format!("Failed to start nodes: {e}"), None)
            })?;
        }
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Restarted: {label}"
        ))]))
    }

    // ── Perturbation tools ──────────────────────────────────────────────

    /// Disconnects target nodes from the network for a period, then reconnects them.
    /// Simulates a network partition.
    #[tool(
        name = "perturb_disconnect",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn perturb_disconnect(
        &self,
        params: Parameters<PerturbTimedParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let p = params.0;
        let time_off = parse_duration_opt(p.duration.as_deref())?;
        let action = Perturbation::Disconnect {
            targets: p.targets.clone(),
            time_off,
        };
        self.apply_perturbation(action, &p.targets).await
    }

    /// Kills target node containers, waits for a period, then restarts them.
    #[tool(
        name = "perturb_kill",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn perturb_kill(
        &self,
        params: Parameters<PerturbTimedParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let p = params.0;
        let time_off = parse_duration_opt(p.duration.as_deref())?;
        let action = Perturbation::Kill {
            targets: p.targets.clone(),
            time_off,
        };
        self.apply_perturbation(action, &p.targets).await
    }

    /// Pauses target node containers for a period, then unpauses them.
    #[tool(
        name = "perturb_pause",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn perturb_pause(
        &self,
        params: Parameters<PerturbTimedParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let p = params.0;
        let time_off = parse_duration_opt(p.duration.as_deref())?;
        let action = Perturbation::Pause {
            targets: p.targets.clone(),
            time_off,
        };
        self.apply_perturbation(action, &p.targets).await
    }

    /// Restarts target node containers cleanly.
    #[tool(
        name = "perturb_restart",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn perturb_restart(
        &self,
        params: Parameters<PerturbTargetsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let p = params.0;
        let action = Perturbation::Restart {
            targets: p.targets.clone(),
        };
        self.apply_perturbation(action, &p.targets).await
    }

    /// Upgrades target node containers by stopping them and restarting with the upgraded image.
    #[tool(
        name = "perturb_upgrade",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn perturb_upgrade(
        &self,
        params: Parameters<PerturbTargetsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let p = params.0;
        let action = Perturbation::Upgrade {
            targets: p.targets.clone(),
            time_off: None,
        };
        self.apply_perturbation(action, &p.targets).await
    }

    // ── Testing tools ───────────────────────────────────────────────────

    /// Runs end-to-end tests against the testnet. Optionally filter by test spec or perform a
    /// dry run.
    #[tool(
        name = "run_tests",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn run_tests(
        &self,
        params: Parameters<RunTestsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let p = params.0;
        let spec = p.spec.unwrap_or_default();
        let dry_run = p.dry_run.unwrap_or(false);
        // Short timeout: tests poll rapidly and individual RPC failures are retried.
        let rpc_timeout = Duration::from_secs(1);

        let testnet = self.testnet.read().await;
        let test_params = crate::tests::TestParams::default();
        testnet
            .run_tests(&spec, dry_run, rpc_timeout, &test_params)
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(format!("Tests failed: {e}"), None))?;

        let mode = if dry_run { " (dry run)" } else { "" };
        let filter = if spec.is_empty() {
            "all".to_string()
        } else {
            spec
        };
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Tests completed{mode}: {filter}"
        ))]))
    }

    /// Waits until the specified nodes reach a target block height, with an optional timeout.
    #[tool(
        name = "wait_height",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn wait_height(
        &self,
        params: Parameters<WaitHeightParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let p = params.0;
        let node_names = p.nodes.unwrap_or_default();
        let timeout = match p.timeout.as_deref() {
            Some(s) => parse_duration_str(s)?,
            None => Duration::from_secs(60),
        };

        let testnet = self.testnet.read().await;
        testnet
            .wait(p.height, &node_names, timeout)
            .await
            .map_err(|e| {
                rmcp::ErrorData::internal_error(
                    format!("Wait for height {} failed: {e}", p.height),
                    None,
                )
            })?;

        let nodes_label = if node_names.is_empty() {
            "all nodes".to_string()
        } else {
            node_names.join(", ")
        };
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Reached height {} on {nodes_label}",
            p.height
        ))]))
    }

    /// Waits until consensus rounds settle at 0. Subscribes to new block headers via
    /// WebSocket, then for each block fetches the decided round from the consensus layer.
    /// Exits successfully once N consecutive blocks settle at round 0.
    #[tool(
        name = "wait_rounds",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn wait_rounds(
        &self,
        params: Parameters<WaitRoundsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let p = params.0;
        let consecutive = p.consecutive.unwrap_or(10);
        let timeout = match p.timeout.as_deref() {
            Some(s) => parse_duration_str(s)?,
            None => Duration::from_secs(120),
        };

        let testnet = self.testnet.read().await;
        testnet
            .wait_rounds(consecutive, timeout)
            .await
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("Wait for rounds failed: {e}"), None)
            })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Consensus rounds settled: {consecutive} consecutive blocks at round 0"
        ))]))
    }

    /// Updates the voting power of one or more validators in the testnet.
    #[tool(
        name = "valset_update",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn valset_update(
        &self,
        params: Parameters<ValsetUpdateParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let updates: Vec<ValidatorPowerUpdate> = params
            .0
            .updates
            .into_iter()
            .map(|u| ValidatorPowerUpdate {
                validator_name: u.validator,
                new_voting_power: u.power,
            })
            .collect();

        let summary: Vec<String> = updates
            .iter()
            .map(|u| format!("{}:{}", u.validator_name, u.new_voting_power))
            .collect();

        let testnet = self.testnet.read().await;
        testnet.valset_update(updates).await.map_err(|e| {
            rmcp::ErrorData::internal_error(format!("Validator set update failed: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Validator set updated: {}",
            summary.join(", ")
        ))]))
    }

    // ── Remote tools ────────────────────────────────────────────────────

    /// Runs a command on a remote node or the Control Center (CC) server via SSH
    /// and returns the output. Only available for remote testnets.
    ///
    /// Use a node name (e.g. "validator-blue") to reach a specific node, or
    /// "cc" for the Control Center server. CC is the control plane — it does
    /// NOT run docker containers. Use node names for docker/compose commands.
    ///
    /// Security: this tool runs arbitrary commands on the remote host. Only
    /// expose the MCP server to trusted users or processes.
    ///
    /// Examples:
    ///   node="validator-blue", command="docker logs el --tail 50"
    ///   node="validator-red", command="docker compose ps"
    ///   node="cc", command="df -h"
    ///   node="cc", command="ls shared/"
    #[tool(
        name = "remote_ssh",
        annotations(read_only_hint = false, open_world_hint = true)
    )]
    async fn remote_ssh(
        &self,
        params: Parameters<RemoteSshParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let p = params.0;
        let infra = {
            let testnet = self.testnet.read().await;
            testnet.remote_infra().map_err(|e| {
                rmcp::ErrorData::invalid_params(
                    format!("remote_ssh requires a remote testnet: {e}"),
                    None,
                )
            })?
        };

        let node = p.node.trim().to_string();
        if node.is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "node must be non-empty".to_string(),
                None,
            ));
        }
        let command = p.command;
        if command.trim().is_empty() {
            return Err(rmcp::ErrorData::invalid_params(
                "command must be non-empty".to_string(),
                None,
            ));
        }

        infra.instance_id(&node).map_err(|e| {
            rmcp::ErrorData::invalid_params(
                format!("Invalid node '{node}': {e}. Must be a node name or 'cc'"),
                None,
            )
        })?;

        let node_label = node.clone();

        let output = match tokio::time::timeout(
            SSH_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                if node == remote::CC_INSTANCE {
                    infra.ssh_cc_with_output(&command)
                } else {
                    infra.ssh_node_with_output(&node, &command)
                }
            }),
        )
        .await
        {
            Ok(join_result) => join_result
                .map_err(|e| {
                    rmcp::ErrorData::internal_error(format!("SSH task panicked: {e}"), None)
                })?
                .map_err(|e| {
                    rmcp::ErrorData::internal_error(
                        format!("SSH to {node_label} failed: {e}"),
                        None,
                    )
                })?,
            Err(_) => {
                return Err(rmcp::ErrorData::internal_error(
                    format!(
                        "SSH to {node_label} timed out after {}s",
                        SSH_TIMEOUT.as_secs()
                    ),
                    None,
                ));
            }
        };

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    /// Manages SSM port-forwarding tunnels to the Control Center server.
    /// Required for RPC and SSH access to remote nodes and monitoring services.
    /// Only available for remote testnets.
    ///
    /// Actions:
    ///   "start" — opens inactive tunnels (idempotent)
    ///   "stop"  — closes all active tunnels
    ///   "list"  — shows active tunnel status
    #[tool(
        name = "remote_ssm",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn remote_ssm(
        &self,
        params: Parameters<RemoteSsmParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let action = params.0.action;
        let infra = {
            let testnet = self.testnet.read().await;
            testnet.remote_infra().map_err(|e| {
                rmcp::ErrorData::invalid_params(
                    format!("remote_ssm requires a remote testnet: {e}"),
                    None,
                )
            })?
        };

        match action {
            crate::SSMSubcommand::Start => {
                infra.ssm_tunnels.start().await.map_err(|e| {
                    rmcp::ErrorData::internal_error(
                        format!("Failed to start SSM tunnels: {e}"),
                        None,
                    )
                })?;
                Ok(CallToolResult::success(vec![Content::text(
                    "SSM tunnels started",
                )]))
            }
            crate::SSMSubcommand::Stop => {
                infra.ssm_tunnels.stop().await.map_err(|e| {
                    rmcp::ErrorData::internal_error(
                        format!("Failed to stop SSM tunnels: {e}"),
                        None,
                    )
                })?;
                Ok(CallToolResult::success(vec![Content::text(
                    "SSM tunnels stopped",
                )]))
            }
            crate::SSMSubcommand::List => {
                let output = infra.ssm_tunnels.list_formatted().map_err(|e| {
                    rmcp::ErrorData::internal_error(
                        format!("Failed to list SSM tunnels: {e}"),
                        None,
                    )
                })?;
                Ok(CallToolResult::success(vec![Content::text(output)]))
            }
        }
    }

    /// Uploads testnet configuration files (genesis, node configs, compose.yaml)
    /// to the Control Center server. Nodes access these files via NFS.
    /// Run this after modifying testnet configuration to push changes to remote nodes.
    /// Only available for remote testnets.
    #[tool(
        name = "remote_provision",
        annotations(read_only_hint = false, open_world_hint = false)
    )]
    async fn remote_provision(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        self.ensure_ssm_tunnels().await?;
        let infra = {
            let testnet = self.testnet.read().await;
            testnet.remote_infra().map_err(|e| {
                rmcp::ErrorData::invalid_params(
                    format!("remote_provision requires a remote testnet: {e}"),
                    None,
                )
            })?
        };

        tokio::task::spawn_blocking(move || infra.provision())
            .await
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("Provision task panicked: {e}"), None)
            })?
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("Provisioning failed: {e}"), None)
            })?;

        Ok(CallToolResult::success(vec![Content::text(
            "Testnet files provisioned to Control Center",
        )]))
    }
}

// ── Parameter structs ───────────────────────────────────────────────────

/// Parameters for tools that accept an optional list of node names.
#[derive(Debug, Deserialize, JsonSchema)]
struct NodeNamesParams {
    /// Optional list of node or container names. If empty or omitted, applies to all nodes.
    nodes: Option<Vec<String>>,
}

/// Parameters for the clean_testnet tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct CleanParams {
    /// If true, also stop monitoring services and remove monitoring data.
    /// Testnet data (including generated files) is removed in both modes.
    all: Option<bool>,
}

/// Parameters for timed perturbation tools (disconnect, kill, pause).
#[derive(Debug, Deserialize, JsonSchema)]
struct PerturbTimedParams {
    /// Target node or container names to perturb.
    targets: Vec<String>,
    /// How long the perturbation should last, e.g. "10s", "1m". If omitted, a random
    /// duration is chosen.
    duration: Option<String>,
}

/// Parameters for non-timed perturbation tools (restart, upgrade).
#[derive(Debug, Deserialize, JsonSchema)]
struct PerturbTargetsParams {
    /// Target node or container names to perturb.
    targets: Vec<String>,
}

/// Parameters for the run_tests tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct RunTestsParams {
    /// Optional test filter spec (e.g. a test name or pattern). Runs all tests if omitted.
    spec: Option<String>,
    /// If true, list matching tests without running them. Defaults to false.
    dry_run: Option<bool>,
}

/// Parameters for the wait_height tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct WaitHeightParams {
    /// Target block height to wait for.
    height: u64,
    /// Optional list of node names to wait on. Waits on all nodes if omitted.
    nodes: Option<Vec<String>>,
    /// Optional timeout duration string, e.g. "60s", "2m". Defaults to 60s.
    timeout: Option<String>,
}

/// Parameters for the wait_rounds tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct WaitRoundsParams {
    /// Number of consecutive round-0 blocks required. Defaults to 10.
    consecutive: Option<u64>,
    /// Optional timeout duration string, e.g. "60s", "2m". Defaults to 120s.
    timeout: Option<String>,
}

/// A single validator power update entry.
#[derive(Debug, Deserialize, JsonSchema)]
struct ValsetUpdateEntry {
    /// Validator name, e.g. "validator1".
    validator: String,
    /// New voting power for this validator.
    power: u64,
}

/// Parameters for the valset_update tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct ValsetUpdateParams {
    /// List of validator power updates to apply.
    updates: Vec<ValsetUpdateEntry>,
}

/// Parameters for the remote_ssh tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct RemoteSshParams {
    /// Node name (e.g. "validator-blue") or "cc" for the Control Center server.
    node: String,
    /// Command to execute on the remote host via SSH.
    command: String,
}

/// Parameters for the remote_ssm tool.
#[derive(Debug, Deserialize, JsonSchema)]
struct RemoteSsmParams {
    /// Action to perform: "start", "stop", or "list".
    action: crate::SSMSubcommand,
}

// ── Helper methods ──────────────────────────────────────────────────────

/// Parse an optional human-readable duration string (e.g. "10s", "1m30s") into a `Duration`.
fn parse_duration_opt(s: Option<&str>) -> Result<Option<Duration>, rmcp::ErrorData> {
    match s {
        Some(s) => {
            let d = humantime::parse_duration(s).map_err(|e| {
                rmcp::ErrorData::invalid_params(format!("Invalid duration '{s}': {e}"), None)
            })?;
            Ok(Some(d))
        }
        None => Ok(None),
    }
}

/// Parse a required human-readable duration string into a `Duration`.
fn parse_duration_str(s: &str) -> Result<Duration, rmcp::ErrorData> {
    humantime::parse_duration(s)
        .map_err(|e| rmcp::ErrorData::invalid_params(format!("Invalid duration '{s}': {e}"), None))
}

impl QuakeMcpServer {
    fn new(testnet: Testnet) -> Self {
        let testnet = Arc::new(RwLock::new(testnet));
        let tool_router = Self::tool_router();
        Self {
            testnet,
            tool_router,
        }
    }

    /// Ensure SSM tunnels are active before performing remote operations.
    ///
    /// For local testnets this is a no-op. For remote testnets it calls the
    /// idempotent `ssm_tunnels.start()` which only opens inactive sessions.
    async fn ensure_ssm_tunnels(&self) -> Result<(), rmcp::ErrorData> {
        let ssm = {
            let testnet = self.testnet.read().await;
            testnet.remote_infra().ok().map(|i| i.ssm_tunnels.clone())
        };
        if let Some(ssm) = ssm {
            ssm.start().await.map_err(|e| {
                rmcp::ErrorData::internal_error(format!("Failed to start SSM tunnels: {e}"), None)
            })?;
        }
        Ok(())
    }

    /// Shared helper that applies a perturbation action through the testnet.
    async fn apply_perturbation(
        &self,
        action: Perturbation,
        targets: &[String],
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let label = format!("{action}");
        let min_time_off = Duration::from_secs(10);
        let max_time_off = Duration::from_secs(20);

        let mut testnet = self.testnet.write().await;
        testnet
            .with_seed(None)
            .perturb(action, min_time_off, max_time_off)
            .await
            .map_err(|e| {
                rmcp::ErrorData::internal_error(
                    format!("Perturbation failed on {}: {e}", targets.join(", ")),
                    None,
                )
            })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Perturbation applied: {label}"
        ))]))
    }
}

#[rmcp::tool_handler]
impl ServerHandler for QuakeMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: Default::default(),
            capabilities: ServerCapabilities {
                resources: Some(ResourcesCapability {
                    subscribe: None,
                    list_changed: Some(false),
                }),
                tools: Some(ToolsCapability {
                    list_changed: Some(false),
                }),
                ..Default::default()
            },
            server_info: Implementation {
                name: "quake-mcp-server".to_string(),
                title: None,
                version: arc_version::SHORT_VERSION.to_string(),
                description: Some(
                    "MCP server for observing and managing Arc testnet state via Quake".to_string(),
                ),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "This server provides tools for observing and managing a running Arc testnet via Quake.\n\n\
                 Observability: testnet_status (overview), list_nodes (node details), get_block_heights \
                 (chain heights), get_mempool (transaction pool), get_peers (network connectivity), \
                 get_mesh (gossipsub mesh analysis), get_perf (block latency and throughput), \
                 get_health (consensus health metrics).\n\n\
                 Lifecycle: start_nodes, stop_nodes, restart_testnet, clean_testnet.\n\n\
                 Perturbations: perturb_disconnect (network partition), perturb_kill (force stop/restart), \
                 perturb_pause (freeze/unpause), perturb_restart (clean restart), perturb_upgrade \
                 (upgrade to new image).\n\n\
                 Testing: run_tests (E2E tests), wait_height (wait for block height), \
                 valset_update (update validator voting power).\n\n\
                 Remote: remote_ssh (run a command on a node or CC via SSH), \
                 remote_ssm (manage SSM tunnels: start/stop/list), \
                 remote_provision (upload config files to CC). Remote testnets only.\n\n\
                 Resources: quake://manifest (testnet TOML config), quake://nodes (node metadata JSON)."
                    .to_string(),
            ),
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_
    {
        let manifest_resource = {
            let mut r = rmcp::model::RawResource::new("quake://manifest", "Testnet manifest");
            r.description = Some("Current testnet manifest (TOML configuration)".to_string());
            r.mime_type = Some("application/toml".to_string());
            Resource::new(r, None)
        };
        let nodes_resource = {
            let mut r = rmcp::model::RawResource::new("quake://nodes", "Node metadata");
            r.description = Some("All node metadata as JSON".to_string());
            r.mime_type = Some("application/json".to_string());
            Resource::new(r, None)
        };
        std::future::ready(Ok(ListResourcesResult {
            meta: None,
            resources: vec![manifest_resource, nodes_resource],
            next_cursor: None,
        }))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, rmcp::ErrorData> {
        let testnet = self.testnet.read().await;

        match request.uri.as_str() {
            "quake://manifest" => {
                // Read the manifest TOML from the .last_manifest pointer
                let last_manifest_path = testnet.quake_dir.join(LAST_MANIFEST_FILENAME);
                let manifest_file_path =
                    std::fs::read_to_string(&last_manifest_path).map_err(|e| {
                        rmcp::ErrorData::internal_error(
                            format!("Failed to read .last_manifest: {e}"),
                            None,
                        )
                    })?;
                let manifest_toml =
                    std::fs::read_to_string(manifest_file_path.trim()).map_err(|e| {
                        rmcp::ErrorData::internal_error(
                            format!("Failed to read manifest file: {e}"),
                            None,
                        )
                    })?;
                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(manifest_toml, "quake://manifest")],
                })
            }
            "quake://nodes" => {
                let nodes_json =
                    serde_json::to_string_pretty(&testnet.nodes_metadata).map_err(|e| {
                        rmcp::ErrorData::internal_error(
                            format!("Failed to serialize nodes: {e}"),
                            None,
                        )
                    })?;
                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(nodes_json, "quake://nodes")],
                })
            }
            other => Err(rmcp::ErrorData::invalid_params(
                format!("Unknown resource URI: {other}"),
                None,
            )),
        }
    }
}

/// Entry point to start the MCP server.
///
/// Supports two transport modes:
/// - stdio (default): for direct integration with Claude Code, Cursor, etc.
/// - HTTP+SSE (`--http`): for remote clients over the network
pub(crate) async fn run_server(testnet: Testnet, http: bool, port: u16) -> Result<()> {
    if http {
        run_http_server(testnet, port).await
    } else {
        run_stdio_server(testnet).await
    }
}

async fn run_stdio_server(testnet: Testnet) -> Result<()> {
    info!("Starting MCP server on stdio transport");

    let server = QuakeMcpServer::new(testnet);
    let transport = rmcp::transport::stdio();
    let service = server
        .serve(transport)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("Failed to start MCP stdio server: {e}"))?;

    info!("MCP server running on stdio");
    service
        .waiting()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("MCP server error: {e}"))?;

    info!("MCP server stopped");
    Ok(())
}

async fn run_http_server(testnet: Testnet, port: u16) -> Result<()> {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::StreamableHttpService;

    info!(port, "Starting MCP server on HTTP+SSE transport");

    let testnet = Arc::new(RwLock::new(testnet));

    let config = rmcp::transport::StreamableHttpServerConfig::default();
    let session_manager = Arc::new(LocalSessionManager::default());

    let service = StreamableHttpService::new(
        {
            let testnet = Arc::clone(&testnet);
            move || {
                Ok(QuakeMcpServer {
                    testnet: Arc::clone(&testnet),
                    tool_router: QuakeMcpServer::tool_router(),
                })
            }
        },
        session_manager,
        config,
    );

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .map_err(|e| color_eyre::eyre::eyre!("Failed to bind to port {port}: {e}"))?;

    info!(port, "MCP HTTP+SSE server listening");

    let app = axum::Router::new().fallback_service(service);
    axum::serve(listener, app)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("MCP HTTP server error: {e}"))?;

    info!("MCP HTTP server stopped");
    Ok(())
}
