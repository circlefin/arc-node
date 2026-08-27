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
use color_eyre::eyre::{bail, Result};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::cli_version::ensure_cl_image_supported;
use crate::manifest::subnets::Subnets;
use crate::manifest::{
    default_subnet_singleton, ClGossipSubConfig, ClPruningPreset, DockerImages, ElConfigOverride,
    EngineApiConnection, ImageOverride, Manifest, Node, NodeType, RemoteKeyId,
};
use crate::node::SubnetName;
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
    #[serde(skip_serializing_if = "is_empty_table")]
    pub config: toml::Table,

    /// Environment variables for the execution layer container.
    /// Keys become env var names, scalar values become their string form.
    /// e.g., `el.env.RUST_LOG = "debug"`.
    #[serde(skip_serializing_if = "is_empty_table")]
    pub env: toml::Table,
}

/// Wrapper for consensus layer configuration in TOML.
///
/// Supports the `cl.config` TOML syntax where `config` is a table of
/// consensus CLI-flag fields.
///
/// # Example
/// ```toml
/// [cl.config]
/// log_level = "debug"
/// ```
/// or equivalently:
/// ```toml
/// cl.config.log_level = "debug"
/// ```
#[derive(Debug, Deserialize, Default, Serialize, PartialEq)]
#[serde(default)]
pub struct ClConfig {
    #[serde(skip_serializing_if = "is_empty_table")]
    pub config: toml::Table,

    /// Environment variables for the consensus layer container.
    /// Keys become env var names, scalar values become their string form.
    /// e.g., `cl.env.ARC_HALT_AT_BLOCK_HEIGHT = 100`.
    #[serde(skip_serializing_if = "is_empty_table")]
    pub env: toml::Table,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

fn is_empty_table(table: &toml::Table) -> bool {
    table.is_empty()
}

/// Merge a global env table with a node-specific one, the node values winning on
/// matching keys. The env tables are flat (one level of scalar values), so a flat
/// override is sufficient — no recursive merge like CLI config tables.
fn merge_env_tables(global: &toml::Table, node: &toml::Table) -> toml::Table {
    let mut merged = global.clone();
    for (key, value) in node {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

/// Whether `key` is a valid environment variable name: a leading letter or
/// underscore followed by letters, digits, or underscores (`^[A-Za-z_][A-Za-z0-9_]*$`).
/// TOML permits quoted keys with arbitrary characters, but those would render as
/// invalid or injected YAML in the compose `environment:` block.
fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Convert an env table into a string map, coercing scalar values to their string
/// representation. Arrays and tables are rejected: an environment variable value is
/// always a single string. Keys are validated as environment variable names.
fn env_table_to_map(
    table: &toml::Table,
    node_name: &str,
    layer: &str,
) -> Result<IndexMap<String, String>> {
    let mut map = IndexMap::with_capacity(table.len());
    for (key, value) in table {
        if !is_valid_env_key(key) {
            bail!(
                "{node_name}: {layer}.env key {key:?} is not a valid environment variable name \
                 (expected ^[A-Za-z_][A-Za-z0-9_]*$)"
            );
        }
        let rendered = match value {
            toml::Value::String(s) => s.clone(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            toml::Value::Boolean(b) => b.to_string(),
            other => bail!(
                "{node_name}: {layer}.env.{key} must be a string, integer, float, or boolean, \
                 got {}",
                other.type_str()
            ),
        };
        if let Some(c) = rendered.chars().find(|c| c.is_control()) {
            bail!(
                "{node_name}: {layer}.env.{key} contains a control character (U+{:04X}); \
                 environment variable values must be single-line, control-char-free",
                c as u32
            );
        }
        map.insert(key.clone(), rendered);
    }
    Ok(map)
}

/// Convert a string env map back into a TOML table of string values, for the
/// `Manifest` → `RawManifest` round-trip.
fn env_map_to_table(env: &IndexMap<String, String>) -> toml::Table {
    env.iter()
        .map(|(k, v)| (k.clone(), toml::Value::String(v.clone())))
        .collect()
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

    /// Per-node consensus layer image override (mixed-version networks).
    /// Takes precedence over any node-group override and the global image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_cl: Option<String>,

    /// Per-node execution layer image override. See `image_cl`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_el: Option<String>,

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
    /// Per-node-group image overrides, keyed by an existing node-group name.
    /// Applied to every member unless the node sets its own `image_cl`/`image_el`.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    group_images: IndexMap<String, ImageOverride>,
    el_init_hardfork: Option<String>,
    #[serde(default, alias = "image_tag_cl")]
    image_cl: Option<String>,
    #[serde(default, alias = "image_tag_el")]
    image_el: Option<String>,
    #[serde(default, alias = "image_tag_cl_upgrade")]
    image_cl_upgrade: Option<String>,
    #[serde(default, alias = "image_tag_el_upgrade")]
    image_el_upgrade: Option<String>,
    /// EC2 instance type for validator/full nodes (remote only).
    node_size: Option<String>,
    /// EC2 instance type for the Control Center (remote only).
    cc_size: Option<String>,
    /// Root EBS volume size for nodes in GiB (remote only).
    node_disk_gb: Option<u32>,
    /// Root EBS volume size for the Control Center in GiB (remote only).
    cc_disk_gb: Option<u32>,
    /// Initial balance for each prefunded account, in whole token units (e.g. 1_000_000 = 1M USDC).
    /// Defaults to 1_000_000 when unset.
    extra_account_balance_usdc: Option<u64>,
    /// ProtocolConfig blockGasLimit and genesis header gas limit.
    /// Defaults to 30_000_000 when unset.
    block_gas_limit: Option<u64>,
    /// Root EBS volume type for nodes (e.g. "gp3", "io2") (remote only).
    node_volume_type: Option<String>,
    /// Provisioned IOPS for the node root EBS volume (remote only).
    node_volume_iops: Option<u32>,
    /// Place the node data directory on local instance-store NVMe instead of the root EBS
    /// volume (remote only). Requires an instance type with local NVMe; a no-op otherwise.
    #[serde(skip_serializing_if = "is_default")]
    node_data_on_instance_store: bool,
    /// CPU limit for the EL container (Docker `cpus`). Whole or fractional CPUs.
    el_cpu_limit: Option<f64>,
    /// Memory limit for the EL container, in GiB. Fractional values are allowed.
    el_memory_limit_gb: Option<f64>,
    /// CPU limit for the CL container (Docker `cpus`). Whole or fractional CPUs.
    cl_cpu_limit: Option<f64>,
    /// Memory limit for the CL container, in GiB. Fractional values are allowed.
    cl_memory_limit_gb: Option<f64>,
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
            group_images: IndexMap::new(),
            el_init_hardfork: None,
            image_cl: None,
            image_el: None,
            image_cl_upgrade: None,
            image_el_upgrade: None,
            node_size: None,
            cc_size: None,
            node_disk_gb: None,
            cc_disk_gb: None,
            extra_account_balance_usdc: None,
            block_gas_limit: None,
            node_volume_type: None,
            node_volume_iops: None,
            node_data_on_instance_store: false,
            el_cpu_limit: None,
            el_memory_limit_gb: None,
            cl_cpu_limit: None,
            cl_memory_limit_gb: None,
        }
    }
}

/// Reject manifests where a node sets both `cl_prune_preset` and an explicit
/// `cl.config.prune_certificates_distance` / `cl.config.prune_certificates_before`.
/// These are mutually exclusive: the preset is a named shortcut while explicit prune
/// config overrides individual knobs. Allowing both would make precedence ambiguous.
fn validate_prune_exclusivity(raw: &RawManifest) -> Result<()> {
    let global_has_prune = has_prune_keys(&raw.cl.config);
    for (node_name, raw_node) in &raw.nodes {
        let node_has_prune = has_prune_keys(&raw_node.cl.config) || global_has_prune;
        if raw_node.cl_prune_preset.is_some() && node_has_prune {
            bail!(
                "{node_name}: cl_prune_preset and explicit \
                 cl.config.prune_certificates_distance/prune_certificates_before are \
                 mutually exclusive. Use either a preset (full/minimal) or explicit \
                 prune settings, not both."
            );
        }
    }
    Ok(())
}

/// Whether the table sets either explicit certificate-pruning knob.
fn has_prune_keys(table: &toml::Table) -> bool {
    table.contains_key("prune_certificates_distance")
        || table.contains_key("prune_certificates_before")
}

/// Resolve the image override that node-groups contribute to `node_name`.
/// Errors if more than one image-declaring group covers the node for the same
/// layer, since the winner would otherwise be arbitrary.
fn group_image_override(
    node_name: &str,
    group_images: &IndexMap<String, ImageOverride>,
    node_groups: &IndexMap<String, Vec<String>>,
) -> Result<ImageOverride> {
    let mut cl: Option<(&str, &str)> = None;
    let mut el: Option<(&str, &str)> = None;
    for (gname, ovr) in group_images {
        let Some(members) = node_groups.get(gname) else {
            continue;
        };
        if !members.iter().any(|m| m == node_name) {
            continue;
        }
        if let Some(img) = ovr.image_cl.as_deref() {
            if let Some((prev, _)) = cl {
                bail!("Node '{node_name}' gets image_cl from groups '{prev}' and '{gname}'; remove the overlap");
            }
            cl = Some((gname, img));
        }
        if let Some(img) = ovr.image_el.as_deref() {
            if let Some((prev, _)) = el {
                bail!("Node '{node_name}' gets image_el from groups '{prev}' and '{gname}'; remove the overlap");
            }
            el = Some((gname, img));
        }
    }
    Ok(ImageOverride {
        image_cl: cl.map(|(_, i)| i.to_string()),
        image_el: el.map(|(_, i)| i.to_string()),
    })
}

impl TryFrom<RawManifest> for Manifest {
    type Error = color_eyre::eyre::Error;

    fn try_from(raw: RawManifest) -> Result<Self> {
        if raw.arc_image_tag.is_some() || raw.arc_image_registry.is_some() {
            warn!("arc_image_tag and arc_image_registry are deprecated; use image_cl/image_el with full image references instead");
        }

        validate_prune_exclusivity(&raw)?;

        let images = raw.images();

        // Pre-v0.5.0 CL releases require a config.toml that Quake no longer
        // generates. Reject them here rather than letting the container fail at
        // startup with unrecognized CLI flags.
        ensure_cl_image_supported(images.cl.as_deref())?;
        ensure_cl_image_supported(images.cl_upgrade.as_deref())?;

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

        // Validate per-group image overrides: keys must name a known group, and
        // group CL images are subject to the same version floor as global images.
        for (group_name, ovr) in &raw.group_images {
            if !node_groups.contains_key(group_name) {
                bail!("group_images references unknown node group '{group_name}'");
            }
            ensure_cl_image_supported(ovr.image_cl.as_deref())?;
        }

        // Merge default CL and EL configs with manifest's global config.
        // Precedence: defaults < manifest global < per-node
        let default_cl =
            toml::Value::try_from(arc_node_consensus_cli::cmd::start::StartCmd::default())?;
        let manifest_cl = toml::Value::Table(raw.cl.config.clone());
        let global_cl_config = merge_toml_values(default_cl, manifest_cl)?;

        let default_el = toml::Value::try_from(ElConfigOverride::default())?;
        let manifest_el = toml::Value::Table(raw.el.config.clone());
        let global_el_config = merge_toml_values(default_el, manifest_el)?;

        // Global env tables, inherited by every node and overridden per-node.
        let global_el_env = raw.el.env.clone();
        let global_cl_env = raw.cl.env.clone();

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
            let cl_config = cl_config_toml.try_into()?;

            // Merge global env with node-specific env (node wins) for each layer.
            let el_env = env_table_to_map(
                &merge_env_tables(&global_el_env, &raw_node.el.env),
                &key,
                "el",
            )?;
            let cl_env = env_table_to_map(
                &merge_env_tables(&global_cl_env, &raw_node.cl.env),
                &key,
                "cl",
            )?;

            // Merge global el.config with node-specific el.config as TOML
            let node_el_config = toml::Value::Table(raw_node.el.config);
            let el_config = merge_toml_values(global_el_config.clone(), node_el_config)?;

            let mut el_config: ElConfigOverride = el_config.try_into()?;

            // Effective per-node images: inline override wins over the node-group
            // override; both fall back to the global image later (nodes.rs).
            let group_ovr = group_image_override(&key, &raw.group_images, &node_groups)?;
            let image_cl = raw_node.image_cl.or(group_ovr.image_cl);
            let image_el = raw_node.image_el.or(group_ovr.image_el);
            ensure_cl_image_supported(image_cl.as_deref())?;

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
                    image_cl,
                    image_el,
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
                    el_env,
                    cl_env,
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
            node_size: raw.node_size,
            cc_size: raw.cc_size,
            node_disk_gb: raw.node_disk_gb,
            cc_disk_gb: raw.cc_disk_gb,
            extra_account_balance_usdc: raw.extra_account_balance_usdc,
            block_gas_limit: raw.block_gas_limit,
            node_volume_type: raw.node_volume_type,
            node_volume_iops: raw.node_volume_iops,
            node_data_on_instance_store: raw.node_data_on_instance_store,
            el_cpu_limit: raw.el_cpu_limit,
            el_memory_limit_gb: raw.el_memory_limit_gb,
            cl_cpu_limit: raw.cl_cpu_limit,
            cl_memory_limit_gb: raw.cl_memory_limit_gb,
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
            // Group overrides are flattened onto each node during the forward
            // conversion, so they round-trip as per-node image_cl/image_el.
            group_images: IndexMap::new(),
            el_init_hardfork: manifest.el_init_hardfork,
            image_cl: manifest.images.cl,
            image_el: manifest.images.el,
            image_cl_upgrade: manifest.images.cl_upgrade,
            image_el_upgrade: manifest.images.el_upgrade,
            arc_image_tag: None,
            arc_image_registry: None,
            node_size: manifest.node_size,
            cc_size: manifest.cc_size,
            node_disk_gb: manifest.node_disk_gb,
            cc_disk_gb: manifest.cc_disk_gb,
            extra_account_balance_usdc: manifest.extra_account_balance_usdc,
            block_gas_limit: manifest.block_gas_limit,
            node_volume_type: manifest.node_volume_type,
            node_volume_iops: manifest.node_volume_iops,
            node_data_on_instance_store: manifest.node_data_on_instance_store,
            el_cpu_limit: manifest.el_cpu_limit,
            el_memory_limit_gb: manifest.el_memory_limit_gb,
            cl_cpu_limit: manifest.cl_cpu_limit,
            cl_memory_limit_gb: manifest.cl_memory_limit_gb,
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

        // Serialize cl_config to TOML, keeping only fields that differ from the
        // StartCmd default.
        let cl_config_table = {
            let table = toml::Table::try_from(&node.cl_config)?;
            let default_table =
                toml::Table::try_from(arc_node_consensus_cli::cmd::start::StartCmd::default())?;
            Self::config_diff(&table, &default_table)
        };

        Ok(Self {
            cl: ClConfig {
                config: cl_config_table,
                env: env_map_to_table(&node.cl_env),
            },
            el: ElConfig {
                config: Self::config_diff(&node_el_table, &default_el_config),
                env: env_map_to_table(&node.el_env),
            },
            image_cl: node.image_cl,
            image_el: node.image_el,
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
        image_cl = "arc_consensus:latest"
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
        image_cl = "arc_consensus:latest"
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
        image_cl = "arc_consensus:latest"
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
            cl.config.prune_certificates_distance = 500
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
            cl.config.prune_certificates_distance = 500
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

    /// Manifest serialization should not include empty/default fields.
    /// Make sure that the default fields are skipped during serialization.
    #[test]
    fn test_default_manifest_serialization() {
        let node = Node {
            cl_config: StartCmd {
                log_level: Some(LogLevel::Info),
                ..StartCmd::default()
            },
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

    /// node_data_on_instance_store round-trips through TOML → Manifest → RawManifest → TOML.
    #[test]
    fn test_node_data_on_instance_store_roundtrip() {
        let toml = r#"
        node_data_on_instance_store = true
        [nodes.val1]
        "#;

        let manifest1 = Manifest::from_string(toml).unwrap();
        assert!(manifest1.node_data_on_instance_store);

        let raw = RawManifest::try_from(manifest1).unwrap();
        let serialized = toml::to_string(&raw).unwrap();
        assert!(serialized.contains("node_data_on_instance_store = true"));

        let manifest2 = Manifest::from_string(&serialized).unwrap();
        assert!(manifest2.node_data_on_instance_store);
    }

    /// Omitting node_data_on_instance_store defaults to false (datadir stays on root EBS).
    #[test]
    fn test_node_data_on_instance_store_defaults_to_false() {
        let manifest = Manifest::from_string("[nodes.val1]\n").unwrap();
        assert!(!manifest.node_data_on_instance_store);
    }

    /// Global `el.env`/`cl.env` are inherited by every node; per-node entries
    /// override matching keys and add new ones.
    #[test]
    fn test_env_global_and_per_node_merge() {
        let toml = r#"
        [el.env]
        RUST_LOG = "info"
        SHARED = "global"
        [cl.env]
        ARC_HALT_AT_BLOCK_HEIGHT = 0

        [nodes.val1.el.env]
        RUST_LOG = "debug"
        [nodes.val1.cl.env]
        EXTRA = "x"
        [nodes.val2]
        "#;

        let manifest = Manifest::from_string(toml).unwrap();

        // val1 overrides RUST_LOG, keeps inherited SHARED, adds cl EXTRA.
        assert_eq!(manifest.nodes["val1"].el_env["RUST_LOG"], "debug");
        assert_eq!(manifest.nodes["val1"].el_env["SHARED"], "global");
        assert_eq!(
            manifest.nodes["val1"].cl_env["ARC_HALT_AT_BLOCK_HEIGHT"],
            "0"
        );
        assert_eq!(manifest.nodes["val1"].cl_env["EXTRA"], "x");

        // val2 inherits the global env unchanged.
        assert_eq!(manifest.nodes["val2"].el_env["RUST_LOG"], "info");
        assert_eq!(manifest.nodes["val2"].el_env["SHARED"], "global");
        assert_eq!(
            manifest.nodes["val2"].cl_env["ARC_HALT_AT_BLOCK_HEIGHT"],
            "0"
        );
        assert!(!manifest.nodes["val2"].cl_env.contains_key("EXTRA"));
    }

    /// Non-string scalar env values are coerced to their string form.
    #[test]
    fn test_env_scalar_coercion() {
        let toml = r#"
        [nodes.val1.el.env]
        COUNT = 42
        RATIO = 1.5
        FLAG = true
        NAME = "hello"
        "#;

        let manifest = Manifest::from_string(toml).unwrap();
        let env = &manifest.nodes["val1"].el_env;
        assert_eq!(env["COUNT"], "42");
        assert_eq!(env["RATIO"], "1.5");
        assert_eq!(env["FLAG"], "true");
        assert_eq!(env["NAME"], "hello");
    }

    /// Array/table env values are rejected: an env var value must be a scalar.
    #[test]
    fn test_env_non_scalar_value_errors() {
        let toml = r#"
        [nodes.val1.el.env]
        BAD = ["a", "b"]
        "#;

        let err = Manifest::from_string(toml).unwrap_err().to_string();
        assert!(
            err.contains("must be a string, integer, float, or boolean"),
            "unexpected error: {err}"
        );
    }

    /// Env keys that are not valid environment variable names are rejected, so they
    /// cannot inject YAML-significant characters into the compose `environment:` block.
    #[test]
    fn test_env_invalid_key_rejected() {
        for bad_key in ["BAD: KEY", "1LEADING_DIGIT", "has-hyphen"] {
            let toml = format!("[nodes.val1.el.env]\n{bad_key:?} = \"x\"\n");
            let err = Manifest::from_string(&toml).unwrap_err().to_string();
            assert!(
                err.contains("not a valid environment variable name"),
                "key {bad_key:?} should be rejected, got: {err}"
            );
        }
    }

    /// Env values containing control characters (newline, tab, CR) are
    /// rejected at parse time. Multi-line env vars don't survive shell
    /// round-tripping and would break the compose YAML. (Null bytes are
    /// rejected earlier by the TOML parser itself — `\0` isn't a TOML escape.)
    #[test]
    fn test_env_control_char_value_rejected() {
        for (label, bad_value) in [
            ("newline", "first\nsecond"),
            ("carriage return", "first\rsecond"),
            ("tab", "a\tb"),
        ] {
            let toml = format!("[nodes.val1.el.env]\nFOO = {bad_value:?}\n");
            let err = Manifest::from_string(&toml).unwrap_err().to_string();
            assert!(
                err.contains("control character"),
                "{label} value should be rejected, got: {err}"
            );
        }
    }

    /// Per-node env survives the Manifest → RawManifest → TOML → Manifest round-trip.
    /// The global env block is not retained separately, so it folds into each node.
    #[test]
    fn test_env_roundtrip() {
        let toml = r#"
        [el.env]
        RUST_LOG = "info"
        [nodes.val1.cl.env]
        ARC_HALT_AT_BLOCK_HEIGHT = "100"
        [nodes.val2]
        "#;

        let manifest1 = Manifest::from_string(toml).unwrap();

        let raw = RawManifest::try_from(manifest1).unwrap();
        let serialized = toml::to_string(&raw).unwrap();

        let manifest2 = Manifest::from_string(&serialized).unwrap();
        // val1: inherited el RUST_LOG + its own cl ARC_HALT_AT_BLOCK_HEIGHT.
        assert_eq!(manifest2.nodes["val1"].el_env["RUST_LOG"], "info");
        assert_eq!(
            manifest2.nodes["val1"].cl_env["ARC_HALT_AT_BLOCK_HEIGHT"],
            "100"
        );
        // val2: inherited el RUST_LOG, no cl env.
        assert_eq!(manifest2.nodes["val2"].el_env["RUST_LOG"], "info");
        assert!(manifest2.nodes["val2"].cl_env.is_empty());
    }
}
