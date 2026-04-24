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

use alloy_primitives::Address;
use arc_consensus_types::Config as ClConfigOverride;
use color_eyre::eyre::{bail, Result};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::manifest::subnets::Subnets;
use crate::manifest::{
    default_subnet_singleton, ClGossipSubConfig, ClPruningPreset, DockerImages, ElConfigOverride,
    EngineApiConnection, Manifest, Node, NodeClConfig, NodeType, RemoteKeyId,
};
use crate::node::SubnetName;
use crate::setup::supports_cli_flags;
use crate::util::merge_toml_values;

/// Node name prefix that indicates a validator node.
const VALIDATOR_PREFIX: &str = "val";

/// Pre-defined node groups.
pub(crate) const NODE_GROUP_ALL: &str = "ALL_NODES";
pub(crate) const NODE_GROUP_VALIDATORS: &str = "ALL_VALIDATORS";
pub(crate) const NODE_GROUP_NON_VALIDATORS: &str = "ALL_NON_VALIDATORS";

fn is_reserved_node_group_name(name: &str) -> bool {
    matches!(
        name,
        NODE_GROUP_ALL | NODE_GROUP_VALIDATORS | NODE_GROUP_NON_VALIDATORS
    )
}

/// Wrapper for execution layer configuration in TOML.
///
/// Supports the `el.config` TOML syntax where `config` is a table
/// of key-value pairs representing Reth CLI flags.
///
/// # Example
/// ```toml
/// [el.config]
/// http = true
/// http.port = 8545
/// txpool.nolocals = true
/// ```
/// or equivalently:
/// ```toml
/// el.config.http = true
/// el.config.http.port = 8545
/// el.config.txpool.nolocals = true
/// ```
///
#[derive(Debug, Deserialize, Default, Serialize, PartialEq)]
#[serde(default)]
pub struct ElConfig {
    /// Execution layer (Reth) CLI flags as a TOML table.
    /// Keys become flag names, values become flag values.
    /// e.g., `builder.deadline = 5` becomes `--builder.deadline=5`
    pub config: toml::Table,
}

/// Wrapper for consensus layer configuration in TOML.
///
/// Supports the `cl.config` TOML syntax where `config` is a table
/// of Malachite configuration fields.
///
/// # Example
/// ```toml
/// [cl.config]
/// logging.log_level = "debug"
/// ```
/// or equivalently:
/// ```toml
/// cl.config.logging.log_level = "debug"
/// ```
#[derive(Debug, Deserialize, Default, Serialize, PartialEq)]
#[serde(default)]
pub struct ClConfig {
    pub config: toml::Table,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

fn is_default_subnet(v: &Vec<String>) -> bool {
    *v == default_subnet_singleton()
}

fn is_latency_emulation_default(v: &bool) -> bool {
    *v
}

/// Raw representation of a node as it appears in the TOML manifest.
/// Used for initial deserialization before transformation into [`Node`].
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct RawNode {
    /// Consensus layer (Malachite) config for this node.
    /// Uses `cl.config` syntax in TOML.
    #[serde(skip_serializing_if = "is_default")]
    cl: ClConfig,

    /// Execution layer (Reth) CLI flags for this node.
    /// Uses `el.config` syntax in TOML.
    #[serde(skip_serializing_if = "is_default")]
    el: ElConfig,

    start_at: Option<u64>,

    region: Option<String>,

    cl_persistent_peers: Option<Vec<String>>,

    #[serde(skip_serializing_if = "is_default")]
    cl_persistent_peers_only: bool,

    #[serde(default, skip_serializing_if = "is_default")]
    cl_gossipsub: ClGossipSubConfig,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    cl_prune_preset: Option<ClPruningPreset>,

    #[serde(
        default = "default_subnet_singleton",
        skip_serializing_if = "is_default_subnet"
    )]
    subnets: Vec<String>,

    remote_signer: Option<RemoteKeyId>,

    /// Enable follow mode (no consensus, sync from remote nodes)
    #[serde(skip_serializing_if = "is_default")]
    follow: bool,

    /// Remote node names to fetch blocks from in follow mode
    #[serde(skip_serializing_if = "is_default")]
    follow_endpoints: Vec<String>,

    /// Voting power for this validator in the genesis validator set.
    /// Only meaningful for validator nodes. When set, all validators must specify it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cl_voting_power: Option<u64>,

    /// Address to receive transaction fees and block rewards (--suggested-fee-recipient).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cl_suggested_fee_recipient: Option<Address>,

    /// Mark this node as external (operated by a third party).
    /// External validators are expected to be multi-hop in mesh health checks
    /// rather than fully-connected. Also applies to their dedicated sentries.
    #[serde(default, skip_serializing_if = "is_default")]
    external: bool,
}

/// Raw representation of the manifest as it appears in TOML.
/// Used for initial deserialization before transformation into [`Manifest`].
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct RawManifest {
    name: Option<String>,
    description: Option<String>,
    #[serde(skip_serializing_if = "is_latency_emulation_default")]
    latency_emulation: bool,
    #[serde(default)]
    monitoring_bind_host: Option<String>,
    /// Global consensus layer (Malachite) config applied to all nodes.
    /// Individual node `cl.config` values override these when keys match.
    /// Uses `cl.config` syntax in TOML.
    #[serde(skip_serializing_if = "is_default")]
    cl: ClConfig,
    /// Global execution layer (Reth) CLI flags applied to all nodes.
    /// Individual node `el.config` values override these when keys match.
    /// Uses `el.config` syntax in TOML.
    #[serde(skip_serializing_if = "is_default")]
    el: ElConfig,
    engine_api_connection: Option<EngineApiConnection>,
    #[serde(default)]
    arc_image_tag: Option<String>,
    #[serde(default)]
    arc_image_registry: Option<String>,
    #[serde(default)]
    nodes: IndexMap<String, RawNode>,
    #[serde(skip_serializing_if = "is_default")]
    node_groups: IndexMap<String, Vec<String>>,
    el_init_hardfork: Option<String>,
    #[serde(default, alias = "image_tag_cl")]
    image_cl: Option<String>,
    #[serde(default, alias = "image_tag_el")]
    image_el: Option<String>,
    #[serde(default, alias = "image_tag_cl_upgrade")]
    image_cl_upgrade: Option<String>,
    #[serde(default, alias = "image_tag_el_upgrade")]
    image_el_upgrade: Option<String>,
}

impl RawManifest {
    /// Build the `DockerImages` referenced by this raw manifest.
    pub fn images(&self) -> DockerImages {
        DockerImages {
            cl: self.image_cl.clone(),
            el: self.image_el.clone(),
            cl_upgrade: self.image_cl_upgrade.clone(),
            el_upgrade: self.image_el_upgrade.clone(),
        }
    }
}

impl Default for RawManifest {
    fn default() -> Self {
        Self {
            latency_emulation: true,
            name: None,
            description: None,
            monitoring_bind_host: None,
            cl: ClConfig::default(),
            el: ElConfig::default(),
            engine_api_connection: None,
            arc_image_tag: None,
            arc_image_registry: None,
            nodes: IndexMap::new(),
            node_groups: IndexMap::new(),
            el_init_hardfork: None,
            image_cl: None,
            image_el: None,
            image_cl_upgrade: None,
            image_el_upgrade: None,
        }
    }
}

/// Collect all leaf keys from a TOML table as dot-separated paths.
fn collect_toml_keys(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(sub) => collect_toml_keys(sub, &path, out),
            _ => out.push(path),
        }
    }
}

/// Reject manifests where a node sets both `cl_prune_preset` and `cl.config.prune.*`.
/// These are mutually exclusive: the preset is a named shortcut while explicit prune
/// config overrides individual knobs. Allowing both would make precedence ambiguous.
fn validate_prune_exclusivity(raw: &RawManifest) -> Result<()> {
    let global_has_prune = has_prune_keys(&raw.cl.config);
    for (node_name, raw_node) in &raw.nodes {
        let node_has_prune = has_prune_keys(&raw_node.cl.config) || global_has_prune;
        if raw_node.cl_prune_preset.is_some() && node_has_prune {
            bail!(
                "{node_name}: cl_prune_preset and cl.config.prune.* are mutually exclusive. \
                 Use either a preset (full/minimal) or explicit prune settings, not both."
            );
        }
    }
    Ok(())
}

fn has_prune_keys(table: &toml::Table) -> bool {
    let mut keys = Vec::new();
    collect_toml_keys(table, "", &mut keys);
    keys.iter().any(|k| k.starts_with("prune."))
}

impl TryFrom<RawManifest> for Manifest {
    type Error = color_eyre::eyre::Error;

    fn try_from(raw: RawManifest) -> Result<Self> {
        if raw.arc_image_tag.is_some() || raw.arc_image_registry.is_some() {
            warn!("arc_image_tag and arc_image_registry are deprecated; use image_cl/image_el with full image references instead");
        }

        validate_prune_exclusivity(&raw)?;

        // Build Docker images (needed early to determine CL config format)
        let images = raw.images();

        let node_names = raw.nodes.keys().cloned().collect::<Vec<_>>();
        let custom_node_groups = raw.node_groups.clone();
        for group_name in custom_node_groups.keys() {
            if is_reserved_node_group_name(group_name) {
                bail!("Node group '{group_name}' uses a reserved built-in group name");
            }
        }
        let node_groups = build_node_groups(&node_names, &custom_node_groups);

        // Check that node names are not used as node group names
        for node_group in node_groups.keys() {
            if node_names.contains(node_group) {
                bail!("Node group '{node_group}' conflicts with a node name");
            }
        }

        // Check that node names in groups are valid node names
        for (group_name, group) in node_groups.iter() {
            for node_name in group {
                if !node_names.contains(node_name) {
                    bail!("Node group '{group_name}' contains invalid node name '{node_name}'");
                }
            }
        }

        // Merge default CL and EL configs with manifest's global config.
        // Precedence: defaults < manifest global < per-node
        // The CL default depends on the image version: Modern uses StartCmd,
        // Legacy uses ClConfigOverride.
        let is_modern = supports_cli_flags(images.cl.as_deref());
        let default_cl = if is_modern {
            toml::Value::try_from(arc_node_consensus_cli::cmd::start::StartCmd::default())?
        } else {
            toml::Value::try_from(ClConfigOverride::default())?
        };
        let manifest_cl = toml::Value::Table(raw.cl.config.clone());
        let global_cl_config = merge_toml_values(default_cl, manifest_cl)?;

        let default_el = toml::Value::try_from(ElConfigOverride::default())?;
        let manifest_el = toml::Value::Table(raw.el.config.clone());
        let global_el_config = merge_toml_values(default_el, manifest_el)?;

        // Build nodes map from raw nodes
        let mut nodes = IndexMap::new();
        let mut node_subnets = IndexMap::new();
        for (key, raw_node) in raw.nodes {
            // Determine node type based on key prefix
            let node_type = if is_validator(&key) {
                NodeType::Validator
            } else {
                NodeType::NonValidator
            };

            // Expand node group names in persistent peers list and remove self from
            // the list
            let cl_persistent_peers = raw_node.cl_persistent_peers.map(|peers| {
                expand_node_group(&peers, &node_groups)
                    .into_iter()
                    .filter(|n| *n != key)
                    .collect()
            });

            // Merge node-specific CL config with global CL config
            let node_cl_config = toml::Value::Table(raw_node.cl.config);
            let cl_config_toml = merge_toml_values(global_cl_config.clone(), node_cl_config)?;

            // Version-branched deserialization
            let cl_config = if is_modern {
                NodeClConfig::Modern(cl_config_toml.try_into()?)
            } else {
                NodeClConfig::Legacy(cl_config_toml.try_into()?)
            };

            // Merge global el.config with node-specific el.config as TOML
            let node_el_config = toml::Value::Table(raw_node.el.config);
            let el_config = merge_toml_values(global_el_config.clone(), node_el_config)?;

            let mut el_config: ElConfigOverride = el_config.try_into()?;

            // Extract trusted_peers from el.config: expand group/node names, remove self,
            // and strip the key so it is not forwarded as a Reth CLI flag.
            let el_trusted_peers = if !el_config.trusted_peers.is_empty() {
                let names = el_config.trusted_peers;
                el_config.trusted_peers = vec![];
                let peers: Vec<String> = expand_node_group(&names, &node_groups)
                    .into_iter()
                    .filter(|n| *n != key)
                    .collect();
                // Normalize: empty after self-filtering means "no explicit peers" → None
                if peers.is_empty() {
                    None
                } else {
                    Some(peers)
                }
            } else {
                None
            };

            node_subnets.insert(key.clone(), raw_node.subnets);
            nodes.insert(
                key,
                Node {
                    node_type,
                    cl_config,
                    el_config,
                    start_at: raw_node.start_at,
                    region: raw_node.region,
                    cl_persistent_peers,
                    cl_persistent_peers_only: raw_node.cl_persistent_peers_only,
                    cl_gossipsub: raw_node.cl_gossipsub,
                    el_trusted_peers,
                    remote_signer: raw_node.remote_signer,
                    follow: raw_node.follow,
                    follow_endpoints: raw_node.follow_endpoints,
                    cl_voting_power: raw_node.cl_voting_power,
                    cl_prune_preset: raw_node.cl_prune_preset,
                    cl_suggested_fee_recipient: raw_node.cl_suggested_fee_recipient,
                    external: raw_node.external,
                },
            );
        }

        if let Some(ref host) = raw.monitoring_bind_host {
            host.parse::<std::net::IpAddr>()
                .map_err(|_| color_eyre::eyre::eyre!("Invalid monitoring_bind_host: {host}"))?;
        }

        Ok(Manifest {
            name: raw.name,
            description: raw.description,
            latency_emulation: raw.latency_emulation,
            monitoring_bind_host: raw.monitoring_bind_host,
            engine_api_connection: raw.engine_api_connection,
            subnets: Subnets::new(&node_subnets),
            images,
            nodes,
            node_groups: custom_node_groups,
            el_init_hardfork: raw.el_init_hardfork,
        })
    }
}

impl TryFrom<Manifest> for RawManifest {
    type Error = color_eyre::eyre::Error;

    fn try_from(manifest: Manifest) -> Result<Self> {
        let node_groups = manifest.node_groups.clone();

        Ok(Self {
            name: manifest.name,
            description: manifest.description,
            latency_emulation: manifest.latency_emulation,
            monitoring_bind_host: manifest.monitoring_bind_host,
            cl: ClConfig::default(),
            el: ElConfig::default(),
            engine_api_connection: manifest.engine_api_connection,
            nodes: manifest
                .nodes
                .iter()
                .map(|(name, node)| {
                    Ok((
                        name.clone(),
                        RawNode::from_node_with_global_config(
                            node.clone(),
                            &manifest.subnets.subnets_of(name),
                            node.el_trusted_peers.clone(),
                        )?,
                    ))
                })
                .collect::<Result<_, Self::Error>>()?,
            node_groups,
            el_init_hardfork: manifest.el_init_hardfork,
            image_cl: manifest.images.cl,
            image_el: manifest.images.el,
            image_cl_upgrade: manifest.images.cl_upgrade,
            image_el_upgrade: manifest.images.el_upgrade,
            arc_image_tag: None,
            arc_image_registry: None,
        })
    }
}

impl RawNode {
    /// Create a RawNode from a Node, filtering out config values that match the global config.
    ///
    /// The caller (Manifest → RawManifest conversion) must ensure `el_config` already contains
    /// `trusted_peers` when the node has `el_trusted_peers` set, so that config_diff round-trips
    /// correctly. See the map closure in `From<Manifest> for RawManifest`.
    fn from_node_with_global_config(
        node: Node,
        subnets: &[SubnetName],
        trusted_peers: Option<Vec<String>>,
    ) -> Result<Self> {
        let mut el_config = node.el_config.clone();
        el_config.trusted_peers = trusted_peers.unwrap_or_default();
        let node_el_table = toml::Table::try_from(el_config)?;
        let default_el_config: toml::Table = toml::Table::try_from(ElConfigOverride::default())?;

        // Serialize cl_config to TOML based on variant
        let cl_config_table = match &node.cl_config {
            NodeClConfig::Modern(start_cmd) => {
                let table = toml::Table::try_from(start_cmd)?;
                let default_table =
                    toml::Table::try_from(arc_node_consensus_cli::cmd::start::StartCmd::default())?;
                Self::config_diff(&table, &default_table)
            }
            NodeClConfig::Legacy(config) => {
                let table = toml::Table::try_from(config)?;
                let default_table = toml::Table::try_from(ClConfigOverride::default())?;
                Self::config_diff(&table, &default_table)
            }
        };

        Ok(Self {
            cl: ClConfig {
                config: cl_config_table,
            },
            el: ElConfig {
                config: Self::config_diff(&node_el_table, &default_el_config),
            },
            start_at: node.start_at,
            region: node.region,
            cl_persistent_peers: node.cl_persistent_peers,
            cl_persistent_peers_only: node.cl_persistent_peers_only,
            cl_gossipsub: node.cl_gossipsub.clone(),
            cl_prune_preset: node.cl_prune_preset,
            subnets: subnets.to_vec(),
            remote_signer: node.remote_signer,
            follow: node.follow,
            follow_endpoints: node.follow_endpoints,
            cl_voting_power: node.cl_voting_power,
            cl_suggested_fee_recipient: node.cl_suggested_fee_recipient,
            external: node.external,
        })
    }

    /// Computes the difference between node config and global config.
    /// Returns only the keys/values in `node_config` that differ from `global_config`.
    pub(super) fn config_diff(
        node_config: &toml::Table,
        global_config: &toml::Table,
    ) -> toml::Table {
        let mut diff = toml::Table::new();

        for (key, node_value) in node_config {
            match global_config.get(key) {
                Some(global_value) => match (node_value, global_value) {
                    (toml::Value::Table(node_table), toml::Value::Table(global_table)) => {
                        let nested_diff = Self::config_diff(node_table, global_table);
                        if !nested_diff.is_empty() {
                            diff.insert(key.clone(), toml::Value::Table(nested_diff));
                        }
                    }
                    _ => {
                        if node_value != global_value {
                            diff.insert(key.clone(), node_value.clone());
                        }
                    }
                },
                None => {
                    diff.insert(key.clone(), node_value.clone());
                }
            }
        }

        diff
    }
}

/// Build the runtime node-group map from manifest node names and custom groups.
///
/// The returned map always contains the predefined groups
/// `ALL_NODES`, `ALL_VALIDATORS`, and `ALL_NON_VALIDATORS`, followed by the
/// custom groups in declaration order. Custom groups from the manifest are expanded
/// against the groups already present in the map, so a later custom group may
/// reference an earlier one or a predefined group.
pub(crate) fn build_node_groups(
    node_names: &[String],
    custom_node_groups: &IndexMap<String, Vec<String>>,
) -> IndexMap<String, Vec<String>> {
    let mut resolved_groups = IndexMap::new();
    resolved_groups.insert(NODE_GROUP_ALL.to_string(), node_names.to_vec());

    let (validators, non_validators): (Vec<_>, Vec<_>) = node_names
        .iter()
        .cloned()
        .partition(|name| is_validator(name));
    resolved_groups.insert(NODE_GROUP_VALIDATORS.to_string(), validators);
    resolved_groups.insert(NODE_GROUP_NON_VALIDATORS.to_string(), non_validators);

    for (group_name, group_members) in custom_node_groups {
        let expanded_group = expand_node_group(group_members, &resolved_groups);
        resolved_groups.insert(group_name.clone(), expanded_group);
    }

    resolved_groups
}

/// Expand the group names in the list using the existing node group definitions.
pub(crate) fn expand_node_group(
    names: &[String],
    existing_node_groups: &IndexMap<String, Vec<String>>,
) -> Vec<String> {
    // Use an IndexSet to avoid duplicates while preserving order
    let mut expanded_names = IndexSet::new();
    for name in names {
        if let Some(group_members) = existing_node_groups.get(name) {
            expanded_names.extend(group_members.iter().cloned());
        } else {
            expanded_names.insert(name.clone());
        }
    }
    expanded_names.into_iter().collect()
}

/// Returns true if the node is a validator, i.e., its name starts with a validator prefix.
pub(crate) fn is_validator(node_name: &str) -> bool {
    node_name.starts_with(VALIDATOR_PREFIX)
}

#[cfg(test)]
mod tests {
    use arc_node_consensus_cli::cmd::start::StartCmd;
    use malachitebft_config::LogLevel;

    use crate::manifest::ElTxpoolConfig;

    use super::*;

    /// el.config.trusted_peers round-trips through RawManifest → Manifest → RawManifest → Manifest.
    #[test]
    fn test_el_trusted_peers_roundtrip() {
        let toml = r#"
        image_cl = "arc_consensus:v0.4.0"
        [nodes.val1.el.config]
        trusted_peers = ["val2"]
        [nodes.val2]
        "#;

        // First parse: TOML → Manifest
        let manifest1 = Manifest::from_string(toml).unwrap();
        assert_eq!(
            manifest1.nodes["val1"].el_trusted_peers,
            Some(vec!["val2".to_string()])
        );
        assert!(
            manifest1.nodes["val1"].el_config.trusted_peers.is_empty(),
            "trusted-peers must be stripped from el_config after extraction"
        );

        // Serialize back: Manifest → RawManifest → TOML
        let raw = RawManifest::try_from(manifest1).unwrap();
        let serialized = toml::to_string(&raw).unwrap();
        assert!(
            serialized.contains("trusted_peers"),
            "trusted_peers must be present in serialized TOML"
        );

        // Second parse: TOML → Manifest (round-trip)
        let manifest2 = Manifest::from_string(&serialized).unwrap();
        assert_eq!(
            manifest2.nodes["val1"].el_trusted_peers,
            Some(vec!["val2".to_string()])
        );
    }

    /// When trusted_peers is set at the global [el.config] level it is inherited by all nodes.
    /// On round-trip, config_diff omits it from per-node sections (values match global), so it
    /// stays in the global section and is re-inherited on re-parse.
    #[test]
    fn test_el_trusted_peers_global_roundtrip() {
        let toml = r#"
        image_cl = "arc_consensus:v0.4.0"
        [el.config]
        trusted_peers = ["val2"]
        [nodes.val1]
        [nodes.val2]
        "#;

        // val1 inherits global trusted_peers ["val2"] (self-filtered: val2 remains).
        // val2 inherits global trusted_peers ["val2"] (self-filtered: only itself → None).
        let manifest1 = Manifest::from_string(toml).unwrap();
        assert_eq!(
            manifest1.nodes["val1"].el_trusted_peers,
            Some(vec!["val2".to_string()]),
        );
        assert_eq!(manifest1.nodes["val2"].el_trusted_peers, None);

        // Serialize back: since Manifest no longer tracks global config separately,
        // trusted_peers will appear in per-node sections (val1 only, since val2 has None).
        let raw = RawManifest::try_from(manifest1).unwrap();
        let serialized = toml::to_string(&raw).unwrap();
        assert!(
            serialized.contains("trusted_peers"),
            "trusted_peers must survive serialization"
        );

        // Re-parse: same result.
        let manifest2 = Manifest::from_string(&serialized).unwrap();
        assert_eq!(
            manifest2.nodes["val1"].el_trusted_peers,
            Some(vec!["val2".to_string()]),
        );
        assert_eq!(manifest2.nodes["val2"].el_trusted_peers, None);
    }

    #[test]
    fn test_custom_node_groups_roundtrip() {
        let toml = r#"
        image_cl = "arc_consensus:v0.4.0"
        [node_groups]
        FULL_NODES = ["full1", "full2"]
        TRUSTED = ["ALL_VALIDATORS", "FULL_NODES", "other_node"]

        [nodes.validator1]
        [nodes.validator2]
        [nodes.full1]
        [nodes.full2]
        [nodes.other_node]
        "#;
        let expected_custom_groups = IndexMap::from([
            (
                "FULL_NODES".to_string(),
                vec!["full1".to_string(), "full2".to_string()],
            ),
            (
                "TRUSTED".to_string(),
                vec![
                    "ALL_VALIDATORS".to_string(),
                    "FULL_NODES".to_string(),
                    "other_node".to_string(),
                ],
            ),
        ]);

        let manifest1 = Manifest::from_string(toml).unwrap();
        assert_eq!(manifest1.node_groups, expected_custom_groups);

        let raw1 = RawManifest::try_from(manifest1).unwrap();
        assert_eq!(raw1.node_groups, expected_custom_groups);

        let serialized_raw = toml::to_string(&raw1).unwrap();
        assert!(
            serialized_raw.contains("[node_groups]"),
            "custom node_groups must be present in serialized TOML"
        );

        let raw2: RawManifest = toml::from_str(&serialized_raw).unwrap();
        assert_eq!(raw2.node_groups, expected_custom_groups);

        let manifest2 = Manifest::from_string(&serialized_raw).unwrap();
        assert_eq!(manifest2.node_groups, expected_custom_groups);
    }

    /// el.config.trusted_peers must be an array; a scalar value should return an error.
    #[test]
    fn test_el_trusted_peers_wrong_type_returns_error() {
        let toml = r#"
        [nodes.val1.el.config]
        trusted_peers = "val2"
        [nodes.val2]
        "#;
        let result = Manifest::from_string(toml);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to merge toml values: array and string"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_prune_preset_and_cl_config_prune_are_mutually_exclusive() {
        let toml_str = r#"
            image_cl = "ghcr.io/org/arc-consensus:latest"
            [nodes.val1]
            cl_prune_preset = "minimal"
            cl.config.prune.certificates_distance = 500
        "#;
        let raw: RawManifest = toml::from_str(toml_str).unwrap();
        let result = Manifest::try_from(raw);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mutually exclusive"),
            "should mention mutual exclusivity: {msg}"
        );
    }

    #[test]
    fn test_prune_preset_and_global_cl_config_prune_are_mutually_exclusive() {
        let toml_str = r#"
            image_cl = "ghcr.io/org/arc-consensus:latest"
            cl.config.prune.certificates_distance = 500
            [nodes.val1]
            cl_prune_preset = "minimal"
        "#;
        let raw: RawManifest = toml::from_str(toml_str).unwrap();
        let result = Manifest::try_from(raw);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mutually exclusive"),
            "global prune + per-node preset should conflict: {msg}"
        );
    }

    #[test]
    fn test_prune_preset_without_cl_config_prune_is_allowed() {
        let toml_str = r#"
            image_cl = "ghcr.io/org/arc-consensus:latest"
            [nodes.val1]
            cl_prune_preset = "full"
            [nodes.val2]
        "#;
        let raw: RawManifest = toml::from_str(toml_str).unwrap();
        let result = Manifest::try_from(raw);
        assert!(
            result.is_ok(),
            "cl_prune_preset alone should be allowed: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_collect_toml_keys() {
        let table: toml::Table = toml::from_str(
            r#"[logging]
log_level = "info"
[consensus.p2p]
rpc_max_size = "42 Mib"
"#,
        )
        .unwrap();

        let mut keys = Vec::new();
        collect_toml_keys(&table, "", &mut keys);
        keys.sort();
        assert_eq!(
            keys,
            vec!["consensus.p2p.rpc_max_size", "logging.log_level",]
        );
    }

    #[test]
    fn test_collect_toml_keys_empty_table() {
        let mut keys = Vec::new();
        collect_toml_keys(&toml::Table::new(), "", &mut keys);
        assert!(keys.is_empty());
    }

    /// Manifest serialization should not include empty/default fields.
    /// Make sure that the default fields are skipped during serialization.
    #[test]
    fn test_default_manifest_serialization() {
        let node = Node {
            cl_config: NodeClConfig::Modern(StartCmd {
                log_level: Some(LogLevel::Info),
                ..StartCmd::default()
            }),
            el_config: ElConfigOverride {
                txpool: crate::manifest::ElTxpoolConfig {
                    pending_max_count: Some(2),
                    ..ElTxpoolConfig::default()
                },
                ..ElConfigOverride::default()
            },
            ..Node::default()
        };
        let manifest = Manifest::new(
            None,
            &IndexMap::from([
                ("val0".to_string(), node),
                ("val1".to_string(), Node::default()),
            ]),
            &IndexMap::from([
                ("val0".to_string(), default_subnet_singleton()),
                ("val1".to_string(), default_subnet_singleton()),
            ]),
        );
        let raw_manifest = RawManifest::try_from(manifest).unwrap();
        let serialized = toml::to_string(&raw_manifest).unwrap();
        // RawManifest skips empty/default fields (latency_emulation=true,
        // subnets=["default"], cl, el, node_groups, Option::None, etc.), and
        // serializes nodes as table sections [nodes.val0] rather than inline.
        assert_eq!(
            serialized,
            "[nodes.val0.cl.config]\nlog_level = \"info\"\n\n[nodes.val0.el.config.txpool]\npending_max_count = 2\n\n[nodes.val1]\n"
        );
    }
}
