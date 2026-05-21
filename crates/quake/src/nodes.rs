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

use crate::infra::{InfraData, InfraType};
use crate::manifest::{self, Subnets};
use crate::node::{Container, ContainerName, IpAddress, NodeMetadata, NodeName, EXECUTION_SUFFIX};
use color_eyre::eyre::{bail, eyre, Context, Result};
use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use url::Url;

use crate::infra::RPC_PROXY_SSM_PORT;

/// Can be either a [`NodeName`] or a [`ContainerName`]
pub(crate) type NodeOrContainerName = String; // Name could contain a wildcard '*'

/// Data about all nodes and their containers
#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct NodesMetadata {
    /// Map of node names to node metadata
    pub nodes: IndexMap<NodeName, NodeMetadata>,
    /// Subnets derived from the manifest
    pub subnets: Subnets,
}

impl NodesMetadata {
    /// Create a new `NodesMetadata` instance from the given `InfraData`,
    /// `manifest_nodes`, and `upgraded_containers`.
    ///
    /// `upgraded_containers` tracks which containers have been upgraded so they
    /// persist across Quake restarts (e.g., `quake stop` followed by `quake start`
    /// restarts the `*_u` versions, not the originals). We use this info to mark
    /// the containers as upgraded and start the correct `*_u` versions.
    pub fn new(
        infra_data: InfraData,
        manifest: &manifest::Manifest,
        upgraded_containers: &BTreeSet<ContainerName>,
    ) -> Result<Self> {
        // Remote mode before provision: infra_data has no nodes yet; return empty so
        // start --remote can create infra (Terraform) and then reload.
        if infra_data.nodes.is_empty() {
            return Ok(Self::default());
        }

        // Build a map from node names to their execution layer internal HTTP URLs.
        // These are container-to-container URLs within Docker network.
        let node_to_el_url: IndexMap<String, String> = infra_data
            .nodes
            .iter()
            .map(|(name, _)| {
                let url = match infra_data.infra_type {
                    // Local: use Docker service name for DNS resolution via the
                    // shared host-access network (same pattern the compose template
                    // uses for --eth-rpc-endpoint).
                    InfraType::Local => {
                        format!("http://{name}_{EXECUTION_SUFFIX}:8545")
                    }
                    // Remote: use private IP with standard port
                    InfraType::Remote => format!("http://127.0.0.1:{RPC_PROXY_SSM_PORT}/{name}/el"),
                };
                (name.clone(), url)
            })
            .collect();

        // Iterate in manifest order — infra_data.nodes may be alphabetically
        // sorted (Terraform's jsonencode) which would misassign CL private keys.
        let mut nodes_map = IndexMap::new();
        for (index, (name, manifest_node)) in manifest.nodes.iter().enumerate() {
            let data = infra_data.nodes.get(name).ok_or_else(|| {
                eyre!(
                    "Node '{name}' is in the manifest but not in infra data. \
                    Infra was likely provisioned with a different scenario. \
                    Re-provision (`quake remote provision`) before re-starting the testnet"
                )
            })?;

            let el_cli_flags = manifest_node.el_cli_flags().unwrap_or_default();

            let follow = manifest_node.follow();

            // Resolve node names in follow_endpoints to actual URLs
            let follow_endpoints: Vec<String> = manifest_node
                .follow_endpoints()
                .iter()
                .filter_map(|endpoint_name| node_to_el_url.get(endpoint_name).cloned())
                .collect();

            let consensus_enabled = manifest_node.cl_config.consensus_enabled();

            let mut node = match infra_data.infra_type {
                InfraType::Local => {
                    let subnet_index_list = manifest.subnets.subnet_indexes_for(name);

                    NodeMetadata::new_local(
                        name,
                        data,
                        &subnet_index_list,
                        index,
                        el_cli_flags,
                        follow,
                        follow_endpoints,
                        consensus_enabled,
                    )
                }
                InfraType::Remote => NodeMetadata::new_remote(
                    name,
                    data,
                    data.subnet_ips(),
                    el_cli_flags,
                    follow,
                    follow_endpoints,
                    consensus_enabled,
                ),
            };

            // Mark containers that have been upgraded
            if upgraded_containers.contains(&node.consensus.name) {
                node.consensus.upgrade();
            }
            if upgraded_containers.contains(&node.execution.name) {
                node.execution.upgrade();
            }

            nodes_map.insert(name.clone(), node);
        }

        Ok(Self {
            nodes: nodes_map,
            subnets: manifest.subnets.clone(),
        })
    }

    pub fn num_nodes(&self) -> usize {
        self.nodes.len()
    }

    pub fn node_names(&self) -> Vec<NodeName> {
        self.nodes.keys().cloned().collect()
    }

    pub fn max_node_name_len(&self) -> usize {
        self.nodes.keys().map(|k| k.len()).max().unwrap_or(12)
    }

    pub fn values(&self) -> Vec<NodeMetadata> {
        self.nodes.values().cloned().collect()
    }

    pub fn filter_values(&self, node_names: &[NodeName]) -> Vec<&NodeMetadata> {
        self.nodes
            .values()
            .filter(|node| node_names.contains(&node.name))
            .collect()
    }

    pub fn get(&self, node: &NodeName) -> Option<&NodeMetadata> {
        self.nodes.get(node)
    }

    pub fn get_consensus_ip_addresses(&self, node: &NodeName) -> Vec<IpAddress> {
        self.nodes
            .get(node)
            .map(|n| n.consensus.private_ip_addresses())
            .unwrap_or_default()
    }

    pub fn get_execution_ip_addresses(&self, node: &NodeName) -> Vec<IpAddress> {
        self.nodes
            .get(node)
            .map(|n| n.execution.private_ip_addresses())
            .unwrap_or_default()
    }

    /// Resolve explicit CL persistent peers to reachable consensus IPs.
    ///
    /// If `node` has no shared subnet with its defined `peers`, return an
    /// error instead of adding an unreachable persistent peer.
    pub fn resolve_cl_persistent_peers_list_ips(
        &self,
        node: &NodeName,
        peers: &[NodeName],
    ) -> Result<Vec<IpAddress>> {
        peers
            .iter()
            .map(|peer| self.resolve_cl_persistent_peer_ips(node, peer))
            .collect::<Result<Vec<_>>>()
            .map(|vecs| vecs.into_iter().flatten().collect())
    }

    /// Resolve default CL persistent peers to reachable consensus IPs.
    ///
    /// If the node has no explicit persistent peer list defined in the manifest,
    /// quake connects it to every other node that shares a subnet with it, using
    /// only the peer's IPs on those shared subnets.
    pub fn default_cl_persistent_peers_list_ips(&self, node: &NodeName) -> Vec<IpAddress> {
        self.nodes
            .keys()
            .filter(|peer| *peer != node)
            .filter_map(|peer| self.default_cl_persistent_peer_ips(node, peer))
            .flatten()
            .collect()
    }

    /// Resolve one explicit persistent peer IP, returning an error if the peer is
    /// not found or shares no subnet with the node.
    fn resolve_cl_persistent_peer_ips(
        &self,
        node: &NodeName,
        peer: &NodeName,
    ) -> Result<Vec<IpAddress>> {
        let Some(peer_meta) = self.nodes.get(peer) else {
            return Err(eyre!(
                "Persistent peer '{peer}' for node '{node}' not found"
            ));
        };

        let shared_subnets = self.subnets.shared_subnets(node, peer);
        if shared_subnets.is_empty() {
            return Err(eyre!(
                "Persistent peer '{peer}' for node '{node}' shares no subnet"
            ));
        }

        Ok(peer_meta
            .consensus
            .private_ip_addresses_for(&shared_subnets))
    }

    /// Resolve one default persistent peer.
    ///
    /// When a node omits `cl_persistent_peers` in the manifest, quake treats every
    /// other manifest node as a candidate persistent peer. Return the candidate's
    /// consensus IPs on shared subnets, or `None` if the candidate is not
    /// reachable from `node`.
    fn default_cl_persistent_peer_ips(
        &self,
        node: &NodeName,
        peer: &NodeName,
    ) -> Option<Vec<IpAddress>> {
        let shared_subnets = self.subnets.shared_subnets(node, peer);
        if shared_subnets.is_empty() {
            return None;
        }

        self.nodes.get(peer).map(|peer_meta| {
            peer_meta
                .consensus
                .private_ip_addresses_for(&shared_subnets)
        })
    }

    /// Find the IP address of `peer`'s EL container on the given subnet.
    ///
    /// Returns `None` if the peer is not in the nodes map or has no IP on that subnet.
    pub fn shared_el_subnet_ip(&self, subnet: &str, peer: &str) -> Option<IpAddress> {
        let peer_meta = self.nodes.get(peer)?;
        peer_meta.execution.private_ip_address_for(subnet)
    }

    pub fn execution_http_url(&self, node: &str) -> Option<Url> {
        self.nodes.get(node).map(|n| n.execution.http_url.clone())
    }

    pub fn execution_ws_url(&self, node: &NodeName) -> Option<Url> {
        self.nodes.get(node).map(|n| n.execution.ws_url.clone())
    }

    pub fn to_execution_http_urls(&self, nodes: &[NodeName]) -> Vec<(NodeName, Url)> {
        nodes
            .iter()
            .map(|name| (name.clone(), self.execution_http_url(name).unwrap()))
            .collect()
    }

    pub fn to_execution_ws_urls(&self, node_names: &[NodeName]) -> Vec<(NodeName, Url)> {
        node_names
            .iter()
            .map(|name| (name.clone(), self.execution_ws_url(name).unwrap()))
            .collect()
    }

    pub fn all_execution_urls(&self) -> Vec<(NodeName, Url)> {
        self.nodes
            .iter()
            .map(|(name, n)| (name.clone(), n.execution.http_url.clone()))
            .collect()
    }

    pub fn all_execution_ws_urls(&self) -> Vec<(NodeName, Url)> {
        self.nodes
            .iter()
            .map(|(name, n)| (name.clone(), n.execution.ws_url.clone()))
            .collect()
    }

    pub fn all_consensus_metrics_urls(&self) -> Vec<(NodeName, Url)> {
        self.nodes
            .iter()
            .map(|(name, n)| (name.clone(), n.consensus.metrics_url.clone()))
            .collect()
    }
    /// The list of consensus layer RPC URLs for nodes with consensus enabled.
    /// Nodes with `consensus_enabled: false` (sync-only followers) are excluded.
    /// In local mode, ports are mapped to 127.0.0.1 with per-node offsets.
    /// In remote mode, URLs use the node's private IP.
    pub fn all_consensus_rpc_urls(&self) -> Vec<(NodeName, Url)> {
        self.nodes
            .iter()
            .filter(|(_, n)| n.consensus_enabled)
            .map(|(name, n)| (name.clone(), n.consensus.rpc_url.clone()))
            .collect()
    }

    /// Serialize node metadata for use on the Control Center.
    ///
    /// The in-memory URLs point at the SSM tunnel (port [`RPC_PROXY_SSM_PORT`])
    /// for developer-side access. The CC is in the same VPC as the nodes, so
    /// we rewrite the URLs to use each node's [`Container::first_private_ip`]
    /// with the standard service ports.
    pub fn serialize_for_cc(&self) -> Result<String> {
        let mut nodes = self.values();
        for node in &mut nodes {
            let el_ip = node.execution.first_private_ip().to_string();
            node.execution.http_url =
                Url::parse(&format!("http://{el_ip}:{}", node.execution.http_port))?;
            node.execution.ws_url =
                Url::parse(&format!("ws://{el_ip}:{}", node.execution.ws_port))?;

            let cl_ip = node.consensus.first_private_ip().to_string();
            node.consensus.rpc_url =
                Url::parse(&format!("http://{cl_ip}:{}", node.consensus.rpc_port))?;
            node.consensus.metrics_url = Url::parse(&format!(
                "http://{cl_ip}:{}/metrics",
                node.consensus.metrics_port
            ))?;
        }
        Ok(serde_json::to_string_pretty(&nodes)?)
    }

    /// The list of all CL and EL container names
    pub fn all_container_names(&self) -> Vec<ContainerName> {
        self.nodes
            .values()
            .flat_map(|n| n.container_names())
            .collect()
    }

    /// Convert a list of container names to a list of containers
    pub fn to_containers(&self, names: &[ContainerName]) -> Vec<&Container> {
        let all_containers = self.nodes.values().flat_map(|n| n.containers());
        all_containers
            .filter(|c| names.contains(c.name()))
            .collect()
    }

    /// Expand a list of node or container names, which can contain * as a wildcard, to a list of container names.
    pub fn expand_to_containers_list(
        &self,
        names: &[NodeOrContainerName],
    ) -> Result<Vec<ContainerName>> {
        let mut all_containers = HashSet::new();
        for name in names {
            let containers = self.expand_to_containers(name)?;
            if containers.is_empty() {
                bail!("No node or container found that matches '{name}'");
            }
            all_containers.extend(containers);
        }

        Ok(all_containers.into_iter().collect())
    }

    /// Expand a name of a node or container, which can contain * as a wildcard, to a list of container names.
    ///
    /// Example: "val*" will expand to "validator0_cl", "validator0_el", "validator1_cl", "validator1_el", etc.
    /// Example: "*_el" will expand to all execution layer containers.
    pub fn expand_to_containers(&self, name: &NodeOrContainerName) -> Result<Vec<ContainerName>> {
        if !name.contains('*') {
            // If the name is a node name, return its containers
            if let Some(node) = self.nodes.get(name) {
                return Ok(node.container_names());
            }
            // If the name is a container name, return it
            if self
                .nodes
                .values()
                .any(|node| node.container_names().contains(name))
            {
                return Ok(vec![name.to_string()]);
            }

            return Ok(vec![]);
        }

        let regex = NodesMetadata::build_regex(name)?;

        // Find matches in the node names and container names
        let mut matches = Vec::new();
        for (name, node) in self.nodes.iter() {
            // Check if node name matches
            if regex.is_match(name) {
                matches.extend(node.container_names());
            } else {
                // Check if container names match
                for container in node.container_names() {
                    if regex.is_match(&container) {
                        matches.push(container);
                    }
                }
            }
        }

        // Remove duplicates while preserving order
        let mut unique_matches = Vec::new();
        for m in matches {
            if !unique_matches.contains(&m) {
                unique_matches.push(m);
            }
        }

        Ok(unique_matches)
    }

    /// Expand a list of node or container names, which can contain * as a wildcard, to a list of simple node names.
    pub fn expand_to_nodes_list(&self, names: &[NodeOrContainerName]) -> Result<Vec<NodeName>> {
        let mut all_nodes = HashSet::new();
        for target in names {
            let node_names = self.expand_to_node_name(target)?;
            if node_names.is_empty() {
                bail!("No node or container found that matches '{target}'");
            }
            all_nodes.extend(node_names);
        }

        Ok(all_nodes.into_iter().collect())
    }

    /// Expand a node or container name, which can contain * as a wildcard, to a list of simple node names.
    ///
    /// Example: "val*" will expand to "validator0", "validator1", etc.
    fn expand_to_node_name(&self, name: &NodeOrContainerName) -> Result<Vec<NodeName>> {
        if !name.contains('*') {
            // If the name is a node name, return it
            if self.nodes.get(name).is_some() {
                return Ok(vec![name.to_string()]);
            }

            return Ok(vec![]);
        }

        let regex = NodesMetadata::build_regex(name)?;

        // Find matches in the node names and container names
        let mut matches = Vec::new();
        for node_name in self.nodes.keys() {
            // Check if node name matches
            if regex.is_match(node_name) {
                matches.push(node_name.to_string());
            }
        }

        // Remove duplicates while preserving order
        let mut unique_matches = Vec::new();
        for m in matches {
            if !unique_matches.contains(&m) {
                unique_matches.push(m);
            }
        }

        Ok(unique_matches)
    }

    fn build_regex(name: &str) -> Result<Regex> {
        // Convert wildcard pattern to regex pattern
        // Escape regex special characters except *
        let escaped = if name.contains('*') {
            name.chars()
                .map(|c| match c {
                    '*' => ".*".to_string(),
                    '.' | '^' | '$' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
                    | '\\' => {
                        format!("\\{c}")
                    }
                    _ => c.to_string(),
                })
                .collect::<String>()
        } else {
            name.to_string()
        };

        // Create regex pattern that matches the entire string
        let pattern = format!("^{escaped}$");
        Regex::new(&pattern).wrap_err_with(|| format!("Failed to build regex pattern for '{name}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::InfraData;
    use crate::manifest::Manifest;
    use std::collections::BTreeSet;

    type NodeSubnets = IndexMap<NodeName, Vec<String>>;

    fn create_test_nodes_metadata(node_subnets: NodeSubnets) -> NodesMetadata {
        let manifest_nodes = node_subnets
            .keys()
            .map(|name| (name.clone(), manifest::Node::default()))
            .collect::<IndexMap<_, _>>();
        let infra_data = InfraData::new_local("testnet".to_string(), &manifest_nodes);
        let testnet_name = Some("testnet".to_string());
        let manifest = Manifest::new(testnet_name, &manifest_nodes, &node_subnets);

        NodesMetadata::new(infra_data, &manifest, &BTreeSet::new()).unwrap()
    }

    /// Build metadata where `source` shares subnet B with `reachable`,
    /// while `unreachable` has no shared subnet with `source`.
    fn metadata_with_reachable_and_unreachable_peers() -> NodesMetadata {
        create_test_nodes_metadata(IndexMap::from([
            ("source".to_string(), vec!["B".to_string()]),
            (
                "reachable".to_string(),
                vec!["A".to_string(), "B".to_string()],
            ),
            ("unreachable".to_string(), vec!["A".to_string()]),
        ]))
    }

    #[test]
    fn resolve_cl_persistent_peers_ips_uses_only_shared_subnets() {
        let metadata = metadata_with_reachable_and_unreachable_peers();

        let reachable_b_ip = metadata
            .get(&"reachable".to_string())
            .unwrap()
            .consensus
            .private_ip_address_for("B")
            .unwrap();
        let reachable_a_ip = metadata
            .get(&"reachable".to_string())
            .unwrap()
            .consensus
            .private_ip_address_for("A")
            .unwrap();
        let source = "source".to_string();
        let peers = ["reachable".to_string()];
        let peer_ips = metadata
            .resolve_cl_persistent_peers_list_ips(&source, &peers)
            .unwrap();

        assert_eq!(peer_ips, vec![reachable_b_ip]);
        assert!(!peer_ips.contains(&reachable_a_ip));
    }

    #[test]
    fn resolve_cl_persistent_peers_ips_errors_without_shared_subnet() {
        let metadata = metadata_with_reachable_and_unreachable_peers();

        let source = "source".to_string();
        let peers = ["unreachable".to_string()];
        let err = metadata
            .resolve_cl_persistent_peers_list_ips(&source, &peers)
            .unwrap_err();

        let error = err.to_string();
        let expected = "Persistent peer 'unreachable' for node 'source' shares no subnet";
        assert!(error.contains(expected));
    }

    #[test]
    fn resolve_cl_persistent_peers_ips_errors_for_unknown_peer() {
        let metadata = metadata_with_reachable_and_unreachable_peers();

        let source = "source".to_string();
        let peers = ["missing".to_string()];
        let err = metadata
            .resolve_cl_persistent_peers_list_ips(&source, &peers)
            .unwrap_err();

        let error = err.to_string();
        assert!(error.contains("Persistent peer 'missing' for node 'source' not found"));
    }

    #[test]
    fn default_cl_persistent_peers_ips_skips_unshared_subnets() {
        let metadata = metadata_with_reachable_and_unreachable_peers();

        let reachable_b_ip = metadata
            .get(&"reachable".to_string())
            .unwrap()
            .consensus
            .private_ip_address_for("B")
            .unwrap();
        let unreachable_a_ip = metadata
            .get(&"unreachable".to_string())
            .unwrap()
            .consensus
            .private_ip_address_for("A")
            .unwrap();
        let source = "source".to_string();
        let unreachable = "unreachable".to_string();
        let peer_ips = metadata.default_cl_persistent_peers_list_ips(&source);

        assert_eq!(peer_ips, vec![reachable_b_ip]);
        assert!(!peer_ips.contains(&unreachable_a_ip));
        assert_eq!(
            metadata.default_cl_persistent_peer_ips(&source, &unreachable),
            None
        );
    }
}
