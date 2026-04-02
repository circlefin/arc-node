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

use tracing::debug;

use super::{
    in_parallel, quake_test, CheckResult, RpcClientFactory, TestOutcome, TestParams, TestResult,
};
use crate::testnet::Testnet;

/// Test that all nodes have at least one peer
/// DISABLED: Known issue with EL peer connectivity in quake
#[quake_test(group = "net", name = "el_peer_counts", disabled = true)]
fn el_peer_counts_test<'a>(
    testnet: &'a Testnet,
    factory: &'a RpcClientFactory,
    _params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        debug!("Testing peer counts...");

        let node_urls = testnet.nodes_metadata.all_execution_urls();
        let results = in_parallel(&node_urls, factory, |client| async move {
            client.get_peers().await
        })
        .await;

        let mut outcome = TestOutcome::new();

        for (name, url, result) in results {
            match result {
                Ok(peers) => {
                    let peer_count = peers.len();
                    if peer_count > 0 {
                        outcome.add_check(CheckResult::success(
                            name,
                            format!("{} ({} peers)", url, peer_count),
                        ));
                    } else {
                        outcome.add_check(CheckResult::failure(
                            name,
                            format!("{} - No peers connected", url),
                        ));
                    }
                }
                Err(e) => {
                    outcome.add_check(CheckResult::failure(
                        name,
                        format!("{} - Error: {}", url, e),
                    ));
                }
            }
        }

        outcome
            .auto_summary("All nodes have peers", "{} node(s) have no peers")
            .into_result()
    })
}

/// Test that persistent peers defined in the manifest are actually connected
#[quake_test(group = "net", name = "cl_persistent_peers")]
fn cl_persistent_peers_test<'a>(
    testnet: &'a Testnet,
    factory: &'a RpcClientFactory,
    _params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        debug!("Testing persistent peer connections...");

        let mut outcome = TestOutcome::new();

        for (node_name, node_config) in testnet.manifest.nodes.iter() {
            if let Some(persistent_peer_names) = &node_config.cl_persistent_peers {
                if persistent_peer_names.is_empty() {
                    continue;
                }

                // Get the node metadata
                let node_metadata = match testnet.nodes_metadata.get(node_name) {
                    Some(metadata) => metadata,
                    None => {
                        outcome.add_check(CheckResult::failure(
                            node_name,
                            "Node metadata not found".to_string(),
                        ));
                        continue;
                    }
                };

                // Get the peers connected to this node
                let client = factory.create(node_metadata.execution.http_url.clone());

                let peers = match client.get_peers().await {
                    Ok(peers) => peers,
                    Err(e) => {
                        outcome.add_check(CheckResult::failure(
                            node_name,
                            format!("Error fetching peers: {}", e),
                        ));
                        continue;
                    }
                };

                // Check if all persistent peers are connected
                let missing_peers: Vec<_> = persistent_peer_names
                    .iter()
                    .filter(|persistent_peer_name| {
                        // Check if this peer name appears in any connected peer
                        !peers.iter().any(|peer| {
                            // Match by checking if the persistent peer name is in the enode
                            testnet
                                .nodes_metadata
                                .get(persistent_peer_name)
                                .map(|peer_meta| {
                                    if let Ok(url) = reqwest::Url::parse(&peer.enode) {
                                        if let Some(host) = url.host_str() {
                                            // Check if the host is in the private IPs of the peer
                                            let ips = peer_meta.execution.private_ip_addresses();
                                            return ips.contains(&host.to_string());
                                        }
                                    }
                                    false
                                })
                                .unwrap_or(false)
                        })
                    })
                    .cloned()
                    .collect();

                if missing_peers.is_empty() {
                    outcome.add_check(CheckResult::success(
                        node_name,
                        format!(
                            "All {} persistent peers connected",
                            persistent_peer_names.len()
                        ),
                    ));
                } else {
                    outcome.add_check(CheckResult::failure(
                        node_name,
                        format!("Missing persistent peers: {}", missing_peers.join(", ")),
                    ));
                }
            }
        }

        if outcome.checks.is_empty() {
            println!("⚠ No nodes with persistent peers defined in manifest");
            return Ok(());
        }

        outcome
            .auto_summary(
                "All persistent peer connections verified",
                "{} node(s) have disconnected persistent peers",
            )
            .into_result()
    })
}
