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

use color_eyre::eyre::{bail, Context, Result};
use indexmap::IndexMap;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use tracing::warn;

use crate::infra;
use crate::latency;
use crate::testnet;
use arc_consensus_types::Config as ClConfigOverride;

use crate::manifest::raw::RawManifest;
use crate::node::NodeName;
use crate::node::SubnetName;

mod flags;
mod generate;
mod raw;
mod subnets;

pub(crate) use generate::generate_manifests;
pub(crate) use raw::is_validator;
pub(crate) use subnets::Subnets;

/// Structure for deserializing arc-remote-signer keys from JSON file.
#[derive(Deserialize)]
struct ArcRemoteSignerKeysFile {
    keys: Vec<String>,
}

/// Predefined public keys for arc-remote-signer service, embedded from JSON file at compile time.
///
/// These public keys correspond to the following KMS keys in the dev AWS account:
/// - dev/arc/remote-signer/1
/// - dev/arc/remote-signer/2
/// - dev/arc/remote-signer/3
static PREDEFINED_ARC_REMOTE_SIGNER_PUBLIC_KEYS: Lazy<Vec<String>> = Lazy::new(|| {
    const JSON_CONTENT: &str = include_str!("../../../tests/helpers/arc-remote-signer-keys.json");
    let parsed: ArcRemoteSignerKeysFile = serde_json::from_str(JSON_CONTENT)
        .expect("Failed to parse embedded arc-remote-signer-keys.json");
    parsed.keys
});

/// Get an arc-remote-signer public key by index (0-based).
fn get_predefined_public_key(index: usize) -> &'static str {
    &PREDEFINED_ARC_REMOTE_SIGNER_PUBLIC_KEYS[index]
}

/// Maximum number of nodes that can use remote signing service.
/// This must match the number of keys in tests/helpers/arc-remote-signer-keys.json.
const MAX_REMOTE_SIGNERS: usize = 3;

/// Default subnet name
pub(crate) const DEFAULT_SUBNET_NAME: &str = "default";

pub(crate) fn default_subnet_singleton() -> Vec<String> {
    vec![DEFAULT_SUBNET_NAME.to_string()]
}

/// Identifier of a predefined signing key for remote signer.
/// Valid values are in the range [1, MAX_REMOTE_SIGNERS].
pub type RemoteKeyId = deranged::RangedUsize<1, MAX_REMOTE_SIGNERS>;

// EL default values passed to cli (overridden by the manifest config)
const EL_DEFAULT_TXPOOL_NOLOCALS: bool = true;
const EL_DEFAULT_BUILDER_DEADLINE: u64 = 1;
const EL_DEFAULT_GPO_MAXPRICE: u64 = 5_000_000_000;
const EL_DEFAULT_HTTP_ENABLE: bool = true;
const EL_DEFAULT_LOG_LEVEL: &str = "debug";
const EL_DEFAULT_RPC_TXFEECAP: u64 = 1000;
const EL_DEFAULT_WS_ENABLE: bool = true;
const EL_DEFAULT_RPC_API: &[&str] = &[
    "admin", "net", "eth", "web3", "debug", "txpool", "trace", "reth",
];
const EL_DEFAULT_ENABLE_ARC_RPC: bool = true;
const EL_DEFAULT_ARC_DENYLIST_ENABLED: bool = true;

fn default_rpc_api() -> Vec<String> {
    EL_DEFAULT_RPC_API.iter().map(|s| s.to_string()).collect()
}

/// Execution layer (Reth) transaction pool configuration overrides.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElTxpoolConfig {
    pub pending_max_count: Option<u64>,
    pub basefee_max_count: Option<u64>,
    pub queued_max_count: Option<u64>,
    pub max_account_slots: Option<u64>,
    pub lifetime: Option<u64>,
    pub max_batch_size: Option<u64>,
    pub nolocals: bool,
}

impl Default for ElTxpoolConfig {
    fn default() -> Self {
        Self {
            pending_max_count: None,
            basefee_max_count: None,
            queued_max_count: None,
            max_account_slots: None,
            lifetime: None,
            max_batch_size: None,
            nolocals: EL_DEFAULT_TXPOOL_NOLOCALS,
        }
    }
}

/// Execution layer (Reth) payload builder configuration overrides.
///
/// Fields mirror reth's `PayloadBuilderArgs` (`reth-node-core`) and correspond
/// to `--builder.*` CLI flags. Durations are expressed as whole seconds.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElBuilderConfig {
    pub interval: Option<u64>,
    pub deadline: u64,
    pub max_tasks: Option<u64>,
}

impl Default for ElBuilderConfig {
    fn default() -> Self {
        Self {
            interval: None,
            deadline: EL_DEFAULT_BUILDER_DEADLINE,
            max_tasks: None,
        }
    }
}

/// Execution layer (Reth) engine configuration overrides.
///
/// Fields mirror reth's `EngineArgs` (`reth-node-core`) and correspond to
/// `--engine.*` CLI flags.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ElEngineConfig {
    pub disable_state_cache: Option<bool>,
    pub cross_block_cache_size: Option<u64>,
    pub persistence_threshold: u64,
    pub memory_block_buffer_target: u64,
}

/// Execution layer (Reth) gas price oracle configuration overrides.
///
/// Fields correspond to `--gpo.*` CLI flags.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElGpoConfig {
    pub maxprice: u64,
}

impl Default for ElGpoConfig {
    fn default() -> Self {
        Self {
            maxprice: EL_DEFAULT_GPO_MAXPRICE,
        }
    }
}

/// Execution layer (Reth) HTTP-RPC server configuration overrides.
///
/// Fields correspond to `--http` and `--http.*` CLI flags.
/// `enable = true` maps to `--http`; `api` maps to `--http.api`.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElHttpConfig {
    pub enable: bool,
    pub api: Vec<String>,
}

impl Default for ElHttpConfig {
    fn default() -> Self {
        Self {
            enable: EL_DEFAULT_HTTP_ENABLE,
            api: default_rpc_api(),
        }
    }
}

/// Execution layer (Reth) log configuration overrides.
///
/// `level` maps to reth's `-v`/`-vv`/... verbosity flags.
/// Valid values: `"error"`, `"warning"`, `"info"`, `"debug"`, `"trace"`.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElLogConfig {
    pub level: String,
}

impl Default for ElLogConfig {
    fn default() -> Self {
        Self {
            level: EL_DEFAULT_LOG_LEVEL.to_string(),
        }
    }
}

/// Execution layer (Reth) RPC server configuration overrides.
///
/// Fields correspond to `--rpc.*` CLI flags.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElRpcConfig {
    pub txfeecap: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forwarder: Option<String>,
}

impl Default for ElRpcConfig {
    fn default() -> Self {
        Self {
            txfeecap: EL_DEFAULT_RPC_TXFEECAP,
            forwarder: None,
        }
    }
}

/// Execution layer (Reth) WebSocket-RPC server configuration overrides.
///
/// Fields correspond to `--ws` and `--ws.*` CLI flags.
/// `enable = true` maps to `--ws`; `api` maps to `--ws.api`.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElWsConfig {
    pub enable: bool,
    pub api: Vec<String>,
}

impl Default for ElWsConfig {
    fn default() -> Self {
        Self {
            enable: EL_DEFAULT_WS_ENABLE,
            api: default_rpc_api(),
        }
    }
}

/// Execution layer denylist configuration overrides.
///
/// Fields correspond to `--arc.denylist.*` CLI flags.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElArcDenylistConfig {
    pub enabled: bool,
}

impl Default for ElArcDenylistConfig {
    fn default() -> Self {
        Self {
            enabled: EL_DEFAULT_ARC_DENYLIST_ENABLED,
        }
    }
}

/// Execution layer Arc-specific payload builder overrides.
///
/// Fields correspond to `--arc.builder.*` CLI flags.
/// Durations are in milliseconds.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ElArcBuilderConfig {
    /// Payload builder loop deadline in milliseconds.
    /// Maps to `--arc.builder.deadline`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<u64>,
    /// Wait for the in-flight payload build instead of racing an empty block.
    /// Maps to `--arc.builder.wait-for-payload`.
    #[serde(rename = "wait-for-payload", skip_serializing_if = "Option::is_none")]
    pub wait_for_payload: Option<bool>,
}

/// Execution layer Arc-specific configuration overrides.
///
/// Groups overrides for `--arc.*` CLI flags.
#[derive(Debug, Default, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElArcConfig {
    pub denylist: ElArcDenylistConfig,
    /// When true, passes `--arc.hide-pending-txs` which enables the
    /// pending-tx subscription filter and pending-block interception
    /// middleware. Set to true on externally-exposed nodes for MEV protection.
    #[serde(default)]
    pub hide_pending_txs: bool,
    pub builder: ElArcBuilderConfig,
}

/// Execution layer (Reth) pruning configuration for an individual data segment.
///
/// Each segment (sender-recovery, transaction-lookup, receipts, etc.) can be
/// pruned fully or kept for a given number of recent blocks (`distance`).
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ElPruneSegmentConfig {
    pub full: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<u64>,
}

/// Pruning preset that maps to the `--full` or `--minimal` CLI flag.
///
/// Use this instead of spelling out every `prune.*` segment when one of the
/// built-in presets is sufficient.  Per-segment `prune.*` overrides can be
/// combined with a preset — reth applies the preset first, then individual
/// segment flags take precedence.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ElPruningPreset {
    Full,
    Minimal,
}

impl std::fmt::Display for ElPruningPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "--full"),
            Self::Minimal => write!(f, "--minimal"),
        }
    }
}

/// Execution layer (Reth) pruning configuration overrides.
///
/// Fields correspond to `--prune.*` CLI flags. Segment names use kebab-case
/// to match reth's CLI format (e.g. `sender-recovery`, `account-history`).
///
/// Set `preset` to `"full"` or `"minimal"` as a shorthand for the built-in
/// pruning profiles.  Per-segment fields can override individual segments.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct ElPruneConfig {
    #[serde(default)]
    pub preset: Option<ElPruningPreset>,
    pub sender_recovery: ElPruneSegmentConfig,
    pub transaction_lookup: ElPruneSegmentConfig,
    pub receipts: ElPruneSegmentConfig,
    pub bodies: ElPruneSegmentConfig,
    pub account_history: ElPruneSegmentConfig,
    pub storage_history: ElPruneSegmentConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_interval: Option<u64>,
}

/// Execution layer (Reth) configuration overrides for a node.
///
/// Groups typed overrides for reth CLI flags by subsystem. The struct
/// serializes to a nested TOML table compatible with the `el.config`
/// manifest syntax. Keys map directly to reth CLI flags after flattening
/// (e.g. `txpool.pending_max_count = 10000` → `--txpool.pending-max-count=10000`).
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ElConfigOverride {
    pub txpool: ElTxpoolConfig,
    pub builder: ElBuilderConfig,
    pub engine: ElEngineConfig,
    pub enable_arc_rpc: bool,
    pub disable_discovery: bool,
    pub gpo: ElGpoConfig,
    pub http: ElHttpConfig,
    pub log: ElLogConfig,
    pub rpc: ElRpcConfig,
    pub ws: ElWsConfig,
    pub trusted_peers: Vec<String>,
    pub trusted_only: bool,
    pub tx_propagation_policy: Option<String>,
    pub arc: ElArcConfig,
    pub prune: ElPruneConfig,
}

impl Default for ElConfigOverride {
    fn default() -> Self {
        Self {
            txpool: ElTxpoolConfig::default(),
            builder: ElBuilderConfig::default(),
            engine: ElEngineConfig::default(),
            enable_arc_rpc: EL_DEFAULT_ENABLE_ARC_RPC,
            disable_discovery: bool::default(),
            gpo: ElGpoConfig::default(),
            http: ElHttpConfig::default(),
            log: ElLogConfig::default(),
            rpc: ElRpcConfig::default(),
            ws: ElWsConfig::default(),
            trusted_peers: Vec::default(),
            trusted_only: bool::default(),
            tx_propagation_policy: Option::default(),
            arc: ElArcConfig::default(),
            prune: ElPruneConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Manifest {
    #[allow(dead_code)]
    pub name: Option<String>,
    #[allow(dead_code)]
    pub description: Option<String>,
    pub latency_emulation: bool,
    pub monitoring_bind_host: Option<String>,
    pub engine_api_connection: Option<EngineApiConnection>,
    /// Subnets derived from the manifest
    pub subnets: Subnets,
    /// Docker images for the consensus and execution layers, and their upgrade versions
    pub images: DockerImages,
    /// Map of node name to node metadata
    pub nodes: IndexMap<String, Node>,
    /// Execution layer initial hardfork name for the network (e.g. "zero3", "zero4", "zero5")
    pub el_init_hardfork: Option<String>,
}

impl Manifest {
    #[cfg(test)]
    pub fn new(
        name: Option<String>,
        nodes: &IndexMap<NodeName, Node>,
        node_subnets: &IndexMap<NodeName, Vec<SubnetName>>,
    ) -> Self {
        Self {
            name,
            latency_emulation: true,
            nodes: nodes.clone(),
            subnets: Subnets::new(node_subnets),
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub fn default_from_subnets(node_subnets: &IndexMap<NodeName, Vec<SubnetName>>) -> Self {
        let node_names = node_subnets.keys().cloned().collect::<HashSet<_>>();
        let nodes: IndexMap<NodeName, Node> = node_names
            .iter()
            .map(|n| (n.to_string(), Node::default()))
            .collect();
        Self::new(None, &nodes, node_subnets)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EngineApiConnection {
    Ipc,
    Rpc,
}

/// GossipSub configuration for a consensus layer node.
#[derive(Debug, Default, Deserialize, Serialize, Clone, PartialEq)]
pub struct ClGossipSubConfig {
    /// Enable explicit peering for persistent peers
    #[serde(default)]
    pub explicit_peering: bool,

    /// Enable mesh peer scoring / prioritization
    #[serde(default)]
    pub mesh_prioritization: bool,

    /// Network load profile: "low", "average", "high".
    /// When None, defaults to "average".
    #[serde(default)]
    pub load: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone, PartialEq)]
pub struct Node {
    /// The type of the node
    pub node_type: NodeType,

    /// Consensus layer configuration
    pub cl_config: ClConfigOverride,

    /// Execution layer (Reth) CLI flags for this node
    pub el_config: ElConfigOverride,

    /// The height to start the node at
    pub start_at: Option<u64>,

    /// Data-center region for latency emulation
    pub region: Option<String>,

    /// Persistent peers for the node
    #[serde(default)]
    pub cl_persistent_peers: Option<Vec<String>>,

    /// Only allow connections to/from persistent peers on the consensus layer
    #[serde(default)]
    pub cl_persistent_peers_only: bool,

    /// GossipSub configuration overrides
    #[serde(default)]
    pub cl_gossipsub: ClGossipSubConfig,

    /// Execution layer (Reth) trusted peers: node names or group names, resolved to enodes for --trusted-peers.
    #[serde(default)]
    pub el_trusted_peers: Option<Vec<String>>,

    /// Use the remote signing service for this node
    /// using the predefined key with the corresponding index.
    #[serde(default)]
    pub remote_signer: Option<RemoteKeyId>,

    /// Enable follow mode (fetch blocks via RPC instead of P2P consensus)
    pub follow: bool,

    /// Node names to fetch blocks from in follow mode
    pub follow_endpoints: Vec<String>,

    /// Voting power for this validator in the genesis validator set.
    /// Only meaningful for validator nodes. When set, all validators must specify it.
    pub cl_voting_power: Option<u64>,

    /// Mark this node as external (operated by a third party).
    /// External validators are expected to be multi-hop in mesh health checks
    /// rather than fully-connected. Also applies to their dedicated sentries.
    pub external: bool,
}

impl Node {
    pub fn with_node_type(mut self, node_type: NodeType) -> Self {
        self.node_type = node_type;
        self
    }

    pub fn with_cl_persistent_peers(mut self, cl_persistent_peers: Vec<String>) -> Self {
        self.cl_persistent_peers = Some(cl_persistent_peers);
        self
    }

    pub fn with_el_trusted_peers(mut self, el_trusted_peers: Vec<String>) -> Self {
        self.el_trusted_peers = Some(el_trusted_peers);
        self
    }

    pub fn follow(&self) -> bool {
        self.follow
    }

    pub fn follow_endpoints(&self) -> &[String] {
        &self.follow_endpoints
    }

    /// Returns the execution layer (Reth) CLI flags for this node, defined in the
    /// manifest file with the `el_config` key.
    /// If not defined, returns an empty vector.
    ///
    /// Pruning preset resolution:
    /// Explicit `el.config.prune.preset` → emitted as `--full` / `--minimal`.
    /// No explicit preset → no preset flag emitted (archive / default).
    ///
    /// Per-segment `prune.*` overrides are always emitted alongside the preset;
    /// reth applies them after the preset so they take precedence.
    pub(crate) fn el_cli_flags(&self) -> Result<Vec<String>> {
        let el_table = toml::Table::try_from(self.el_config.clone())
            .context("Failed to serialize node EL config")?;
        let cli_flags = flags::el_config_to_cli_flags(&el_table);
        let mut cli_flags = flags::filter_reserved_flags(cli_flags);
        cli_flags.retain(|f| !f.starts_with("--prune.preset"));

        if let Some(preset) = self.el_config.prune.preset {
            cli_flags.push(preset.to_string());
        }

        Ok(cli_flags)
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone, Default)]
pub enum NodeType {
    #[default]
    Validator,
    NonValidator,
}

impl Manifest {
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read manifest file: {}", path.display()))?;
        let content = substitute_env_vars(&content)
            .context("Failed to substitute environment variables in manifest")?;
        Self::from_string(&content)
    }

    fn from_string(content: &str) -> Result<Self> {
        let raw: RawManifest = toml::from_str(content).wrap_err("Failed to parse manifest")?;
        let manifest = Manifest::try_from(raw)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn into_file(self, path: &Path) -> Result<()> {
        let raw_manifest = RawManifest::try_from(self)?;
        std::fs::write(
            path,
            toml::to_string(&raw_manifest).context("Failed to serialize manifest")?,
        )?;
        Ok(())
    }

    /// Returns the names of the validators as defined in the manifest.
    /// e.g., in the manifest:
    /// ```toml
    /// [nodes.validator-blue]
    /// [nodes.validator42]
    /// [nodes.full1]
    /// ```
    /// this method will return `["validator-blue", "validator42"]`.
    pub(crate) fn validator_names(&self) -> Vec<NodeName> {
        self.nodes
            .iter()
            .filter(|(_, node)| node.node_type == NodeType::Validator)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Collects explicit voting powers from validators, or `None` if none are set.
    pub(crate) fn validator_voting_powers(&self) -> Option<Vec<u64>> {
        let powers: Vec<u64> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.node_type == NodeType::Validator)
            .filter_map(|(_, node)| node.cl_voting_power)
            .collect();

        if powers.is_empty() {
            return None;
        }

        Some(powers)
    }

    pub fn validate(&self) -> Result<()> {
        if self.nodes.is_empty() {
            bail!("At least one node must be defined");
        }

        // Check starting heights
        for (node_name, node) in self.nodes.iter() {
            if let Some(start_at) = node.start_at {
                if start_at == 0 {
                    bail!("Start height cannot be 0 for node '{node_name}'")
                }
            }
        }

        // Check that node regions are valid
        for (node_name, node) in self.nodes.iter() {
            if let Some(region) = node.region.as_ref() {
                if !latency::Region::is_valid(region) {
                    bail!("Invalid region '{}' for node '{}'", region, node_name)
                }
            }
        }

        // Check that we have enough predefined keys for remote signers.
        let remote_signer_count = self
            .nodes
            .values()
            .filter(|node| node.remote_signer.is_some())
            .count();

        if remote_signer_count > MAX_REMOTE_SIGNERS {
            bail!(
                "At most {MAX_REMOTE_SIGNERS} nodes can use the remote signing service, \
                but {remote_signer_count} nodes have it enabled"
            );
        }

        // Check that remote signer key IDs are unique
        let mut used_key_ids = HashSet::new();
        for (node_name, node) in self.nodes.iter() {
            if let Some(key_id) = node.remote_signer {
                if used_key_ids.contains(&key_id) {
                    bail!("Duplicate remote_signer key ID {key_id} for node '{node_name}'");
                }
                used_key_ids.insert(key_id);
            }
        }

        // Emit warning if full node is configured with remote signer,
        // as this could be a misconfiguration.
        // We do not emit an error here in order to allow for setting up a full node
        // intended to become a validator later with the remote signer.
        for (node_name, node) in self.nodes.iter() {
            if node.remote_signer.is_some() && node.node_type != NodeType::Validator {
                warn!("Node '{node_name}' is a full node and does not require remote signer");
                warn!("This could be a misconfiguration, unless the node is intended to become a validator later");
            }
        }

        // Check cl_voting_power: if any validator specifies it, all validators must
        let total_validators = self
            .nodes
            .values()
            .filter(|n| n.node_type == NodeType::Validator)
            .count();
        let validators_with_power = self
            .nodes
            .values()
            .filter(|n| n.node_type == NodeType::Validator && n.cl_voting_power.is_some())
            .count();
        if validators_with_power > 0 && validators_with_power != total_validators {
            bail!(
                "cl_voting_power must be specified for all validators or none; \
                 {validators_with_power} of {total_validators} validators have it set"
            );
        }
        for (node_name, node) in self.nodes.iter() {
            if node.node_type == NodeType::Validator {
                if let Some(0) = node.cl_voting_power {
                    bail!("cl_voting_power must be greater than 0 for validator '{node_name}'");
                }
            }
        }

        // Check that node names in follow_endpoints exist in the manifest
        for (node_name, node) in self.nodes.iter() {
            if node.follow {
                self.contain_nodes(&node.follow_endpoints)
                    .with_context(|| format!("Node '{node_name}' has invalid follow_endpoints"))?;
            }
        }

        // Check that all persistent peers reference valid node names
        for (node_name, node) in self.nodes.iter() {
            if let Some(peers) = &node.cl_persistent_peers {
                for peer in peers {
                    if !self.nodes.contains_key(peer) {
                        bail!(
                            "Node '{node_name}' has invalid persistent peer '{peer}': \
                            no node with that name exists in the manifest"
                        );
                    }
                }
            }
        }

        // Check that all el_trusted_peers reference valid node names
        for (node_name, node) in self.nodes.iter() {
            if let Some(peers) = &node.el_trusted_peers {
                for peer in peers {
                    if !self.nodes.contains_key(peer) {
                        bail!(
                            "Node '{node_name}' has invalid el_trusted_peers entry '{peer}': \
                            no node with that name exists in the manifest"
                        );
                    }
                }
            }
        }

        // Check that all subnets are connected through bridge nodes.
        self.subnets.validate_topology()?;

        Ok(())
    }

    /// Returns the number of validator nodes in the manifest
    pub fn num_validators(&self) -> usize {
        self.nodes
            .values()
            .filter(|node| node.node_type == NodeType::Validator)
            .count()
    }

    /// Returns a mapping from node index (1-based) to predefined signing key for nodes using remote signer
    /// in the order they appear in the manifest.
    ///
    /// # Panics
    /// Panics if there are more nodes with remote signer enabled than predefined keys.
    /// This should not happen if the manifest has been validated before calling this method.
    pub fn public_key_overrides(&self) -> IndexMap<usize, String> {
        let mut overrides = IndexMap::new();

        for (i, node) in self.nodes.values().enumerate() {
            if let Some(key_id) = node.remote_signer {
                let key_idx = key_id.get() - 1; // Convert to 0-based index
                overrides.insert(i + 1, get_predefined_public_key(key_idx).to_string());
            }
        }

        overrides
    }

    /// Return true iff all given node names exist in the manifest
    pub fn contain_nodes(&self, node_names: &[String]) -> Result<()> {
        for node_name in node_names {
            if !self.nodes.contains_key(node_name) {
                bail!("Node {node_name} not found");
            }
        }
        Ok(())
    }

    /// Nodes that share at least one subnet with the given node
    pub fn filter_nodes_with_shared_subnets(
        &self,
        node: &NodeName,
    ) -> impl Iterator<Item = (&NodeName, &Node)> {
        let node_subnets = self.subnets.subnets_of(node);
        self.nodes.iter().filter(move |(peer, _)| {
            let peer_subnets = self.subnets.subnets_of(peer);
            **peer != *node && peer_subnets.iter().any(|s| node_subnets.contains(s))
        })
    }

    /// Build a map from each subnet to a list of nodes in that subnet.
    ///
    /// This is used by Terraform in remote mode to create separate subnets for
    /// each logical network and place nodes in the appropriate subnets.
    pub fn build_network_topology(&self) -> IndexMap<SubnetName, Vec<NodeName>> {
        let mut topology: IndexMap<SubnetName, Vec<NodeName>> = IndexMap::new();
        for (node_name, _) in self.nodes.iter() {
            for subnet in self.subnets.subnets_of(node_name) {
                topology.entry(subnet).or_default().push(node_name.clone());
            }
        }
        topology
    }
}

/// Replaces `${VAR_NAME}` patterns in `content` with values from the environment
/// (including `.env` files discovered by `dotenvy`).
///
/// Only `[A-Za-z_][A-Za-z0-9_]*` names are valid inside `${…}`. Any `${` sequence
/// that doesn't match (e.g. `${}`, `${1FOO}`, `${UN-CLOSED}`) is treated as a
/// malformed placeholder and returns an error.
fn substitute_env_vars(content: &str) -> Result<String> {
    use regex::Regex;
    use std::sync::OnceLock;

    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap());

    static MALFORMED_RE: OnceLock<Regex> = OnceLock::new();
    let malformed_re = MALFORMED_RE
        .get_or_init(|| Regex::new(r"\$\{[^}]*\}|\$\{[^}]*$").expect("static regex pattern"));

    // Detect malformed placeholders: any `${…}` that the valid regex doesn't match.
    for m in malformed_re.find_iter(content) {
        if !re.is_match(m.as_str()) {
            bail!(
                "Malformed placeholder '{}' — variable names must match \
                 [A-Za-z_][A-Za-z0-9_]* (e.g. ${{IMAGE_REGISTRY_URL}})",
                m.as_str()
            );
        }
    }

    let mut err = None;
    let result = re.replace_all(content, |caps: &regex::Captures<'_>| {
        if err.is_some() {
            return String::new();
        }
        let full_match = &caps[0];
        let var_name = &caps[1];
        match dotenvy::var(var_name) {
            Ok(value) => value,
            Err(_) => {
                err = Some(color_eyre::eyre::eyre!(
                    "Environment variable '{var_name}' not found (referenced as {full_match})"
                ));
                String::new()
            }
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(result.into_owned()),
    }
}

/// Docker image overrides from the manifest file. All fields are optional.
/// Absent fields use infrastructure-specific defaults when resolved via
/// [`to_local`](Self::to_local) or [`to_remote`](Self::to_remote).
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Default)]
pub(crate) struct DockerImages {
    /// Consensus layer image: either unqualified (`arc_consensus:latest`) for local builds
    /// or a full registry path (`ghcr.io/org/repo/arc-consensus:tag`) for remote pulls.
    pub cl: Option<String>,
    /// Execution layer image: same format as `cl`.
    pub el: Option<String>,
    /// Consensus layer upgrade image: used by `quake upgrade` to replace running CL containers.
    pub cl_upgrade: Option<String>,
    /// Execution layer upgrade image: used by `quake upgrade` to replace running EL containers.
    pub el_upgrade: Option<String>,
}

impl DockerImages {
    /// Resolve an explicit image or substitute env vars in a default pattern.
    fn resolve_image(explicit: &Option<String>, default: &str) -> Result<String> {
        match explicit {
            Some(img) => Ok(img.clone()),
            None => substitute_env_vars(default),
        }
    }

    /// Resolve images for local infrastructure, filling in defaults for absent fields.
    pub fn to_local(&self) -> Result<testnet::DockerImages> {
        Ok(testnet::DockerImages {
            cl: Self::resolve_image(&self.cl, infra::local::DEFAULT_IMAGE_CL)?,
            el: Self::resolve_image(&self.el, infra::local::DEFAULT_IMAGE_EL)?,
            cl_upgrade: self.cl_upgrade.clone(),
            el_upgrade: self.el_upgrade.clone(),
        })
    }

    /// Resolve images for remote infrastructure, filling in defaults for absent fields.
    /// Returns an error if any resolved image does not use the `ghcr.io/` registry.
    pub fn to_remote(&self) -> Result<testnet::DockerImages> {
        if self.cl_upgrade.is_some() || self.el_upgrade.is_some() {
            bail!("Upgrading containers is currently not supported in remote infrastructure");
        }

        let images = testnet::DockerImages {
            cl: Self::resolve_image(&self.cl, infra::remote::DEFAULT_IMAGE_CL)?,
            el: Self::resolve_image(&self.el, infra::remote::DEFAULT_IMAGE_EL)?,
            cl_upgrade: None,
            el_upgrade: None,
        };

        for img in [&images.cl, &images.el] {
            if !img.starts_with("ghcr.io/") {
                bail!("Image {img} must start with 'ghcr.io/' for remote mode");
            }
        }

        Ok(images)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deranged::RangedUsize;
    use malachitebft_config::LogLevel;
    use std::env;

    // Check number of nodes, names, types, and order of declaration in the manifest
    fn validate_nodes(
        nodes: &IndexMap<String, Node>,
        expected_node_names: Vec<&'static str>,
        expected_types: Vec<NodeType>,
    ) {
        assert_eq!(nodes.len(), expected_node_names.len());
        nodes.iter().enumerate().for_each(|(i, (name, node))| {
            assert_eq!(name, expected_node_names[i]);
            assert_eq!(node.node_type, expected_types[i]);
        });
    }

    #[test]
    fn test_load_no_nodes() {
        let str = r#"
        name = "testnet"
        description = "test"
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err(), "Expected error because there are no nodes");
    }

    #[test]
    fn test_load_simple_manifest() {
        let str = r#"
        name = "testnet"
        description = "test"
        [nodes.validator1]
        [nodes.validator2]
        [nodes.full1]
        [nodes.full2]
        [nodes.validator3]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // Check first-level keys
        assert_eq!(manifest.name, Some("testnet".to_string()));
        assert_eq!(manifest.description, Some("test".to_string()));
        assert!(manifest.engine_api_connection.is_none());

        let expected_node_names = vec!["validator1", "validator2", "full1", "full2", "validator3"];
        let expected_types = vec![
            NodeType::Validator,
            NodeType::Validator,
            NodeType::NonValidator,
            NodeType::NonValidator,
            NodeType::Validator,
        ];
        validate_nodes(&manifest.nodes, expected_node_names, expected_types);

        // Check that latency emulation is enabled by default
        assert!(manifest.latency_emulation);
        assert!(manifest.nodes["validator1"].region.is_none());
        assert!(manifest.nodes["validator2"].region.is_none());
    }

    #[test]
    fn test_load_manifest_with_config() {
        let str = r#"
        name = "testnet"
        description = "test"
        cl.config.logging.log_level = "warn"
        [nodes.validator1]
        cl.config.logging.log_level = "info"
        [nodes.validator2]
        cl.config.consensus.p2p.rpc_max_size = "123kb"
        [nodes.validator3]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // Check top-level keys
        assert_eq!(manifest.name, Some("testnet".to_string()));
        assert_eq!(manifest.description, Some("test".to_string()));

        let expected_node_names = vec!["validator1", "validator2", "validator3"];
        let expected_types = vec![
            NodeType::Validator,
            NodeType::Validator,
            NodeType::Validator,
        ];
        validate_nodes(&manifest.nodes, expected_node_names, expected_types);

        // Check nodes individual config
        assert_eq!(
            manifest.nodes["validator1"].cl_config.logging.log_level,
            LogLevel::Info
        );
        assert_eq!(
            manifest.nodes["validator2"].cl_config.logging.log_level,
            LogLevel::Warn
        );
        assert_eq!(
            manifest.nodes["validator2"]
                .cl_config
                .consensus
                .p2p
                .rpc_max_size,
            bytesize::ByteSize::kb(123),
        );
        assert_eq!(
            manifest.nodes["validator3"].cl_config.logging.log_level,
            LogLevel::Warn
        );
    }

    #[test]
    fn test_load_invalid_global_cl_config() {
        let str = r#"
        cl.config.foo = 1
        [nodes.validator1]
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err(), "Expected global CL config to be invalid");
    }

    #[test]
    fn test_load_invalid_node_config() {
        let str = r#"
        [nodes.validator1]
        cl.config.foo = 1
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err(), "Expected node config to be invalid");
    }

    #[test]
    fn test_load_manifest_without_latency_emulation() {
        let str = r#"
        latency_emulation = false
        [nodes.validator1]
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        assert!(!manifest.latency_emulation);
    }

    #[test]
    fn test_load_manifest_with_regions() {
        let str = r#"
        [nodes.validator1]
        region = "eu-central-1"
        [nodes.validator2]
        [nodes.full1]
        region = "eu-west-2"
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        assert_eq!(
            manifest.nodes["validator1"].region,
            Some("eu-central-1".to_string())
        );
        assert!(manifest.nodes["validator2"].region.is_none());
        assert_eq!(
            manifest.nodes["full1"].region,
            Some("eu-west-2".to_string())
        );
    }

    #[test]
    fn test_load_manifest_with_invalid_region() {
        let str = r#"
        [nodes.validator1]
        region = "InvalidRegion"
        "#;
        let result = Manifest::from_string(str);
        println!("{:?}", result);
        assert!(
            result.is_err(),
            "Expected error because of an invalid region"
        );
    }

    #[test]
    fn test_load_manifest_with_top_level_settings() {
        let str = r#"
        engine_api_connection = "rpc"
        image_cl = "arc_consensus:latest"
        [nodes.validator1]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // Check engine API connection
        assert!(manifest.engine_api_connection.is_some());
        assert!(matches!(
            manifest.engine_api_connection,
            Some(EngineApiConnection::Rpc)
        ));
        assert!(manifest.images.cl.is_some());
        assert_eq!(manifest.images.cl, Some("arc_consensus:latest".to_string()));
    }

    #[test]
    fn test_node_types() {
        let str = r#"
        [nodes.validator0]
        [nodes.non_validator0]
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        assert_eq!(manifest.nodes.len(), 2);
        assert_eq!(manifest.nodes["validator0"].node_type, NodeType::Validator);
        assert_eq!(
            manifest.nodes["non_validator0"].node_type,
            NodeType::NonValidator
        );
    }

    #[test]
    fn test_node_with_empty_config_uses_global() {
        let str = r#"
        [cl.config]
        consensus.enabled = false

        [nodes.validator-0]
        cl.config = {}  # explicitly empty
    "#;
        let result = Manifest::from_string(str).unwrap();
        // Verify the node inherited global config
        assert!(!result.nodes["validator-0"].cl_config.consensus.enabled);
    }

    #[test]
    fn test_validate_max_remote_signers() {
        let str = r#"
        [nodes.validator1]
        remote_signer = 1
        [nodes.validator2]
        remote_signer = 2
        [nodes.validator3]
        remote_signer = 3
        [nodes.validator4]
        remote_signer = 4
        "#;
        let result = Manifest::from_string(str);

        assert!(result.is_err());
        let err = format!("{:#}", result.unwrap_err());
        // The error message comes from deranged crate when value is out of range
        assert!(err.contains("expected an integer in the valid range"));
    }

    #[test]
    fn test_validate_duplicate_key_ids() {
        let str = r#"
        [nodes.validator1]
        remote_signer = 1
        [nodes.validator2]
        remote_signer = 2
        [nodes.validator3]
        remote_signer = 2
        "#;
        let result = Manifest::from_string(str);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Duplicate remote_signer key ID 2 for node 'validator3'"));
    }

    #[test]
    fn test_validate_invalid_persistent_peer() {
        let str = r#"
        [nodes.validator1]
        cl_persistent_peers = ["validator2", "nonexistent"]
        [nodes.validator2]
        "#;
        let result = Manifest::from_string(str);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid persistent peer 'nonexistent'"));
    }

    #[test]
    fn test_validate_invalid_el_trusted_peers() {
        let str = r#"
        [nodes.validator1.el.config]
        trusted_peers = ["validator2", "nonexistent"]
        [nodes.validator2]
        "#;
        let result = Manifest::from_string(str);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid el_trusted_peers entry 'nonexistent'"));
    }

    #[test]
    fn test_validate_valid_cl_persistent_peers() {
        let str = r#"
        [nodes.validator1]
        cl_persistent_peers = ["validator2", "validator3"]
        [nodes.validator2]
        cl_persistent_peers = ["validator1"]
        [nodes.validator3]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        assert_eq!(
            manifest.nodes["validator1"].cl_persistent_peers,
            Some(vec!["validator2".to_string(), "validator3".to_string()])
        );
        assert_eq!(
            manifest.nodes["validator2"].cl_persistent_peers,
            Some(vec!["validator1".to_string()])
        );
        assert_eq!(manifest.nodes["validator3"].cl_persistent_peers, None);
    }

    #[test]
    fn test_validate_exactly_three_remote_signers() {
        let str = r#"
        [nodes.validator1]
        remote_signer = 1
        [nodes.validator2]
        remote_signer = 2
        [nodes.validator3]
        remote_signer = 3
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        let overrides = manifest.public_key_overrides();
        assert_eq!(overrides.len(), 3);
    }

    #[test]
    fn test_public_key_overrides_sparse_nodes() {
        let str = r#"
        [nodes.validator1]
        [nodes.validator2]
        remote_signer = 1
        [nodes.validator3]
        remote_signer = 2
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // Should only have entries for nodes with remote_signer enabled
        let overrides = manifest.public_key_overrides();
        assert_eq!(overrides.len(), 2);
        assert!(overrides.contains_key(&2));
        assert!(overrides.contains_key(&3));
        assert!(!overrides.contains_key(&1));
    }

    #[test]
    fn test_public_key_overrides_correct_key_assignment() {
        let str = r#"
        [nodes.validator1]
        [nodes.validator2]
        [nodes.validator3]
        remote_signer = 1
        [nodes.validator4]
        remote_signer = 2
        [nodes.validator5]
        remote_signer = 3
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        let overrides = manifest.public_key_overrides();
        assert_eq!(overrides.len(), 3);
        assert_eq!(
            overrides.get(&3).unwrap(),
            "0x22faef225605e08abb380a5da398b609b6641cd7d1e4d718cc0fe6ecd8a2a094"
        );
        assert_eq!(
            overrides.get(&4).unwrap(),
            "0x784ad3ae1bbac71ff39311bca0de2517cdfa727857c5a2a5cbf395d9af3af43a"
        );
        assert_eq!(
            overrides.get(&5).unwrap(),
            "0x65957d56ed7cdd8e9f55e2f4cc2905fe337ac7f05d74f2f17280a7489ee24413"
        );
    }

    #[test]
    fn test_validate_full_node_can_use_remote_signer() {
        let str = r#"
            [nodes.full1]
            remote_signer = 1
            "#;
        let manifest = Manifest::from_string(str).unwrap();

        assert_eq!(
            manifest.nodes["full1"].remote_signer,
            Some(RangedUsize::new_static::<1>())
        );
        assert!(manifest.public_key_overrides().contains_key(&1));
    }

    #[test]
    fn test_el_config_inherits_defaults() {
        // Manifest with no EL config should inherit all defaults
        let str = r#"
        [nodes.validator1]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // Check defaults are applied.
        // (just checking a subset of them here).
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--http".to_string()));
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--ws".to_string()));
        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("disable-discovery")));
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--txpool.nolocals".to_string()));
    }

    #[test]
    fn test_el_config_global_overrides_defaults() {
        // Manifest global EL config should override specific defaults
        let str = r#"
        [el.config]
        disable_discovery = false
        txpool.nolocals = false
        engine.persistence_threshold = 10
        rpc.txfeecap = 500

        [nodes.validator1]
        [nodes.validator2]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // Boolean overrides: false means flag is omitted
        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("disable-discovery")));
        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("txpool.nolocals")));
        assert!(!manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("disable-discovery")));
        assert!(!manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("txpool.nolocals")));

        // Value overrides: new values replace defaults
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--engine.persistence-threshold=10".to_string()));
        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--engine.persistence-threshold=0".to_string()));
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--rpc.txfeecap=500".to_string()));
        assert!(manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--engine.persistence-threshold=10".to_string()));
        assert!(!manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--engine.persistence-threshold=0".to_string()));
        assert!(manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--rpc.txfeecap=500".to_string()));

        // Defaults that weren't overridden should still be present
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--http".to_string()));
        assert!(manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--http".to_string()));
    }

    #[test]
    fn test_el_config_per_node_overrides_all() {
        // Per-node EL config should override both defaults and manifest global
        // (for both boolean and value flags)
        let str = r#"
        [el.config]
        disable_discovery = false
        builder.deadline = 10

        [nodes.validator1]
        el.config.engine.persistence_threshold = 1
        el.config.builder.deadline = 5

        [nodes.validator-blue]
        el.config.disable_discovery = true

        [nodes.validator3]
        [nodes.validator4]
        el.config.http.enable = false
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // validator1: inherits global bool override (no disable-discovery),
        // overrides engine.persistence-threshold (default) and builder.deadline (global)
        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("disable-discovery")));
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--engine.persistence-threshold=1".to_string()));
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--builder.deadline=5".to_string()));
        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--builder.deadline=10".to_string()));

        // validator-blue: overrides the global boolean back to true
        assert!(manifest.nodes["validator-blue"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--disable-discovery".to_string()));

        // validator3: inherits global overrides (no disable-discovery, deadline=10)
        assert!(!manifest.nodes["validator3"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("disable-discovery")));
        assert!(manifest.nodes["validator3"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--builder.deadline=10".to_string()));

        // validator4: disables http (overrides default http.enable=true)
        assert!(!manifest.nodes["validator4"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f == "--http"));

        // check that all nodes inherit the defaults
        // checking only txpool.nolocals here as an example.
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--txpool.nolocals".to_string()));
        assert!(manifest.nodes["validator-blue"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--txpool.nolocals".to_string()));
        assert!(manifest.nodes["validator3"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--txpool.nolocals".to_string()));
        assert!(manifest.nodes["validator4"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--txpool.nolocals".to_string()));
    }

    #[test]
    fn test_el_config_per_node_only_overrides_defaults() {
        // Per-node EL config should override defaults directly if there is no
        // global EL config.
        let str = r#"
        [nodes.validator1]
        el.config.engine.persistence_threshold = 1

        [nodes.validator2]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // validator1: overrides engine.persistence-threshold, but inherits
        // other defaults
        // (just checking a subset of the defaults here).
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--engine.persistence-threshold=1".to_string()));
        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--engine.persistence-threshold=0".to_string()));
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--http".to_string()));
        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("disable-discovery")));

        // validator2: inherits all defaults unchanged
        // (just checking a subset of them here).
        assert!(manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--engine.persistence-threshold=0".to_string()));
        assert!(manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--http".to_string()));
    }

    #[test]
    fn test_el_config_array_values() {
        // Array values should be joined with commas
        let str = r#"
        [el.config]
        http.api = ["admin", "debug"]

        [nodes.validator1]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // Array is joined as comma-separated value
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--http.api=admin,debug".to_string()));
        // Default array is overridden
        // the default array is
        // http.api = ["admin", "net", "eth", "web3", "debug", "txpool", "trace", "reth"]
        // so this check confirms that no flag contains both "eth" and "http.api".
        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("eth") && f.contains("http.api")));
    }

    #[test]
    fn test_el_pruning_no_preset_without_explicit_config() {
        let str = r#"
        [nodes.val1]
        [nodes.full1]
        [nodes.sentry]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // Without explicit preset, no preset flag is emitted for any node type.
        for name in &["val1", "full1", "sentry"] {
            let flags = manifest.nodes[*name].el_cli_flags().unwrap();
            assert!(
                !flags.contains(&"--full".to_string()),
                "{name}: unexpected --full"
            );
            assert!(
                !flags.contains(&"--minimal".to_string()),
                "{name}: unexpected --minimal"
            );
        }
    }

    #[test]
    fn test_el_pruning_explicit_preset_full() {
        let str = r#"
        [nodes.sentry]
        el.config.prune.preset = "full"
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        manifest.validate().unwrap();

        let flags = manifest.nodes["sentry"].el_cli_flags().unwrap();
        assert!(
            flags.contains(&"--full".to_string()),
            "explicit preset = full should emit --full, got: {flags:?}",
        );
        assert!(!flags.contains(&"--minimal".to_string()));
        assert!(
            !flags.iter().any(|f| f.contains("prune.preset")),
            "preset field must not leak as a CLI flag, got: {flags:?}",
        );
    }

    #[test]
    fn test_el_pruning_explicit_preset_minimal() {
        let str = r#"
        [nodes.sentry]
        el.config.prune.preset = "minimal"
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        manifest.validate().unwrap();

        let flags = manifest.nodes["sentry"].el_cli_flags().unwrap();
        assert!(
            flags.contains(&"--minimal".to_string()),
            "explicit preset = minimal should emit --minimal, got: {flags:?}",
        );
        assert!(!flags.contains(&"--full".to_string()));
    }

    #[test]
    fn test_el_pruning_preset_with_segment_overrides() {
        let str = r#"
        [nodes.sentry]
        el.config.prune.preset = "full"
        el.config.prune.bodies.distance = 100
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        manifest.validate().unwrap();

        let flags = manifest.nodes["sentry"].el_cli_flags().unwrap();
        assert!(
            flags.contains(&"--full".to_string()),
            "preset should be emitted alongside segment overrides, got: {flags:?}",
        );
        assert!(
            flags.iter().any(|f| f.contains("prune.bodies.distance")),
            "segment override should be emitted, got: {flags:?}",
        );
    }

    #[test]
    fn test_el_pruning_preset_roundtrip() {
        let toml_str = r#"
        el.config.prune.preset = "minimal"
        el.config.prune.bodies.distance = 100

        [nodes.val1]
        [nodes.sentry]
        el.config.prune.preset = "full"
        "#;

        let manifest1 = Manifest::from_string(toml_str).unwrap();
        assert_eq!(
            manifest1.nodes["val1"].el_config.prune.preset,
            Some(ElPruningPreset::Minimal),
        );
        assert_eq!(
            manifest1.nodes["sentry"].el_config.prune.preset,
            Some(ElPruningPreset::Full),
        );

        // Serialize back and re-parse
        let raw = crate::manifest::raw::RawManifest::try_from(manifest1).unwrap();
        let serialized = toml::to_string(&raw).unwrap();
        let manifest2 = Manifest::from_string(&serialized).unwrap();

        assert_eq!(
            manifest2.nodes["val1"].el_config.prune.preset,
            Some(ElPruningPreset::Minimal),
            "preset must survive round-trip for val1",
        );
        assert_eq!(
            manifest2.nodes["sentry"].el_config.prune.preset,
            Some(ElPruningPreset::Full),
            "preset must survive round-trip for sentry",
        );
        assert_eq!(
            manifest2.nodes["val1"].el_config.prune.bodies.distance,
            Some(100),
            "bodies.distance must survive round-trip",
        );
    }

    #[test]
    fn test_arc_builder_deadline() {
        // Global arc.builder.deadline applies to all nodes
        let str = r#"
        el.config.arc.builder.deadline = 300

        [nodes.validator1]
        [nodes.validator2]
        el.config.arc.builder.deadline = 500
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // validator1 inherits global
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--arc.builder.deadline=300".to_string()));

        // validator2 overrides with per-node value
        assert!(manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--arc.builder.deadline=500".to_string()));
        assert!(!manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--arc.builder.deadline=300".to_string()));
    }

    #[test]
    fn test_arc_builder_deadline_omitted_when_unset() {
        let str = r#"
        [nodes.validator1]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("arc.builder.deadline")));
    }

    #[test]
    fn test_rpc_forwarder_set() {
        let str = r#"
        [nodes.validator1]
        el.config.rpc.forwarder = "http://sentry:8545"
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--rpc.forwarder=http://sentry:8545".to_string()));
    }

    #[test]
    fn test_rpc_forwarder_omitted_when_unset() {
        let str = r#"
        [nodes.validator1]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("rpc.forwarder")));
    }

    #[test]
    fn test_arc_builder_wait_for_payload() {
        let str = r#"
        el.config.arc.builder.wait-for-payload = false

        [nodes.validator1]
        [nodes.validator2]
        el.config.arc.builder.wait-for-payload = true
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        // validator1 inherits global (false)
        assert!(manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--arc.builder.wait-for-payload=false".to_string()));

        // validator2 overrides with per-node value (true)
        assert!(manifest.nodes["validator2"]
            .el_cli_flags()
            .unwrap()
            .contains(&"--arc.builder.wait-for-payload=true".to_string()));
    }

    #[test]
    fn test_arc_builder_wait_for_payload_omitted_when_unset() {
        let str = r#"
        [nodes.validator1]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        assert!(!manifest.nodes["validator1"]
            .el_cli_flags()
            .unwrap()
            .iter()
            .any(|f| f.contains("wait-for-payload")));
    }

    #[test]
    fn test_node_groups() {
        let str = r#"
        [node_groups]
        FULL_NODES = ["full1", "full2"]
        TRUSTED = ["ALL_VALIDATORS", "FULL_NODES", "other_node"]

        [nodes.validator1]
        cl_persistent_peers = ["TRUSTED"]
        [nodes.validator2]
        [nodes.validator3]
        [nodes.validator4]
        [nodes.full1]
        cl_persistent_peers = ["ALL_NON_VALIDATORS"]
        [nodes.full2]
        cl_persistent_peers = ["ALL_VALIDATORS"]
        [nodes.sentry]
        cl_persistent_peers = ["ALL_NODES"]
        [nodes.other_node]
        "#;
        let manifest = Manifest::from_string(str).unwrap();

        assert_eq!(manifest.nodes.len(), 8);
        assert_eq!(
            manifest.nodes["validator1"].cl_persistent_peers,
            Some(vec![
                "validator2".to_string(),
                "validator3".to_string(),
                "validator4".to_string(),
                "full1".to_string(),
                "full2".to_string(),
                "other_node".to_string(),
            ])
        );
        assert_eq!(
            manifest.nodes["full1"].cl_persistent_peers,
            Some(vec![
                "full2".to_string(),
                "sentry".to_string(),
                "other_node".to_string(),
            ])
        );
        assert_eq!(
            manifest.nodes["full2"].cl_persistent_peers,
            Some(vec![
                "validator1".to_string(),
                "validator2".to_string(),
                "validator3".to_string(),
                "validator4".to_string(),
            ])
        );
        assert_eq!(
            manifest.nodes["sentry"].cl_persistent_peers,
            Some(vec![
                "validator1".to_string(),
                "validator2".to_string(),
                "validator3".to_string(),
                "validator4".to_string(),
                "full1".to_string(),
                "full2".to_string(),
                "other_node".to_string(),
            ])
        );
    }

    #[test]
    fn test_node_group_with_non_existing_node() {
        let str = r#"
        [node_groups]
        INVALID = ["invalid", "ALL_NODES"]
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err());
    }

    #[test]
    fn test_node_group_with_repeated_elements() {
        let str = r#"
        [node_groups]
        GROUP_A = ["a", "b"]
        GROUP_B = ["GROUP_A", "a", "c"]
        [nodes.a]
        [nodes.b]
        [nodes.c]
        [nodes.d]
        cl_persistent_peers = ["GROUP_B"]
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        assert_eq!(
            manifest.nodes["d"].cl_persistent_peers,
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string(),])
        );
    }

    #[test]
    fn test_node_with_predefined_group_name() {
        let str = r#"
        [nodes.ALL_NODES]
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err());
    }

    #[test]
    fn test_node_with_group_name() {
        let str = r#"
        [node_groups]
        GROUP_A = ["a", "b"]
        [nodes.GROUP_A]
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_multiple_subnets() {
        let str = r#"
        [nodes.validator1]
        subnets = ["A"]
        [nodes.validator2]
        subnets = ["A", "default"]
        [nodes.validator3]
        subnets = ["default"]
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_load_multiple_subnets_chained() {
        let str = r#"
        [nodes.validator1]
        subnets = ["A"]
        [nodes.validator2]
        subnets = ["A", "B"]
        [nodes.validator3]
        subnets = ["B", "C"]
        [nodes.validator4]
        subnets = ["C", "default"]
        [nodes.validator5]
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_load_disconnected_subnets() {
        let str = r#"
        [nodes.validator1]
        subnets = ["A"]
        [nodes.validator2]
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_disconnected_multi_subnets() {
        let str = r#"
        [nodes.validator1]
        subnets = ["A", "C"]
        [nodes.validator2]
        subnets = ["B", "default"]
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err());
    }

    #[test]
    fn test_max_remote_signers_matches_json_keys() {
        // Ensure MAX_REMOTE_SIGNERS is synchronized with the number of keys
        // in tests/helpers/arc-remote-signer-keys.json
        assert_eq!(
            MAX_REMOTE_SIGNERS,
            PREDEFINED_ARC_REMOTE_SIGNER_PUBLIC_KEYS.len(),
            "MAX_REMOTE_SIGNERS ({}) must match the number of keys in arc-remote-signer-keys.json ({})",
            MAX_REMOTE_SIGNERS,
            PREDEFINED_ARC_REMOTE_SIGNER_PUBLIC_KEYS.len()
        );
    }

    #[test]
    fn test_voting_power_all_validators_specified() {
        let str = r#"
        [nodes.validator1]
        cl_voting_power = 2000
        [nodes.validator2]
        cl_voting_power = 1000
        [nodes.full1]
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        assert_eq!(manifest.validator_voting_powers(), Some(vec![2000, 1000]));
    }

    #[test]
    fn test_voting_power_none_specified() {
        let str = r#"
        [nodes.validator1]
        [nodes.validator2]
        [nodes.full1]
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        assert_eq!(manifest.validator_voting_powers(), None);
    }

    #[test]
    fn test_voting_power_partial_is_error() {
        let str = r#"
        [nodes.validator1]
        cl_voting_power = 2000
        [nodes.validator2]
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("cl_voting_power must be specified for all validators or none"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_voting_power_zero_is_error() {
        let str = r#"
        [nodes.validator1]
        cl_voting_power = 2000
        [nodes.validator2]
        cl_voting_power = 0
        "#;
        let result = Manifest::from_string(str);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("cl_voting_power must be greater than 0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_voting_power_on_non_validator_ignored() {
        let str = r#"
        [nodes.validator1]
        [nodes.full1]
        cl_voting_power = 9999
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        assert_eq!(manifest.validator_voting_powers(), None);
        assert_eq!(manifest.nodes["full1"].cl_voting_power, Some(9999));
    }

    #[test]
    fn test_voting_power_roundtrip() {
        let str = r#"
        [nodes.validator1]
        cl_voting_power = 2000
        [nodes.validator2]
        cl_voting_power = 1000
        "#;
        let manifest = Manifest::from_string(str).unwrap();
        let raw = RawManifest::try_from(manifest.clone()).unwrap();
        let toml_str = toml::to_string(&raw).unwrap();
        let manifest2 = Manifest::from_string(&toml_str).unwrap();
        assert_eq!(
            manifest.validator_voting_powers(),
            manifest2.validator_voting_powers()
        );
    }

    #[test]
    fn test_scenario_toml_files_are_valid() {
        unsafe { env::set_var("IMAGE_REGISTRY_URL", "ghcr.io/test-org/test-repo") };
        let scenarios_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios");
        let toml_files: Vec<_> = [
            scenarios_dir.as_path(),
            scenarios_dir.join("examples").as_path(),
        ]
        .iter()
        .flat_map(|dir| {
            std::fs::read_dir(dir)
                .unwrap_or_else(|e| panic!("Failed to read directory {}: {}", dir.display(), e))
                .filter_map(|entry| {
                    let path = entry.expect("Failed to read directory entry").path();
                    (path.is_file() && path.extension().is_some_and(|ext| ext == "toml"))
                        .then_some(path)
                })
        })
        .collect();

        assert!(
            !toml_files.is_empty(),
            "No TOML files found in scenarios directories"
        );

        for path in &toml_files {
            Manifest::from_file(path).unwrap_or_else(|e| {
                panic!("Failed to create Manifest from {}: {}", path.display(), e)
            });
        }
        unsafe { env::remove_var("IMAGE_REGISTRY_URL") };
    }

    #[test]
    fn filter_shared_subnets_single_node() {
        let manifest =
            Manifest::default_from_subnets(&[("node1".into(), vec!["default".into()])].into());
        let peers: Vec<_> = manifest
            .filter_nodes_with_shared_subnets(&"node1".into())
            .collect();
        assert_eq!(peers.len(), 0);
    }

    #[test]
    fn filter_shared_subnets_single_subnet() {
        let manifest = Manifest::default_from_subnets(
            &[
                ("node1".into(), vec!["default".into()]),
                ("node2".into(), vec!["default".into()]),
                ("node3".into(), vec!["default".into()]),
            ]
            .into(),
        );
        let peers: Vec<&String> = manifest
            .filter_nodes_with_shared_subnets(&"node1".into())
            .map(|(name, _)| name)
            .collect();
        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&&"node2".to_string()));
        assert!(peers.contains(&&"node3".to_string()));

        let subnets = &manifest.subnets;
        assert_eq!(subnets.shared_subnets("node1", "node2"), vec!["default"]);
        assert_eq!(subnets.shared_subnets("node1", "node3"), vec!["default"]);
        assert_eq!(subnets.shared_subnets("node2", "node3"), vec!["default"]);
    }

    #[test]
    fn filter_shared_subnets_disjoint_subnets() {
        let manifest = Manifest::default_from_subnets(
            &[
                ("node1".into(), vec!["A".into()]),
                ("node2".into(), vec!["B".into()]),
                ("bridge".into(), vec!["A".into(), "B".into()]),
            ]
            .into(),
        );
        let peers: Vec<&String> = manifest
            .filter_nodes_with_shared_subnets(&"node1".into())
            .map(|(name, _)| name)
            .collect();
        assert!(!peers.contains(&&"node2".to_string()));
        assert!(peers.contains(&&"bridge".to_string()));

        let subnets = &manifest.subnets;
        assert!(subnets.shared_subnets("node1", "node2").is_empty());
        assert_eq!(subnets.shared_subnets("node1", "bridge"), vec!["A"]);
        assert_eq!(subnets.shared_subnets("node2", "bridge"), vec!["B"]);
    }

    #[test]
    fn filter_shared_subnets_chain_topology() {
        let manifest = Manifest::default_from_subnets(
            &[
                ("n1".into(), vec!["A".into()]),
                ("bridge_ab".into(), vec!["A".into(), "B".into()]),
                ("n2".into(), vec!["B".into()]),
                ("bridge_bc".into(), vec!["B".into(), "C".into()]),
                ("n3".into(), vec!["C".into()]),
            ]
            .into(),
        );

        let peers_n1: Vec<&String> = manifest
            .filter_nodes_with_shared_subnets(&"n1".into())
            .map(|(name, _)| name)
            .collect();
        assert_eq!(peers_n1, vec!["bridge_ab"]);

        let peers_n2: Vec<&String> = manifest
            .filter_nodes_with_shared_subnets(&"n2".into())
            .map(|(name, _)| name)
            .collect();
        assert_eq!(peers_n2.len(), 2);
        assert!(peers_n2.contains(&&"bridge_ab".to_string()));
        assert!(peers_n2.contains(&&"bridge_bc".to_string()));

        let peers_n3: Vec<&String> = manifest
            .filter_nodes_with_shared_subnets(&"n3".into())
            .map(|(name, _)| name)
            .collect();
        assert_eq!(peers_n3, vec!["bridge_bc"]);

        let subnets = &manifest.subnets;
        assert_eq!(subnets.shared_subnets("n1", "bridge_ab"), vec!["A"]);
        assert_eq!(subnets.shared_subnets("n2", "bridge_bc"), vec!["B"]);
        assert_eq!(subnets.shared_subnets("n3", "bridge_bc"), vec!["C"]);
        assert!(subnets.shared_subnets("n1", "n2").is_empty());
        assert!(subnets.shared_subnets("n1", "n3").is_empty());
        assert!(subnets.shared_subnets("n2", "n3").is_empty());
    }

    #[test]
    fn test_substitute_env_vars_replaces_known_vars() {
        unsafe { env::set_var("QUAKE_TEST_REGISTRY", "ghcr.io/test-org/repo") };
        let input = r#"image_cl="${QUAKE_TEST_REGISTRY}/consensus:abc""#;
        let result = substitute_env_vars(input).unwrap();
        assert_eq!(result, r#"image_cl="ghcr.io/test-org/repo/consensus:abc""#);
        unsafe { env::remove_var("QUAKE_TEST_REGISTRY") };
    }

    #[test]
    fn test_substitute_env_vars_multiple_vars() {
        unsafe {
            env::set_var("QUAKE_TEST_A", "alpha");
            env::set_var("QUAKE_TEST_B", "beta");
        }
        let input = "${QUAKE_TEST_A}-${QUAKE_TEST_B}";
        let result = substitute_env_vars(input).unwrap();
        assert_eq!(result, "alpha-beta");
        unsafe {
            env::remove_var("QUAKE_TEST_A");
            env::remove_var("QUAKE_TEST_B");
        }
    }

    #[test]
    fn test_substitute_env_vars_no_vars() {
        let input = "image_cl=\"ghcr.io/org/repo:tag\"";
        let result = substitute_env_vars(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn test_substitute_env_vars_missing_var_returns_error() {
        let input = r#"image="${QUAKE_TEST_NONEXISTENT_VAR_12345}/foo""#;
        let result = substitute_env_vars(input);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("QUAKE_TEST_NONEXISTENT_VAR_12345"),
            "Error should mention the missing variable name, got: {msg}"
        );
    }

    #[test]
    fn test_substitute_env_vars_malformed_empty_braces() {
        let result = substitute_env_vars("image=${}");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Malformed placeholder"),);
    }

    #[test]
    fn test_substitute_env_vars_malformed_hyphen_in_name() {
        let result = substitute_env_vars("image=${IMAGE-REGISTRY}/foo");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Malformed placeholder"),);
    }

    #[test]
    fn test_substitute_env_vars_malformed_leading_digit() {
        let result = substitute_env_vars("image=${1FOO}/bar");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Malformed placeholder"),);
    }

    #[test]
    fn test_substitute_env_vars_malformed_unclosed() {
        let result = substitute_env_vars("image=${UNCLOSED");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Malformed placeholder"),);
    }
}
