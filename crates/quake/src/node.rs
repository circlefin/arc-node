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

use indexmap::IndexMap;
use serde::Serialize;
use std::ops::{Deref, DerefMut};
use url::Url;

use crate::infra::{NodeInfraData, RPC_PROXY_SSM_PORT};
use crate::manifest::RemoteKeyId;

const APP_CONSENSUS_BASE_PORT: usize = 27000;
const APP_METRICS_BASE_PORT: usize = 29000;
const APP_PPROF_BASE_PORT: usize = 6060;
const APP_RPC_BASE_PORT: usize = 31000;

pub(crate) const RETH_HTTP_BASE_PORT: usize = 8545;
pub(crate) const RETH_WS_BASE_PORT: usize = 8546;
const RETH_AUTHRPC_BASE_PORT: usize = 8551;
const RETH_METRICS_BASE_PORT: usize = 9001;
const RETH_PPROF_BASE_PORT: usize = 6161;

/// Suffix of container names to identify CL, EL, or upgraded containers
pub(crate) const CONSENSUS_SUFFIX: &str = "cl";
pub(crate) const EXECUTION_SUFFIX: &str = "el";
pub(crate) const UPGRADED_SUFFIX: &str = "u";

// e.g., "validator1", "validator-green", "full-blue", "full3", ...
pub(crate) type NodeName = String;
// e.g., "validator1_cl", "validator-green_el", "validator-blue_el_u", ...
pub(crate) type ContainerName = String;
// As defined in the manifest file, e.g., "default", "trusted", ...
pub(crate) type SubnetName = String;
// e.g. "172.16.0.1"
pub(crate) type IpAddress = String;
// e.g. "172.16.0.0/16"
pub(crate) type CidrBlock = String;

#[derive(Serialize, Clone, Debug)]
pub(crate) enum ContainerKind {
    Consensus,
    Execution,
}

impl ContainerKind {
    fn suffix(&self) -> &str {
        match self {
            ContainerKind::Consensus => CONSENSUS_SUFFIX,
            ContainerKind::Execution => EXECUTION_SUFFIX,
        }
    }

    /// Third octet in the private IP address of the container
    fn index(&self) -> usize {
        match self {
            ContainerKind::Consensus => 1,
            ContainerKind::Execution => 2,
        }
    }
}

/// A map of subnet names to the IP address of the container in the subnet
#[derive(Serialize, Clone, Debug)]
pub(crate) struct SubnetIps(IndexMap<SubnetName, IpAddress>);

impl SubnetIps {
    /// Create a new SubnetIps from the given subnet names and indexes.
    /// An IP address is built based on the subnet index, the container index, and the node index.
    pub fn from(
        kind: ContainerKind,
        subnet_indexes: &[(SubnetName, usize)],
        node_index: usize,
    ) -> Self {
        let subnets = subnet_indexes
            .iter()
            .map(|(subnet_name, subnet_index)| {
                let ip = Container::build_ip_address(*subnet_index, kind.index(), node_index);
                (subnet_name.clone(), ip)
            })
            .collect::<IndexMap<_, _>>();
        Self(subnets)
    }
}

impl std::fmt::Display for SubnetIps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ips_str = self
            .0
            .iter()
            .map(|(subnet, ip)| format!("{subnet}: {ip}"))
            .collect::<Vec<_>>();
        write!(f, "{}", ips_str.join(", "))
    }
}

/// A generic container
#[derive(Serialize, Clone, Debug)]
pub(crate) struct Container {
    /// Name of the container when the testnet starts
    pub name: ContainerName,

    /// Name of the container after the node was upgraded
    pub name_upgraded: ContainerName,

    /// Name of the node that the container belongs to
    pub node_name: NodeName,

    /// Kind of container, either a Consensus Layer container or an Execution Layer container
    pub kind: ContainerKind,

    /// Subnets the container is connected to and their private IP addresses
    pub subnet_ips: SubnetIps,

    /// Whether the container was upgraded
    pub upgraded: bool,
}

impl Container {
    pub fn new(node: &NodeName, kind: ContainerKind, subnet_ips: &SubnetIps) -> Self {
        let name = format!("{node}_{}", kind.suffix());
        let name_upgraded = format!("{name}_{UPGRADED_SUFFIX}");
        Self {
            name,
            name_upgraded,
            node_name: node.clone(),
            kind,
            subnet_ips: subnet_ips.clone(),
            upgraded: false,
        }
    }

    /// Name of the container according to its kind and whether it was upgraded
    pub fn name(&self) -> &ContainerName {
        if self.upgraded {
            &self.name_upgraded
        } else {
            &self.name
        }
    }

    /// Build a private IP address from the given subnet, container, and node indexes (for local mode)
    ///
    /// Format: "172.<subnet>.<container>.<node>" where:
    /// - subnet index starts at 21
    /// - container index is 1 for CL and 2 for EL
    /// - node index starts at 0
    fn build_ip_address(subnet_index: usize, container_index: usize, node_index: usize) -> String {
        assert!((21..=255).contains(&subnet_index));
        assert!(container_index == 1 || container_index == 2);
        assert!(node_index <= 255);
        format!("172.{subnet_index}.{container_index}.{node_index}")
    }

    /// Set the container as upgraded
    pub fn upgrade(&mut self) {
        self.upgraded = true;
    }

    /// Get the private IP addresses of all subnets the container is connected to
    pub fn private_ip_addresses(&self) -> Vec<IpAddress> {
        self.subnet_ips.0.values().cloned().collect()
    }

    /// Get the private IP address for the given subnet
    pub fn private_ip_address_for(&self, subnet: &str) -> Option<IpAddress> {
        self.subnet_ips.0.get(subnet).cloned()
    }

    /// Get the private IP addresses for the given subnets only
    pub fn private_ip_addresses_for(&self, subnets: &[SubnetName]) -> Vec<IpAddress> {
        self.subnet_ips
            .0
            .iter()
            .filter(|(subnet, _)| subnets.contains(subnet))
            .map(|(_, ip)| ip.clone())
            .collect()
    }

    /// The first subnet IP in map order (primary ENI in remote mode).
    ///
    /// **Ordering guarantee:** `subnet_ips` is an `IndexMap` whose insertion
    /// order matches the Terraform-assigned primary ENI IP first. The ordering
    /// is preserved through: Terraform `network_ips` merge → `infra-data.json`
    /// key order → `serde_json` `IndexMap` deserialization.
    pub fn first_private_ip(&self) -> &IpAddress {
        self.subnet_ips
            .0
            .values()
            .next()
            .expect("Container should have at least one subnet IP")
    }

    /// Get the map of subnet names to the IP address of the container in the subnet
    pub fn subnet_ip_map(&self) -> &IndexMap<SubnetName, IpAddress> {
        &self.subnet_ips.0
    }
}

/// Consensus Layer (Malachite app) container metadata
#[derive(Serialize, Clone, Debug)]
pub(crate) struct ConsensusContainer {
    inner: Container,

    /// Exposed ports
    pub consensus_port: usize,
    pub metrics_port: usize,
    pub pprof_port: usize,
    pub rpc_port: usize,

    /// URL of the consensus layer RPC endpoint, accessible from the host
    pub rpc_url: Url,

    /// URL of the consensus layer metrics endpoint, accessible from the host.
    /// Local: direct port on 127.0.0.1. Remote: via RPC proxy on CC.
    pub metrics_url: Url,

    /// CLI flags for starting the consensus layer
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cli_flags: Vec<String>,
    /// CLI flags for starting the upgraded consensus layer
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cli_flags_upgraded: Vec<String>,
}

impl ConsensusContainer {
    /// Create a new ConsensusContainer with local network IPs (local mode)
    pub fn new_local(
        node: &NodeName,
        subnet_indexes: &[(SubnetName, usize)],
        node_index: usize,
        port_offset: usize,
    ) -> Self {
        let subnets = SubnetIps::from(ContainerKind::Consensus, subnet_indexes, node_index);
        let rpc_port = APP_RPC_BASE_PORT + port_offset;
        let metrics_port = APP_METRICS_BASE_PORT + port_offset;
        let rpc_url =
            Url::parse(&format!("http://127.0.0.1:{rpc_port}")).expect("Failed to parse RPC URL");
        let metrics_url = Url::parse(&format!("http://127.0.0.1:{metrics_port}/metrics"))
            .expect("Failed to parse metrics URL");
        Self {
            inner: Container::new(node, ContainerKind::Consensus, &subnets),
            consensus_port: APP_CONSENSUS_BASE_PORT + port_offset,
            metrics_port,
            pprof_port: APP_PPROF_BASE_PORT + port_offset,
            rpc_port,
            rpc_url,
            metrics_url,
            cli_flags: Vec::new(),
            cli_flags_upgraded: Vec::new(),
        }
    }

    /// Create a new ConsensusContainer with explicit network IPs (remote mode)
    pub fn new_remote(node: &NodeName, subnets: &SubnetIps) -> Self {
        let rpc_url = Url::parse(&format!("http://127.0.0.1:{RPC_PROXY_SSM_PORT}/{node}/cl"))
            .expect("Failed to parse RPC URL");
        let metrics_url = Url::parse(&format!(
            "http://127.0.0.1:{RPC_PROXY_SSM_PORT}/{node}/cl/metrics"
        ))
        .expect("Failed to parse metrics URL");
        Self {
            inner: Container::new(node, ContainerKind::Consensus, subnets),
            consensus_port: APP_CONSENSUS_BASE_PORT,
            metrics_port: APP_METRICS_BASE_PORT,
            pprof_port: APP_PPROF_BASE_PORT,
            rpc_port: APP_RPC_BASE_PORT,
            rpc_url,
            metrics_url,
            cli_flags: Vec::new(),
            cli_flags_upgraded: Vec::new(),
        }
    }

    /// Set CLI flags for both the original and upgraded consensus layers
    pub fn set_cli_flags(&mut self, flags: Vec<String>) {
        self.cli_flags = flags.clone();
        self.cli_flags_upgraded = flags;
    }

    /// Set CLI flags for the upgraded consensus layer
    pub fn set_cli_flags_upgraded(&mut self, flags: Vec<String>) {
        self.cli_flags_upgraded = flags;
    }
}

impl Deref for ConsensusContainer {
    type Target = Container;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for ConsensusContainer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Execution Layer (Reth) container metadata
#[derive(Serialize, Clone, Debug)]
pub(crate) struct ExecutionContainer {
    pub inner: Container,

    /// Execution layer (Reth) CLI flags for this container
    pub(crate) cli_flags: Vec<String>,

    /// Exposed ports
    pub http_port: usize,
    pub ws_port: usize,
    pub authrpc_port: usize,
    pub metrics_port: usize,
    pub pprof_port: usize,

    /// URLs of the container's endpoints
    pub http_url: Url,
    pub ws_url: Url,
}

impl ExecutionContainer {
    /// Create a new ExecutionContainer with local network IPs (local mode)
    pub fn new_local(
        node: &NodeName,
        subnet_indexes: &[(SubnetName, usize)],
        node_index: usize,
        http_url: &str,
        ws_url: &str,
        cli_flags: Vec<String>,
    ) -> Self {
        let subnets = SubnetIps::from(ContainerKind::Execution, subnet_indexes, node_index);
        let port_offset = node_index * 100;
        Self {
            inner: Container::new(node, ContainerKind::Execution, &subnets),
            http_port: RETH_HTTP_BASE_PORT + port_offset,
            ws_port: RETH_WS_BASE_PORT + port_offset,
            authrpc_port: RETH_AUTHRPC_BASE_PORT + port_offset,
            metrics_port: RETH_METRICS_BASE_PORT + port_offset,
            pprof_port: RETH_PPROF_BASE_PORT + port_offset,
            http_url: Url::parse(http_url).expect("Failed to parse HTTP URL"),
            ws_url: Url::parse(ws_url).expect("Failed to parse WS URL"),
            cli_flags,
        }
    }

    /// Create a new ExecutionContainer with explicit network IPs (remote mode)
    pub fn new_remote(
        node: &NodeName,
        subnets: &SubnetIps,
        http_url: &str,
        ws_url: &str,
        cli_flags: Vec<String>,
    ) -> Self {
        Self {
            inner: Container::new(node, ContainerKind::Execution, subnets),
            http_port: RETH_HTTP_BASE_PORT,
            ws_port: RETH_WS_BASE_PORT,
            authrpc_port: RETH_AUTHRPC_BASE_PORT,
            metrics_port: RETH_METRICS_BASE_PORT,
            pprof_port: RETH_PPROF_BASE_PORT,
            http_url: Url::parse(http_url).expect("Failed to parse HTTP URL"),
            ws_url: Url::parse(ws_url).expect("Failed to parse WS URL"),
            cli_flags,
        }
    }
}

impl Deref for ExecutionContainer {
    type Target = Container;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for ExecutionContainer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Data about the node and its containers
#[derive(Serialize, Clone, Debug)]
pub(crate) struct NodeMetadata {
    /// Node name
    pub name: NodeName,

    /// Public IP address, common to all containers
    pub public_ip: IpAddress,

    /// Consensus layer (Malachite) container data
    pub consensus: ConsensusContainer,

    /// Execution layer (Reth) container data
    pub execution: ExecutionContainer,

    /// Optional remote signer key ID
    pub remote_signer: Option<RemoteKeyId>,

    /// Whether this node runs in follow mode
    pub follow: bool,

    /// Remote EL HTTP endpoints for follow mode (resolved from node names)
    pub follow_endpoints: Vec<String>,

    /// Whether consensus is enabled for this node (default true)
    /// When false, the node only syncs and doesn't participate in consensus
    pub consensus_enabled: bool,
}

impl NodeMetadata {
    #[allow(clippy::too_many_arguments)]
    pub fn new_local(
        name: &NodeName,
        infra_data: &NodeInfraData,
        subnet_indexes: &[(SubnetName, usize)],
        index: usize,
        el_cli_flags: Vec<String>,
        follow: bool,
        follow_endpoints: Vec<String>,
        consensus_enabled: bool,
    ) -> Self {
        // Public endpoints of the EL container
        let http_url = format!("http://127.0.0.1:{}", RETH_HTTP_BASE_PORT + (index * 100));
        let ws_url = format!("ws://127.0.0.1:{}", RETH_WS_BASE_PORT + (index * 100));

        Self {
            name: name.to_string(),
            public_ip: infra_data.public_ip.clone(),
            consensus: ConsensusContainer::new_local(name, subnet_indexes, index, index),
            execution: ExecutionContainer::new_local(
                name,
                subnet_indexes,
                index,
                &http_url,
                &ws_url,
                el_cli_flags,
            ),
            remote_signer: infra_data.remote_signer,
            follow,
            follow_endpoints,
            consensus_enabled,
        }
    }

    pub fn new_remote(
        name: &NodeName,
        infra_data: &NodeInfraData,
        subnet_ips: &IndexMap<SubnetName, IpAddress>,
        el_cli_flags: Vec<String>,
        follow: bool,
        follow_endpoints: Vec<String>,
        consensus_enabled: bool,
    ) -> Self {
        // URL of the EL RPC proxy on CC, which routes requests to nodes by name.
        // The proxy runs on CC and is accessed via SSM tunnel.
        let el_http_url = format!("http://127.0.0.1:{RPC_PROXY_SSM_PORT}/{name}/el");
        // URL of the EL WebSocket proxy on CC, routed through the same SSM tunnel.
        let el_ws_url = format!("ws://127.0.0.1:{RPC_PROXY_SSM_PORT}/{name}/el/ws");

        // Use VPC IPs directly from Terraform. In remote mode, all inter-node
        // traffic goes through the VPC, so these are the IPs that matter for
        // peer discovery, latency emulation, and iptables-based disconnect.
        let subnets = SubnetIps(subnet_ips.clone());

        Self {
            name: name.to_string(),
            public_ip: infra_data.public_ip.clone(),
            consensus: ConsensusContainer::new_remote(name, &subnets),
            execution: ExecutionContainer::new_remote(
                name,
                &subnets,
                &el_http_url,
                &el_ws_url,
                el_cli_flags,
            ),
            remote_signer: infra_data.remote_signer,
            follow,
            follow_endpoints,
            consensus_enabled,
        }
    }

    /// The containers of the node
    pub fn containers(&self) -> Vec<&Container> {
        vec![&self.consensus.inner, &self.execution.inner]
    }

    /// The names of the running CL and EL containers
    pub fn container_names(&self) -> Vec<ContainerName> {
        vec![
            self.consensus.name().to_string(),
            self.execution.name().to_string(),
        ]
    }
}
