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

//! Helper functions for the `info` command.

use std::collections::HashMap;
use std::path::Path;

use alloy_rpc_types_admin::{EthPeerInfo, SnapPeerInfo};
use color_eyre::eyre::{self, Report, Result};
use reqwest::Url;
use tokio::time::Duration;
use tracing::warn;

use crate::infra::{InfraData, NodeInfraData};
use crate::nodes::NodesMetadata;
use crate::rpc::valset_manager::ContractValidatorStatus;
use crate::rpc::{self, Controllers};

/// Print information about the nodes and their containers
pub(crate) fn print_nodes_info(nodes: &NodesMetadata) {
    let max_name_len = nodes.max_node_name_len();
    let nodes = nodes.values();

    #[rustfmt::skip]
    println!(
        "  {:<max_name_len$} | CL {:>10} {:>8} {:>7} | EL {:>5} {:>5} {:>8} {:>8} | {:>9} {:>6} {:>6}",
        "",
        "Consensus", "RPC", "Metrics",
        "HTTP", "WS", "AuthRPC", "Metrics",
        "consensus", "follow", "endpoints"
    );
    for node in nodes {
        print!("  {:<max_name_len$} |", node.name);
        print!(
            "    {:>10} {:>8} {:>7} |",
            node.consensus.consensus_port, node.consensus.rpc_port, node.consensus.metrics_port
        );
        print!(
            "    {:>5} {:>5} {:>8} {:>8} |",
            node.execution.http_port,
            node.execution.ws_port,
            node.execution.authrpc_port,
            node.execution.metrics_port
        );
        println!(
            " {:>9} {:>6} {:>6}",
            if node.consensus_enabled {
                "enabled"
            } else {
                "disabled"
            },
            node.follow,
            node.follow_endpoints.join(",")
        );
    }
}

pub(crate) fn print_nodes_ip_addresses(nodes: &NodesMetadata) {
    let max_name_len = nodes.max_node_name_len();
    let nodes = nodes.values();
    let max_public_ip_len = nodes.iter().map(|n| n.public_ip.len()).max().unwrap_or(12);

    let cl_ips_str = nodes
        .iter()
        .map(|n| n.consensus.subnet_ips.to_string())
        .collect::<Vec<_>>();
    let max_cl_ips_str_len = cl_ips_str.iter().map(|s| s.len()).max().unwrap_or(0);

    let el_ips_str = nodes
        .iter()
        .map(|n| n.execution.subnet_ips.to_string())
        .collect::<Vec<_>>();
    let max_el_ips_str_len = el_ips_str.iter().map(|s| s.len()).max().unwrap_or(0);

    #[rustfmt::skip]
    println!(
        "  {:<max_name_len$} {:>max_public_ip_len$} | {:<max_cl_ips_str_len$} | {:<max_el_ips_str_len$}",
        "", "Public IP", "CL Private IPs", "EL Private IPs"
    );
    for (index, node) in nodes.iter().enumerate() {
        println!(
            "  {:<max_name_len$} {:>max_public_ip_len$} | {:<max_cl_ips_str_len$} | {:<max_el_ips_str_len$}",
            node.name,
            node.public_ip,
            &cl_ips_str[index],
            &el_ips_str[index]
        );
    }
}

fn instance_id_or_unknown(name: &str, data: &NodeInfraData) -> String {
    data.instance_id().unwrap_or_else(|e| {
        warn!(%name, %e, "Instance ID not found");
        "unknown".to_string()
    })
}

pub(crate) fn print_remote_infra_data(infra_data: &InfraData) {
    let max_name_len = infra_data
        .max_node_name_len()
        .unwrap_or(14)
        .max("Control Center".len());
    let max_public_ip_len = infra_data.max_public_ip_len().unwrap_or(12);

    println!(
        " {:<max_name_len$} | {:>max_public_ip_len$} | Instance ID",
        "", "Public IP"
    );
    if let Some(cc) = infra_data.control_center.as_ref() {
        println!(
            " {:<max_name_len$} | {:>max_public_ip_len$} | {}",
            "Control Center",
            cc.public_ip,
            instance_id_or_unknown("Control Center", cc)
        );
    }
    for (node_name, node) in infra_data.nodes.iter() {
        println!(
            " {:<max_name_len$} | {:>max_public_ip_len$} | {}",
            node_name,
            node.public_ip,
            instance_id_or_unknown(node_name, node)
        );
    }
}

/// Fetch and print the consensus public key and address of each node
pub(crate) async fn print_keys(
    node_urls: &[(String, Url)],
    controllers: &Controllers,
    max_name_len: usize,
) {
    println!(
        "  {:<max_name_len$} | {:<42} | {:<64} | {:<40}",
        "", "Controller", "Public Key", "Address"
    );
    for (name, (controller, public_key, address)) in
        rpc::fetch_node_keys(node_urls, controllers).await
    {
        let controller = format_result_cell(controller, 42, false);
        let public_key = format_result_cell(public_key, 64, false);
        let address = format_result_cell(address, 40, false);
        println!("  {name:<max_name_len$} | {controller} | {public_key} | {address}");
    }
}

pub(crate) async fn print_latest_data(
    node_urls: &[(String, Url)],
    controllers: &Controllers,
    cl_mesh_peers: &HashMap<String, i64>,
    max_name_len: usize,
) {
    println!(
        "  {:<max_name_len$} | Height | Peers | CL Mesh | {:<12} | {:<6}",
        "", "Voting Power", "Status"
    );
    for (name, (height, peers, contract_validator)) in
        rpc::fetch_latest_data(node_urls, controllers).await
    {
        let height = format_result_cell(height, 6, true);
        let peers = peers.unwrap_or_default();

        let num_peers = peers.len();
        let cl_mesh = cl_mesh_peers
            .get(&name)
            .map(|c| format!("{c:>7}"))
            .unwrap_or_else(|| format!("{:>7}", "-"));
        let (voting_power, status) = match contract_validator {
            Ok(validator) => (format!("{:>12}", validator.votingPower), validator.status),
            Err(e) => (
                red(&error_to_short_string(e), 12),
                ContractValidatorStatus::Unknown,
            ),
        };
        println!(
            "  {name:<max_name_len$} | {height} | {num_peers:>5} | {cl_mesh} | {voting_power} | {status:?}"
        );
    }
}

/// Fetch the latest block height of a single node.
pub(crate) async fn get_node_height(nodes: &NodesMetadata, node: &str) -> Result<u64> {
    let url = nodes
        .execution_http_url(node)
        .ok_or_else(|| eyre::eyre!("Unknown node '{node}'"))?;
    let client = rpc::RpcClient::new(url, Duration::from_secs(5));
    client.get_latest_block_number_with_retries(0).await
}

pub(crate) async fn loop_print_latest_heights(
    node_urls: &[(String, Url)],
    max_rounds: u32,
) -> Result<()> {
    let width = 10;
    for (name, _) in node_urls.iter() {
        print!("{name:>width$} | ");
    }
    println!();

    let mut rounds = 0u32;
    loop {
        let results = rpc::fetch_latest_heights(node_urls).await;
        let heights = results.iter().map(|(_, r)| *r.as_ref().unwrap_or(&0));

        let heighest_height = heights
            .clone()
            .max()
            .ok_or_else(|| eyre::eyre!("Failed to get the highest height in {heights:?}"))?;

        for h in heights {
            if h == heighest_height || h == heighest_height - 1 {
                print!("{h:>width$} | ");
            } else {
                // For lagging nodes, print height in red
                print!("{} | ", red(&h.to_string(), width));
            }
        }
        println!();

        rounds += 1;
        if max_rounds > 0 && rounds >= max_rounds {
            break;
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

pub(crate) async fn loop_print_mempool(node_urls: &[(String, Url)]) -> Result<()> {
    for (name, _) in node_urls.iter() {
        print!("{name:>11} | ");
    }
    println!();

    loop {
        for (_, mempool_result) in rpc::fetch_mempool_status(node_urls).await {
            if mempool_result.0 <= 0 {
                print!("{:>5}", mempool_result.0);
            } else {
                print!("{}", blue(&mempool_result.0.to_string(), 5));
            }
            print!(",");
            if mempool_result.1 <= 0 {
                print!("{:>5}", mempool_result.1);
            } else {
                print!("{}", red(&mempool_result.1.to_string(), 5));
            }
            print!(" | ");
        }
        println!();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Fetch and print detailed peer information for all nodes
pub(crate) async fn print_peers_info(nodes: &NodesMetadata, all: bool) -> Result<()> {
    let node_urls = nodes.all_execution_urls();
    let peers_info = rpc::fetch_peers_info(&node_urls).await;

    // Map from private EL IP address to (node name, subnet name).
    // Bridge nodes have multiple IPs (one per subnet), so we insert one entry per IP.
    let mut private_ip_to_node_map: HashMap<String, (String, String)> = HashMap::new();
    for (node_name, node_metadata) in nodes.nodes.iter() {
        for (subnet_name, ip) in node_metadata.execution.subnet_ip_map() {
            private_ip_to_node_map.insert(ip.clone(), (node_name.clone(), subnet_name.clone()));
        }
    }

    for (name, peers) in peers_info {
        println!("* {name} ({} peers)", peers.len());
        if peers.is_empty() {
            println!("  (no peers connected)");
        }
        for peer in peers {
            // Extract the IP address from the enode URL (enode://<hash>@<host>:<port>)
            let enode_host = peer.enode.split("@").nth(1).expect("enode host not found");
            let enode_ip = enode_host.split(":").next().expect("enode IP not found");

            // Map the enode IP to the node name and subnet
            let (peer_name, subnet) = private_ip_to_node_map
                .get(enode_ip)
                .map(|(name, subnet)| (name.as_str(), subnet.as_str()))
                .unwrap_or(("unknown", "unknown"));
            println!(
                "  - {peer_name}: local={}, remote={}, subnet={subnet}, inbound={}, trusted={}, static={}",
                peer.network.local_address,
                peer.network.remote_address,
                peer.network.inbound,
                peer.network.trusted,
                peer.network.static_node,
            );
            if all {
                println!("    enode: {}", peer.enode);
                println!("    name: {}", peer.name);
                println!("    caps: {}", peer.caps.join(", "));
                let eth_version = match &peer.protocols.eth {
                    Some(EthPeerInfo::Info(info)) => format!("{}", info.version),
                    Some(EthPeerInfo::Handshake) => "handshake".to_string(),
                    None => "n/a".to_string(),
                };
                let snap_version = match &peer.protocols.snap {
                    Some(SnapPeerInfo::Info(info)) => format!("{}", info.version),
                    Some(SnapPeerInfo::Handshake) => "handshake".to_string(),
                    None => "n/a".to_string(),
                };
                println!("    protocols: eth={eth_version}, snap={snap_version}");
            }
        }
        println!();
    }
    Ok(())
}

/// Format a `Result<String>` into a fixed-width table cell, coloring errors red.
///
/// Padding is applied to the visible text *before* wrapping with ANSI codes so
/// that the caller can emit the returned string with `{}` (no width specifier)
/// and columns stay aligned regardless of whether the value is an error.
fn format_result_cell(result: Result<String, Report>, width: usize, right_align: bool) -> String {
    match result {
        Ok(val) => {
            if right_align {
                format!("{val:>width$}")
            } else {
                format!("{val:<width$}")
            }
        }
        Err(e) => {
            let text = error_to_short_string(e);
            let padded = if right_align {
                format!("{text:>width$}")
            } else {
                format!("{text:<width$}")
            };
            red(&padded, width)
        }
    }
}

fn red(s: &str, width: usize) -> String {
    format!("\x1b[31m{s:>width$}\x1b[0m")
}

fn blue(s: &str, width: usize) -> String {
    format!("\x1b[34m{s:>width$}\x1b[0m")
}

/// Print table statistics for a Malachite CL store.db (redb database).
pub(crate) fn print_store_info(store_path: &Path) -> Result<()> {
    let info = arc_checks::collect_store_info(store_path)?;
    print!("{info}");
    Ok(())
}

/// Compact a known error message to a short string to make the output more readable
fn error_to_short_string(error: Report) -> String {
    let err = error.root_cause().to_string().replace("\n", "  ");
    match err.as_str() {
        "operation timed out" => "timeout".to_string(),
        "Connection reset by peer (os error 54)" => "conn reset".to_string(),
        "Connection refused (os error 61)" => "conn refused".to_string(),
        "connection refused" => "conn refused".to_string(),
        "connection reset by peer" => "conn reset".to_string(),
        "connection timed out" => "conn timed out".to_string(),
        "connection timeout" => "conn timeout".to_string(),
        "connection reset" => "conn reset".to_string(),
        "connection closed before message completed" => "conn closed".to_string(),
        "connection was not ready" => "c. not ready".to_string(),
        _ => err,
    }
}
