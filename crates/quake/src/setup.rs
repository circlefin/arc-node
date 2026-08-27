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

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::Path};

use alloy_primitives::{address, Address};
use arc_node_consensus_cli::args::Args;
use arc_node_consensus_cli::cmd::start::{StartCmd, RUNTIME_SINGLE_THREADED};
use arc_node_consensus_cli::file::save_priv_validator_key;
use arc_node_consensus_cli::new::generate_private_keys;
use clap::Parser;
use color_eyre::eyre::{eyre, Context, Result};
use handlebars::Handlebars;
use indexmap::{IndexMap, IndexSet};
use itertools::Itertools;
use malachitebft_config::TransportProtocol;
use serde::Serialize;
use tracing::{debug, warn};
use url::Url;

use crate::cli_version::apply_version_compat;
use crate::infra::InfraType;
use crate::manifest::{self, Subnets};
use crate::node::{CidrBlock, NodeMetadata, NodeName, SubnetName, RETH_HTTP_BASE_PORT};
use crate::nodekey::{self, NodekeyData};
use crate::nodes::NodesMetadata;
use crate::testnet::QUAKE_DIR;
use crate::{shell, testnet};

const APP_CONSENSUS_DEFAULT_PORT: usize = 27000;
const APP_METRICS_DEFAULT_PORT: usize = 29000;
const APP_RPC_DEFAULT_PORT: usize = 31000;
const REMOTE_SIGNER_PROXY_PORT: usize = 10340;

/// Fallback recipient when a validator scenario doesn't set `cl_suggested_fee_recipient`.
/// Matches `LOCALDEV_FEE_RECIPIENT` in `tests/helpers/networks/localdev.ts`. Used by
/// scenarios like `localdev-remote-signer.toml`; `localdev.toml` sets per-validator
/// recipients explicitly.
const QUAKE_DEFAULT_FEE_RECIPIENT: Address = address!("0x65E0a200006D4FF91bD59F9694220dafc49dbBC1");

/// Compile system contracts and bindings
pub(crate) fn generate_system_contracts(repo_root_dir: &Path, force: bool) -> Result<()> {
    let npm_dir = repo_root_dir.join("node_modules");
    if !npm_dir.exists() {
        let cmd = "npm install";
        shell::exec("bash", vec!["-c", cmd], repo_root_dir, None, false)?;
    }
    // Compile Hardhat contracts (genesis task reads from contracts/out/hardhat/)
    let hardhat_out_dir = repo_root_dir.join("contracts").join("out").join("hardhat");
    if force || !hardhat_out_dir.exists() {
        let cmd = "npx hardhat --config hardhat.config.ts compile";
        shell::exec("bash", vec!["-c", cmd], repo_root_dir, None, false)?;
    } else {
        debug!("⏭️ Skipping Hardhat compile");
    }

    // Compile Forge contracts (ArtifactHelper.s.sol reads from contracts/out/forge/ to
    // compute CREATE2 addresses; stale artifacts produce wrong contract addresses in genesis)
    let forge_out_dir = repo_root_dir.join("contracts").join("out").join("forge");
    if force || !forge_out_dir.exists() {
        let cmd = "forge build";
        shell::exec("bash", vec!["-c", cmd], repo_root_dir, None, false)?;
    } else {
        debug!("⏭️ Skipping Forge compile");
    }

    Ok(())
}

/// Inputs to [`generate_genesis_file`].
///
/// Grouped into a struct because the underlying hardhat invocation has
/// accumulated enough knobs (paths, validator config, optional genesis
/// overrides) that a positional signature was getting hard to read at call
/// sites and easy to mis-order. All fields are inputs only — there is no
/// hidden state.
pub(crate) struct GenesisParams<'a> {
    pub repo_root_dir: &'a Path,
    pub genesis_file: &'a Path,
    pub num_extra_accounts: usize,
    pub public_keys_overrides: &'a IndexMap<usize, String>,
    pub validator_names: &'a [String],
    pub validator_voting_powers: Option<&'a [u64]>,
    pub force: bool,
    pub el_init_hardfork: Option<&'a str>,
    pub extra_account_balance_usdc: Option<u64>,
    pub block_gas_limit: Option<u64>,
}

/// Generate genesis file
pub(crate) fn generate_genesis_file(params: GenesisParams<'_>) -> Result<()> {
    let GenesisParams {
        repo_root_dir,
        genesis_file,
        num_extra_accounts,
        public_keys_overrides,
        validator_names,
        validator_voting_powers,
        force,
        el_init_hardfork,
        extra_account_balance_usdc,
        block_gas_limit,
    } = params;

    if !force && genesis_file.exists() {
        debug!("⏭️ Skipping generating and copying genesis file");
        return Ok(());
    }

    if validator_names.is_empty() {
        return Err(eyre!("validator_names must not be empty"));
    }

    let num_validators = validator_names.len();
    let num_overrides = public_keys_overrides.len();

    // Metadata to identify the genesis file for the given parameters
    let mut metadata =
        format!("val_{num_validators}-extra_{num_extra_accounts}-over_{num_overrides}");
    if let Some(bal) = extra_account_balance_usdc {
        metadata.push_str(&format!("-bal_{bal}"));
    }
    if let Some(gl) = block_gas_limit {
        metadata.push_str(&format!("-gas_{gl}"));
    }
    if let Some(el_init_hardfork) = el_init_hardfork {
        metadata.push_str(&format!("-hardfork_{el_init_hardfork}"));
    };

    // Include validator names in the cache file name because
    // controllers-config.json is keyed by validator name, but the underlying
    // controller data is generated by index. Example: if one run used
    // ["validator-blue", "validator-green"], it would cache a file named like
    // .quake/.cache/controllers-config-val_2-extra_100-over_2.json.
    //
    // If a later run (with the same num_validators/overrides) uses
    // ["validator-green", "validator-blue"], this function would look for a cached
    // file with the same name as before, that is
    // .quake/.cache/controllers-config-val_2-extra_100-over_2.json, and so it would
    // reuse that old cached file where validator names point to the wrong
    // controllers.
    //
    // That would make lookups by name update the wrong on-chain validator. Hashing
    // the names forces a new cache entry whenever the name set/order changes.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    validator_names.hash(&mut hasher);
    validator_voting_powers.hash(&mut hasher);
    let names_hash = hasher.finish();
    metadata.push_str(&format!("-vnames_{names_hash:x}"));

    // Generate genesis file if --force was given or if the genesis file doesn't
    // exist in the testnet directory
    let filename = format!("genesis-{metadata}.json");
    let quake_cache_dir = repo_root_dir.join(QUAKE_DIR).join(".cache");
    let cached_genesis_file = quake_cache_dir.join(&filename);

    let public_keys_overrides = public_keys_overrides
        .iter()
        .map(|(index, pk)| format!("{index}:{pk}"))
        .join(",");

    // Cache genesis file for the given parameters. For 100k extra prefunded
    // accounts, it takes about 10 minutes to generate.
    if force || !cached_genesis_file.exists() {
        let val_names_joined = validator_names.join(",");
        let mut cmd = format!(
            "npx hardhat \
                --config hardhat.config.ts genesis \
                --num-validators {num_validators} \
                --num-extra-accounts {num_extra_accounts} \
                --validator-names '{val_names_joined}' \
                --override-public-keys '{public_keys_overrides}' \
                --output-dir {} \
                --output-suffix '{metadata}'",
            &quake_cache_dir.display(),
        );
        if let Some(powers) = validator_voting_powers {
            let powers_joined = powers
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(",");
            cmd.push_str(&format!(" --voting-powers '{powers_joined}'"));
        }
        if let Some(el_init_hardfork) = el_init_hardfork {
            cmd.push_str(&format!(" --hardfork {el_init_hardfork}"));
        }
        if let Some(bal) = extra_account_balance_usdc {
            cmd.push_str(&format!(" --extra-account-balance {bal}"));
        }
        if let Some(gl) = block_gas_limit {
            cmd.push_str(&format!(" --block-gas-limit {gl}"));
        }
        shell::exec("bash", vec!["-c", cmd.as_str()], repo_root_dir, None, false)?;
        debug!(
            "✅ Generated genesis file for {num_validators} validators and {num_extra_accounts} extra prefunded accounts..."
        );
    } else {
        debug!(
            "⏭️ Using cached genesis file {} for {num_validators} validators and {num_extra_accounts} extra prefunded accounts",
            cached_genesis_file.display()
        );
    }

    // path to the .quake/<testnet_name>/assets/ dir
    // the genesis_file we pass as parameter is located at
    // .quake/<testnet_name>/assets/genesis.json
    let testnet_assets_dir = genesis_file.parent().unwrap();

    // the hardhat scripts create the config.json file and puts it in the .cache
    // folder
    let genesis_config_file = testnet_assets_dir.join("config.json");
    let cached_genesis_config_filename = format!("config-{metadata}.json");
    let cached_genesis_config_file = quake_cache_dir.join(cached_genesis_config_filename);

    // the hardhat scripts create the controllers-config.json file and puts
    // it in the .cache folder
    let controller_config_file = testnet_assets_dir.join("controllers-config.json");
    let cached_controllers_config_filename = format!("controllers-config-{metadata}.json");
    let cached_controllers_config_file = quake_cache_dir.join(cached_controllers_config_filename);

    // Copy all the cached files to testnet directory
    fs::copy(cached_genesis_file, genesis_file)?;
    fs::copy(cached_genesis_config_file, genesis_config_file)?;
    fs::copy(cached_controllers_config_file, controller_config_file)?;

    debug!("✅ Copied genesis file to {}", genesis_file.display());

    Ok(())
}

/// A Docker image build configuration for the arc_builders.yaml template.
#[derive(Serialize, Clone)]
pub(crate) struct ImageBuild {
    /// Service name in docker-compose (e.g., "arc_execution_build")
    pub service_name: String,
    /// Image tag to apply (e.g., "arc_execution:latest")
    pub tag: String,
}

#[derive(Serialize)]
pub(crate) struct ComposeTemplateDataLocal {
    /// Top-level `name` field in the compose file
    pub compose_project_name: String,
    /// Nodes in the testnet
    pub nodes: Vec<NodeMetadata>,
    /// Docker networks and their subnet addresses
    pub networks: Vec<ComposeTemplateSubnets>,
    /// External deployments directory with files we reuse in the testnets
    pub deployments_dir: String,
    /// Quake directory containing all testnet directories and files
    pub quake_dir: String,
    /// Resolved Docker images for the consensus and execution layers, and their upgrade versions
    pub images: testnet::DockerImages,
    /// Whether to enable RPC or use the default IPC connection between Reth and Malachite
    pub rpc: bool,
    /// Reth (EL) images to build locally
    pub reth_builds: Vec<ImageBuild>,
    /// Malachite (CL) images to build locally
    pub malachite_builds: Vec<ImageBuild>,
    /// Whether to enable latency emulation (mount latency_setup.sh in containers)
    pub latency_emulation: bool,
    /// Bind address for monitoring service ports (Prometheus, Grafana).
    /// When None, the template falls back to "127.0.0.1" via the {{default}} helper.
    pub monitoring_bind_host: Option<String>,
    /// For each node, a comma-separated list of enodes to add as trusted peers
    pub trusted_peers: IndexMap<NodeName, Option<String>>,
    /// CPU limit for the EL container (Docker `cpus`); when None, no limit is applied.
    pub el_cpu_limit: Option<f64>,
    /// Memory limit for the EL container in GiB; when None, no limit is applied.
    pub el_memory_limit_gb: Option<f64>,
    /// CPU limit for the CL container (Docker `cpus`); when None, no limit is applied.
    pub cl_cpu_limit: Option<f64>,
    /// Memory limit for the CL container in GiB; when None, no limit is applied.
    pub cl_memory_limit_gb: Option<f64>,
}

#[derive(Serialize)]
pub(crate) struct ComposeTemplateSubnets {
    pub name: SubnetName,
    pub subnet: CidrBlock,
}

/// Convert the map into a struct that can be used in a template
pub fn build_template_networks(
    subnet_cidr_map: &IndexMap<SubnetName, CidrBlock>,
) -> Vec<ComposeTemplateSubnets> {
    subnet_cidr_map
        .into_iter()
        .map(|(name, subnet)| ComposeTemplateSubnets {
            name: name.clone(),
            subnet: subnet.clone(),
        })
        .collect()
}

#[derive(Serialize)]
pub(crate) struct ComposeTemplateDataRemote {
    /// Top-level `name` field in the compose file, used as part of the network name
    pub compose_project_name: String,
    /// Consensus layer container name
    pub cl_container_name: String,
    /// Execution layer container name
    pub el_container_name: String,
    /// Manifest node name — used to locate the per-node latency_setup.sh on NFS.
    pub node_name: String,
    /// Whether latency emulation is enabled for the testnet. When true, the
    /// per-node `latency_setup.sh` is bind-mounted into both EL and CL containers
    /// at the path the entrypoint expects (`/usr/local/bin/latency_setup.sh`).
    pub latency_emulation: bool,
    /// Whether to enable RPC or use the default IPC connection between Reth and Malachite
    pub rpc: bool,
    /// Remote home directory
    pub remote_home_dir: String,
    /// Resolved Docker images for the consensus and execution layers, and their upgrade versions
    pub images: testnet::DockerImages,
    /// Consensus layer (Malachite) CLI flags for this node
    /// e.g., ["--moniker=cl", "--p2p.addr=/ip4/0.0.0.0/tcp/27000", "--p2p.persistent-peers=..."]
    pub cl_cli_flags: Vec<String>,
    /// Execution layer (Reth) CLI flags for this node
    /// e.g., ["--txpool.nolocals", "--disable-discovery"]
    pub el_cli_flags: Vec<String>,
    /// Comma-separated list of trusted peer enodes for this node
    pub trusted_peers: Option<String>,
    /// CPU limit for the EL container (Docker `cpus`); when None, no limit is applied.
    pub el_cpu_limit: Option<f64>,
    /// Memory limit for the EL container in GiB; when None, the legacy default applies.
    pub el_memory_limit_gb: Option<f64>,
    /// CPU limit for the CL container (Docker `cpus`); when None, no limit is applied.
    pub cl_cpu_limit: Option<f64>,
    /// Memory limit for the CL container in GiB; when None, the legacy default applies.
    pub cl_memory_limit_gb: Option<f64>,
    /// Environment variables for the execution layer (Reth) container.
    pub el_env: IndexMap<String, String>,
    /// Environment variables for the consensus layer (Malachite) container.
    pub cl_env: IndexMap<String, String>,
}

/// Generate docker compose content from the given template and data and write to the given path
pub(crate) fn generate_compose_file<T>(
    compose_path: &Path,
    template_data: &T,
    template: &str,
    force: bool,
) -> Result<()>
where
    T: Serialize,
{
    if !force && compose_path.exists() {
        debug!(path=%compose_path.display(), "⏭️ Skipping generating compose file");
        return Ok(());
    }

    // Set up handlebars for template rendering
    let mut handlebars = Handlebars::new();
    handlebars
        .register_template_string("compose", template)
        .context("Failed to register compose template")?;

    helpers::register(&mut handlebars);

    // Render template
    let compose_content = handlebars
        .render("compose", template_data)
        .context("Failed to render compose template")?;

    // Write compose file to testnet directory
    fs::write(compose_path, compose_content)
        .with_context(|| format!("Failed to write compose file: {}", compose_path.display()))?;

    debug!("✅ Generated compose file at {}", compose_path.display());
    Ok(())
}

/// Generate JWT secret for Engine API RPC and write to file
pub(crate) fn generate_jwt_secret(testnet_dir: &Path, force: bool) -> Result<()> {
    let jwt_secret_path = testnet_dir.join("assets").join("jwtsecret");
    let jwt_secret_path_str = jwt_secret_path.to_string_lossy().to_string();

    if !force && jwt_secret_path.exists() {
        debug!("⏭️ Skipping generating JWT secret file");
        return Ok(());
    }

    let secret = shell::exec_with_output("bash", vec!["-c", "openssl rand -hex 32"], testnet_dir)?;
    fs::write(jwt_secret_path, secret.trim())?;

    debug!("✅ Generated JWT secret file at {}", jwt_secret_path_str);
    Ok(())
}

/// Create EL data dirs and set directory permissions so containers (running as non-root user arc)
/// can write to mounted volumes. Required on Linux where bind-mount permissions are strict.
pub(crate) fn set_local_testnet_directory_permissions(
    testnet_dir: &Path,
    node_names: &[String],
) -> Result<()> {
    let logs_dir = testnet_dir.join("logs");
    for name in node_names {
        let reth_dir = testnet_dir.join(name).join("reth");
        fs::create_dir_all(&reth_dir)
            .with_context(|| format!("Failed to create directory: {}", reth_dir.display()))?;
        let sockets_dir = testnet_dir.join(name).join("sockets");
        fs::create_dir_all(&sockets_dir)
            .with_context(|| format!("Failed to create directory: {}", sockets_dir.display()))?;
    }
    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(0o777);
        fs::set_permissions(&logs_dir, perms.clone())
            .with_context(|| format!("Failed to set permissions on {}", logs_dir.display()))?;
        for name in node_names {
            let node_dir = testnet_dir.join(name);
            if node_dir.exists() {
                fs::set_permissions(&node_dir, perms.clone()).with_context(|| {
                    format!("Failed to set permissions on {}", node_dir.display())
                })?;
            }
            let sockets_dir = testnet_dir.join(name).join("sockets");
            if sockets_dir.exists() {
                fs::set_permissions(&sockets_dir, perms.clone()).with_context(|| {
                    format!("Failed to set permissions on {}", sockets_dir.display())
                })?;
            }
        }
    }
    Ok(())
}

pub(crate) fn generate_app_private_keys(
    testnet_dir: &Path,
    nodes_metadata: &NodesMetadata,
    force: bool,
) -> Result<()> {
    debug!("Generating Malachite app private keys...");

    let num_nodes = nodes_metadata.num_nodes();
    let private_keys = generate_private_keys(num_nodes, true)?;

    // Assign keys to validators first so their indices align with the genesis.
    // The genesis derives validator public keys from BIP39 indices 2..2+N_validators,
    // so the first N_validators private keys must go to validators in the same order
    // that the genesis file expects.
    let all_names = nodes_metadata.node_names();

    let (validator_names, non_validator_names): (Vec<&String>, Vec<&String>) = all_names
        .iter()
        .partition(|name| manifest::is_validator(name));

    let ordered_names: Vec<&String> = validator_names
        .into_iter()
        .chain(non_validator_names)
        .collect();

    for (i, name) in ordered_names.iter().enumerate() {
        let node_home_dir = testnet_dir.join(name).join("malachite");

        // Create the directory if it doesn't exist
        fs::create_dir_all(&node_home_dir)
            .with_context(|| format!("Failed to create directory: {}", node_home_dir.display()))?;

        let args = Args {
            home: Some(node_home_dir.clone()),
            ..Args::default()
        };

        let priv_key_file = args.get_default_priv_validator_key_file_path()?;

        // Skip if the private key already exists
        if !force && priv_key_file.exists() {
            debug!("⏭️ Skipping generating private key for node {name}");
            continue;
        }

        debug!(
            "Generating private key for node {name} at {} ...",
            priv_key_file.display()
        );

        // Save private key file
        let private_key = private_keys[i].clone();
        save_priv_validator_key(&priv_key_file, &private_key)?;
    }

    debug!(
        "✅ Generated Consensus Layer private keys at {}",
        testnet_dir.display()
    );
    Ok(())
}

/// Generate CLI flags for a node based on its configuration.
///
/// Builds a `StartCmd` from the manifest config + Node-level overrides +
/// deployment-specific fields, then calls `to_cli_flags()`.
///
/// `follow_endpoint_urls` are pre-resolved container-accessible EL RPC URLs for follow
/// mode (e.g. `http://validator-1_el:8545` for local, `http://10.0.0.5:8545` for remote).
pub(crate) fn generate_consensus_cli_flags(
    name: &str,
    node: Option<&manifest::Node>,
    listen_ip: &str,
    peers_ips: &[String],
    image_tag: Option<&str>,
    follow_endpoint_urls: &[String],
) -> Result<Vec<String>> {
    let Some(node) = node else {
        return generate_default_consensus_cli_flags(name, listen_ip, peers_ips, image_tag);
    };

    let transport = TransportProtocol::default();

    let mut cmd = node.cl_config.clone();

    cmd.moniker = Some(name.to_string());
    cmd.p2p_addr = transport.multiaddr(listen_ip, APP_CONSENSUS_DEFAULT_PORT);
    cmd.metrics = Some(
        format!("{listen_ip}:{APP_METRICS_DEFAULT_PORT}")
            .parse()
            .context("failed to parse metrics listen address")?,
    );
    cmd.rpc_addr = Some(
        format!("0.0.0.0:{APP_RPC_DEFAULT_PORT}")
            .parse()
            .context("failed to parse RPC listen address")?,
    );

    if !peers_ips.is_empty() {
        cmd.p2p_persistent_peers = peers_ips
            .iter()
            .map(|ip| transport.multiaddr(ip, APP_CONSENSUS_DEFAULT_PORT))
            .collect();
    }

    cmd.p2p_persistent_peers_only = node.cl_persistent_peers_only;
    cmd.gossipsub_explicit_peering = node.cl_gossipsub.explicit_peering;
    cmd.gossipsub_mesh_prioritization = node.cl_gossipsub.mesh_prioritization;
    cmd.gossipsub_load = node.cl_gossipsub.load.clone();

    if node.node_type == manifest::NodeType::Validator {
        cmd.validator = true;
    }

    // `--validator` requires a non-zero `--suggested-fee-recipient`. When
    // validator scenarios omit `cl_suggested_fee_recipient`, fall back to
    // QUAKE_DEFAULT_FEE_RECIPIENT, which is what localdev genesis expects
    // when `ProtocolConfig.rewardBeneficiary = 0`.
    let effective_fee_recipient = node.cl_suggested_fee_recipient.or_else(|| {
        (node.node_type == manifest::NodeType::Validator).then_some(QUAKE_DEFAULT_FEE_RECIPIENT)
    });
    if let Some(addr) = effective_fee_recipient {
        cmd.suggested_fee_recipient = Some(addr.into());
    }

    if node.remote_signer.is_some() {
        cmd.signing_remote = Some(format!(
            "http://{name}-signer-proxy:{REMOTE_SIGNER_PROXY_PORT}"
        ));
    }

    if cmd.prune_certificates_distance == 0 && cmd.prune_certificates_before == 0 {
        if let Some(preset) = node.cl_prune_preset {
            match preset {
                manifest::ClPruningPreset::Full => cmd.full = true,
                manifest::ClPruningPreset::Minimal => cmd.minimal = true,
            }
        }
    }

    if node.follow {
        cmd.follow = true;
        cmd.follow_endpoints = follow_endpoint_urls
            .iter()
            .map(|url| {
                url.parse()
                    .context(format!("invalid follow endpoint URL: {url}"))
            })
            .collect::<Result<Vec<_>>>()?;
    }

    let flags = cmd.to_cli_flags();
    validate_generated_cl_flags(&flags)?;
    Ok(apply_version_compat(flags, image_tag))
}

/// Generate default CLI flags when no node config is provided.
/// Used for nodes without manifest entries.
fn generate_default_consensus_cli_flags(
    name: &str,
    listen_ip: &str,
    peers_ips: &[String],
    image_tag: Option<&str>,
) -> Result<Vec<String>> {
    let transport = TransportProtocol::default();
    let cmd = StartCmd {
        moniker: Some(name.to_string()),
        p2p_addr: transport.multiaddr(listen_ip, APP_CONSENSUS_DEFAULT_PORT),
        metrics: Some(
            format!("{listen_ip}:{APP_METRICS_DEFAULT_PORT}")
                .parse()
                .context("failed to parse metrics listen address")?,
        ),
        rpc_addr: Some(
            format!("0.0.0.0:{APP_RPC_DEFAULT_PORT}")
                .parse()
                .context("failed to parse RPC listen address")?,
        ),
        // Use single-threaded runtime for lower resource usage when running local devnet.
        runtime_flavor: RUNTIME_SINGLE_THREADED.to_string(),
        p2p_persistent_peers: peers_ips
            .iter()
            .map(|ip| transport.multiaddr(ip, APP_CONSENSUS_DEFAULT_PORT))
            .collect(),
        ..StartCmd::default()
    };

    let flags = cmd.to_cli_flags();
    validate_generated_cl_flags(&flags)?;
    Ok(apply_version_compat(flags, image_tag))
}

/// Validate generated CL CLI flags by trial-parsing them against the actual
/// Args/StartCmd parser. Any flag accepted by the CL binary is automatically valid.
fn validate_generated_cl_flags(flags: &[String]) -> Result<()> {
    let trial_args = std::iter::once("arc-node-consensus")
        .chain(std::iter::once("start"))
        .chain(flags.iter().map(String::as_str));

    Args::try_parse_from(trial_args).map_err(|e| {
        eyre!(
            "Generated CL flags are invalid — a flag may be missing from StartCmd \
             or have an incompatible value: {e}"
        )
    })?;
    Ok(())
}

#[derive(Serialize)]
struct PrometheusTemplateData {
    nodes: Vec<NodeMetadata>,
}

/// Generate configuration file for Prometheus
pub(crate) fn generate_prometheus_config(path: &Path, nodes: Vec<NodeMetadata>) -> Result<()> {
    let template = include_str!("../templates/prometheus.yml.hbs");
    let template_data = PrometheusTemplateData { nodes };

    // Set up handlebars for template rendering
    let mut handlebars = Handlebars::new();
    handlebars
        .register_template_string("prometheus", template)
        .context("Failed to register Prometheus template")?;

    // Render template
    let content = handlebars
        .render("prometheus", &template_data)
        .context("Failed to render Prometheus template")?;

    // Overwrite any existing file
    fs::write(path, content)
        .with_context(|| format!("Failed to write Prometheus config file: {}", path.display()))?;

    debug!("✅ Generated Prometheus config file at {}", path.display());
    Ok(())
}

/// Generate file with all node metadata.
///
/// For remote testnets the file is consumed on the Control Center, so URLs
/// are rewritten from SSM tunnel addresses to each node's first private IP
/// with standard service ports (CC-side tools connect directly to nodes).
pub(crate) fn generate_nodes_metadata_file(
    path: &Path,
    nodes_metadata: &NodesMetadata,
    infra_type: InfraType,
    force: bool,
) -> Result<()> {
    if !force && path.exists() {
        debug!("⏭️ Skipping generating node metadata file");
        return Ok(());
    }

    let content = match infra_type {
        InfraType::Remote => nodes_metadata.serialize_for_cc()?,
        InfraType::Local => serde_json::to_string_pretty(&nodes_metadata.values())?,
    };
    fs::write(path, content).with_context(|| format!("Failed to write {path:?}"))?;

    debug!("✅ Generated file with node metadata: {path:?}");
    Ok(())
}

mod helpers {
    use handlebars::{handlebars_helper, Handlebars};

    // Increments a number by 1, used in templates to convert 0-based indices to 1-based
    handlebars_helper!(inc: |x: i64| x + 1);

    // Returns the value if present and non-empty, otherwise the fallback.
    // Usage: {{default variable "fallback_value"}}
    handlebars_helper!(default: |val: Json, fallback: Json| {
        if val.is_null() || val.as_str().is_some_and(str::is_empty) {
            fallback.clone()
        } else {
            val.clone()
        }
    });

    // Escape a string for safe inclusion inside a YAML double-quoted scalar.
    // Required for CLI flag values that may contain `"` (e.g. --byzantine=<JSON>).
    // Handles `\`, `"`, and every C0 control char + DEL — a literal newline
    // in a YAML scalar would either break the document or silently swallow
    // the rest of the line. Callers (env vars, CLI flags) should normally
    // be passing single-line scalars; this is defense-in-depth.
    pub(super) fn yaml_dq_escape_str(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\0' => out.push_str("\\0"),
                c if c.is_control() => {
                    let n = c as u32;
                    if n <= 0xFF {
                        out.push_str(&format!("\\x{n:02X}"));
                    } else if n <= 0xFFFF {
                        out.push_str(&format!("\\u{n:04X}"));
                    } else {
                        out.push_str(&format!("\\U{n:08X}"));
                    }
                }
                _ => out.push(c),
            }
        }
        out
    }
    handlebars_helper!(yaml_dq_escape: |s: str| yaml_dq_escape_str(s));

    // Whether `map` is an object containing `key`. Unlike `lookup`, this tests key
    // presence rather than value truthiness, so an explicit empty-string value still
    // counts as present (used to suppress a built-in env var the user overrides).
    handlebars_helper!(contains_key: |map: Json, key: str| {
        map.as_object().is_some_and(|m| m.contains_key(key))
    });

    pub fn register(handlebars: &mut Handlebars) {
        handlebars.register_helper("inc", Box::new(inc));
        handlebars.register_helper("default", Box::new(default));
        handlebars.register_helper("yaml_dq_escape", Box::new(yaml_dq_escape));
        handlebars.register_helper("contains_key", Box::new(contains_key));
    }
}

/// Generate nodekeys for Reth P2P identity and write them to disk.
pub(crate) fn generate_nodekeys(
    nodes_metadata: &NodesMetadata,
    testnet_dir: &Path,
    force: bool,
) -> Result<IndexMap<NodeName, NodekeyData>> {
    let node_names = nodes_metadata.node_names();
    let nodekeys = nodekey::load_or_generate_nodekeys(&node_names, testnet_dir, force)?;

    // Always write nodekeys to disk to ensure in-memory state matches what Reth will use at startup.
    nodekey::write_nodekey_files(testnet_dir, &nodekeys, force)?;
    Ok(nodekeys)
}

/// Build the trusted_peers map for compose templates: for each node, a
/// comma-separated list of enodes of all other nodes it should peer with.
///
/// When `el_trusted_peers_per_node` is `Some`, a node whose entry is
/// `Some(non-empty Vec)` uses only those named peers (filtered to those that
/// share an EL subnet). Any node whose entry is `None`, or whose explicit list
/// is empty, falls back to full-mesh. When `el_trusted_peers_per_node` is
/// `None`, all nodes use full-mesh.
///
/// Connections are tracked globally: explicit-peer connections are registered
/// in the same deduplication table used by full-mesh nodes, preventing full-mesh
/// from re-adding a pair already configured by an explicit list (A→B but not B→A).
/// Two nodes that both list each other explicitly will each emit the other in their
/// `--trusted-peers` — Reth handles duplicate trusted peers gracefully.
/// In both modes, only nodes that share an EL subnet are peered — nodes on
/// disjoint subnets cannot reach each other directly and will route through bridge nodes.
pub(crate) fn build_trusted_peers_map(
    nodekeys: &IndexMap<NodeName, NodekeyData>,
    el_trusted_peers_per_node: Option<&IndexMap<NodeName, Option<Vec<NodeName>>>>,
    nodes_metadata: &NodesMetadata,
    subnets: &Subnets,
) -> Result<IndexMap<NodeName, Option<String>>> {
    let mut nodes_to_connect: IndexMap<NodeName, IndexSet<NodeName>> = IndexMap::new();
    let mut trusted_peers: IndexMap<NodeName, Option<String>> = IndexMap::new();

    for node in nodekeys.keys() {
        // If this node has an explicit el_trusted_peers list, use those enodes directly.
        if let Some(Some(explicit_peers)) = el_trusted_peers_per_node.and_then(|m| m.get(node)) {
            if !explicit_peers.is_empty() {
                let mut enodes: Vec<String> = Vec::new();
                for peer_name in explicit_peers {
                    let Some(peer_data) = nodekeys.get(peer_name) else {
                        warn!(
                            node = %node,
                            peer = %peer_name,
                            "el_trusted_peers entry not found in nodekeys; skipping"
                        );
                        continue;
                    };
                    let Some(subnet) = subnets.shared_subnets(node, peer_name).into_iter().next()
                    else {
                        warn!(
                            node = %node,
                            peer = %peer_name,
                            "el_trusted_peers: no shared EL subnet with peer; skipping"
                        );
                        continue;
                    };
                    let Some(peer_ip) = nodes_metadata.shared_el_subnet_ip(&subnet, peer_name)
                    else {
                        continue;
                    };
                    enodes.push(peer_data.enode_for_ip(&peer_ip)?);
                    // Register this connection so full-mesh nodes later in the iteration
                    // don't emit a duplicate connection in the other direction.
                    nodes_to_connect
                        .entry(node.clone())
                        .or_default()
                        .insert(peer_name.clone());
                    nodes_to_connect
                        .entry(peer_name.clone())
                        .or_default()
                        .insert(node.clone());
                }
                trusted_peers.insert(
                    node.clone(),
                    if enodes.is_empty() {
                        None
                    } else {
                        Some(enodes.join(","))
                    },
                );
                continue;
            }
        }

        // Full-mesh fallback: peer with all nodes that share an EL subnet,
        // connecting each pair only once (A→B but not B→A).
        let mut peer_enodes: Vec<nodekey::Enode> = Vec::new();

        for (peer_name, peer_data) in nodekeys.iter() {
            if peer_name == node {
                continue;
            }
            // Skip if already connected in the other direction (B→A already registered)
            if nodes_to_connect
                .get(node)
                .is_some_and(|p| p.contains(peer_name))
            {
                continue;
            }

            // Only peer with nodes that share an EL subnet
            let Some(first_shared_subnet) =
                subnets.shared_subnets(node, peer_name).into_iter().next()
            else {
                continue;
            };

            let Some(peer_ip) = nodes_metadata.shared_el_subnet_ip(&first_shared_subnet, peer_name)
            else {
                continue;
            };

            peer_enodes.push(peer_data.enode_for_ip(&peer_ip)?);

            nodes_to_connect
                .entry(node.clone())
                .or_default()
                .insert(peer_name.clone());
            nodes_to_connect
                .entry(peer_name.clone())
                .or_default()
                .insert(node.clone());
        }

        let peer_enodes_optional = if peer_enodes.is_empty() {
            None
        } else {
            Some(peer_enodes.join(","))
        };
        trusted_peers.insert(node.clone(), peer_enodes_optional);
    }
    debug!(?nodes_to_connect, ?trusted_peers, "Nodes to connect");

    Ok(trusted_peers)
}

/// Rewrites `--rpc.forwarder=http://{peer}_el:port` for remote deployments.
///
/// Local Docker Compose resolves `{name}_el` hostnames on a single machine. Remote
/// mode runs one compose stack per EC2 instance, so those names must be replaced
/// with the peer EL's VPC IP on a subnet shared with this node (same idea as
/// [`NodesMetadata::shared_el_subnet_ip`] for trusted peers).
pub(crate) fn rewrite_rpc_forwarder_for_remote(
    el_cli_flags: &mut [String],
    node_name: &str,
    nodes_metadata: &NodesMetadata,
    subnets: &Subnets,
) {
    for flag in el_cli_flags.iter_mut() {
        let Some(rest) = flag.strip_prefix("--rpc.forwarder=") else {
            continue;
        };
        let Ok(parsed) = Url::parse(rest) else {
            continue;
        };
        let Some(host) = parsed.host_str() else {
            continue;
        };
        let Some(peer_name) = host.strip_suffix("_el") else {
            continue;
        };
        let Some(md) = nodes_metadata.nodes.get(peer_name) else {
            warn!(
                node = %node_name,
                peer = %peer_name,
                "rpc.forwarder: peer node not in testnet metadata; leaving flag unchanged"
            );
            continue;
        };
        let shared = subnets.shared_subnets(node_name, peer_name);
        let ip = shared
            .first()
            .and_then(|s| md.execution.private_ip_address_for(s))
            .unwrap_or_else(|| {
                warn!(
                    node = %node_name,
                    peer = %peer_name,
                    "rpc.forwarder: no shared subnet with peer; falling back to first private IP \
                     which may not be routable"
                );
                md.execution.first_private_ip().clone()
            });
        let port = parsed.port().unwrap_or(RETH_HTTP_BASE_PORT as u16);
        *flag = format!("--rpc.forwarder=http://{ip}:{port}");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::infra::InfraData;
    use crate::manifest::Manifest;
    use crate::nodes::NodesMetadata;
    use indexmap::IndexMap;
    use tempfile::tempdir;

    fn create_test_nodes_metadata(num_nodes: usize) -> NodesMetadata {
        create_test_nodes_metadata_with_subnets(
            (0..num_nodes)
                .map(|i| (format!("node-{}", i), manifest::default_subnet_singleton()))
                .collect::<IndexMap<_, _>>(),
        )
    }

    fn create_test_nodes_metadata_with_subnets(
        node_subnets: IndexMap<String, Vec<String>>,
    ) -> NodesMetadata {
        let mut manifest_nodes = IndexMap::new();
        for (name, _) in node_subnets.iter() {
            manifest_nodes.insert(name.clone(), manifest::Node::default());
        }
        let infra_data = InfraData::new_local("testnet".to_string(), &manifest_nodes);
        let manifest = Manifest::new(Some("testnet".to_string()), &manifest_nodes, &node_subnets);
        NodesMetadata::new(
            infra_data,
            &manifest,
            &manifest.images.to_local().unwrap(),
            &BTreeSet::new(),
        )
        .unwrap()
    }

    fn create_test_nodekeys(num_nodes: usize) -> (IndexMap<NodeName, NodekeyData>, NodesMetadata) {
        let dir = tempdir().unwrap();
        let nodes_metadata = create_test_nodes_metadata(num_nodes);
        let node_names = nodes_metadata.node_names();
        let nodekeys = nodekey::load_or_generate_nodekeys(&node_names, dir.path(), false).unwrap();
        (nodekeys, nodes_metadata)
    }

    /// Render the remote node compose template with the given per-layer env maps.
    fn render_remote_compose_env(el_env: serde_json::Value, cl_env: serde_json::Value) -> String {
        let mut handlebars = handlebars::Handlebars::new();
        handlebars
            .register_template_string(
                "compose",
                include_str!("../templates/remote/compose-node.yaml.hbs"),
            )
            .unwrap();
        helpers::register(&mut handlebars);

        let ctx = serde_json::json!({
            "el_container_name": "node_el",
            "cl_container_name": "node_cl",
            "el_env": el_env,
            "cl_env": cl_env,
        });
        handlebars.render("compose", &ctx).unwrap()
    }

    /// A per-node env var that collides with a built-in replaces it (no duplicate
    /// YAML key), while non-colliding vars are added alongside the built-ins.
    #[test]
    fn test_compose_env_override_suppresses_builtin() {
        let rendered = render_remote_compose_env(
            serde_json::json!({ "PATH": "/custom/path", "FEATURE_X": "on" }),
            serde_json::json!({ "ARC_GENESIS_FILE_PATH": "/custom/genesis.json", "CL_CUSTOM": "1" }),
        );

        // EL: PATH override replaces the built-in default; new var is added.
        assert!(rendered.contains(r#"PATH: "/custom/path""#));
        assert!(!rendered.contains("PATH: /usr/local/sbin"));
        assert!(rendered.contains(r#"FEATURE_X: "on""#));

        // CL: ARC_GENESIS_FILE_PATH override replaces the built-in default.
        assert!(rendered.contains(r#"ARC_GENESIS_FILE_PATH: "/custom/genesis.json""#));
        assert!(!rendered.contains("ARC_GENESIS_FILE_PATH: /assets/genesis.json"));
        assert!(rendered.contains(r#"CL_CUSTOM: "1""#));
    }

    /// Env vars that do not collide with built-ins leave the built-ins in place.
    #[test]
    fn test_compose_env_keeps_builtins_when_no_collision() {
        let rendered =
            render_remote_compose_env(serde_json::json!({ "FOO": "bar" }), serde_json::json!({}));

        assert!(rendered.contains("PATH: /usr/local/sbin"));
        assert!(rendered.contains("ARC_GENESIS_FILE_PATH: /assets/genesis.json"));
        assert!(rendered.contains(r#"FOO: "bar""#));
    }

    /// Env values are emitted inside a double-quoted YAML scalar with `"` escaped.
    #[test]
    fn test_compose_env_value_is_yaml_escaped() {
        let rendered = render_remote_compose_env(
            serde_json::json!({ "JSON_CFG": r#"{"a":1}"# }),
            serde_json::json!({}),
        );

        assert!(rendered.contains(r#"JSON_CFG: "{\"a\":1}""#));
    }

    /// `yaml_dq_escape` escapes every C0 control character so a stray newline
    /// can't break the rendered compose YAML. `env_table_to_map` rejects these
    /// at manifest-load time, but the helper is also used for CLI flags, so it
    /// must defend itself too.
    #[test]
    fn test_yaml_dq_escape_handles_control_chars() {
        let escaped = helpers::yaml_dq_escape_str("line1\nline2\tcol\rend\0\x07");
        assert_eq!(escaped, r#"line1\nline2\tcol\rend\0\x07"#);
        // Backslash and quote still escape correctly when mixed with controls.
        assert_eq!(helpers::yaml_dq_escape_str("a\\b\"c\n"), r#"a\\b\"c\n"#);
    }

    /// An explicit empty-string override still counts as "present" and suppresses the
    /// built-in, so no duplicate YAML key is emitted (key-presence, not truthiness).
    #[test]
    fn test_compose_env_empty_string_override_suppresses_builtin() {
        let rendered =
            render_remote_compose_env(serde_json::json!({ "PATH": "" }), serde_json::json!({}));

        assert!(!rendered.contains("PATH: /usr/local/sbin"));
        assert!(rendered.contains(r#"PATH: """#));
    }

    /// The local template resolves `el_env`/`cl_env` per node inside `{{#each nodes}}`;
    /// overrides still replace the matching built-in for that node's containers.
    #[test]
    fn test_local_compose_env_per_node_override() {
        let mut handlebars = handlebars::Handlebars::new();
        handlebars
            .register_template_string(
                "compose",
                include_str!("../templates/local/compose.yaml.hbs"),
            )
            .unwrap();
        helpers::register(&mut handlebars);

        let ctx = serde_json::json!({
            "nodes": [{
                "el_env": { "RUST_LOG": "trace", "FEATURE_X": "on" },
                "cl_env": { "ARC_HALT_AT_BLOCK_HEIGHT": "5", "CL_CUSTOM": "1" },
            }],
        });
        let rendered = handlebars.render("compose", &ctx).unwrap();

        // EL: RUST_LOG override replaces the built-in default; new var added.
        assert!(rendered.contains(r#"RUST_LOG: "trace""#));
        assert!(!rendered.contains("RUST_LOG: debug,net::peers=trace"));
        assert!(rendered.contains(r#"FEATURE_X: "on""#));

        // CL: ARC_HALT_AT_BLOCK_HEIGHT override replaces the built-in default.
        assert!(rendered.contains(r#"ARC_HALT_AT_BLOCK_HEIGHT: "5""#));
        assert!(!rendered.contains("ARC_HALT_AT_BLOCK_HEIGHT: 0"));
        assert!(rendered.contains(r#"CL_CUSTOM: "1""#));
    }

    fn assert_peer_count(
        trusted_peers: &IndexMap<NodeName, Option<String>>,
        node: &str,
        expected: usize,
    ) {
        let entry = trusted_peers
            .get(node)
            .unwrap_or_else(|| panic!("{node} not found in trusted_peers"));
        let actual = entry.as_ref().map_or(0, |s| s.split(',').count());
        assert_eq!(
            actual, expected,
            "{node}: expected {expected} peers, got {actual}"
        );
    }

    #[test]
    fn generate_app_private_keys_creates_correct_number() {
        let dir = tempdir().unwrap();
        let nodes_metadata = create_test_nodes_metadata(3);

        let result = generate_app_private_keys(dir.path(), &nodes_metadata, false);
        assert!(result.is_ok());

        // Verify 3 private key files were created
        for i in 0..3 {
            let key_file = dir
                .path()
                .join(format!("node-{}", i))
                .join("malachite")
                .join("config")
                .join("priv_validator_key.json");
            assert!(key_file.exists());
        }
    }

    #[test]
    fn generate_app_private_keys_with_force_overwrites() {
        let dir = tempdir().unwrap();
        let nodes_metadata = create_test_nodes_metadata(1);

        // Generate initial keys
        generate_app_private_keys(dir.path(), &nodes_metadata, false).unwrap();
        let key_file = dir
            .path()
            .join("node-0")
            .join("malachite")
            .join("config")
            .join("priv_validator_key.json");
        let original_contents = fs::read_to_string(&key_file).unwrap();

        // Generate again with force
        generate_app_private_keys(dir.path(), &nodes_metadata, true).unwrap();
        let new_contents = fs::read_to_string(&key_file).unwrap();

        // Keys are deterministic (same seed), so they will be the same
        // But the important thing is that the file was overwritten
        assert_eq!(original_contents, new_contents);
        assert!(key_file.exists());
    }

    #[test]
    fn generate_app_private_keys_without_force_skips_existing() {
        let dir = tempdir().unwrap();
        let nodes_metadata = create_test_nodes_metadata(1);

        // Generate initial keys
        generate_app_private_keys(dir.path(), &nodes_metadata, false).unwrap();
        let key_file = dir
            .path()
            .join("node-0")
            .join("malachite")
            .join("config")
            .join("priv_validator_key.json");
        let original_contents = fs::read_to_string(&key_file).unwrap();

        // Generate again without force
        generate_app_private_keys(dir.path(), &nodes_metadata, false).unwrap();
        let contents_after = fs::read_to_string(&key_file).unwrap();

        // Contents should be unchanged
        assert_eq!(original_contents, contents_after);
    }

    #[test]
    fn generate_app_private_keys_creates_directories() {
        let dir = tempdir().unwrap();
        let nodes_metadata = create_test_nodes_metadata(2);

        let result = generate_app_private_keys(dir.path(), &nodes_metadata, false);
        assert!(result.is_ok());

        // Verify directories were created
        assert!(dir.path().join("node-0").join("malachite").exists());
        assert!(dir.path().join("node-1").join("malachite").exists());
    }

    /// Regression test: when non-validator nodes appear between validators in
    /// manifest order (e.g. val1, val2, sentry-1, val3), validators must still
    /// receive BIP39 keys at indices 0..N_validators so their public keys match
    /// what the genesis file expects.
    #[test]
    fn generate_app_private_keys_validators_get_first_indices() {
        let dir = tempdir().unwrap();

        // Create nodes in an order where a non-validator sits between validators,
        // mimicking the mainnet-small topology that triggered the bug.
        let node_subnets: IndexMap<String, Vec<String>> = [
            ("validator1", vec!["default".into()]),
            ("validator2", vec!["default".into()]),
            ("sentry-1", vec!["default".into()]),
            ("validator3", vec!["default".into()]),
        ]
        .into_iter()
        .map(|(name, subnets)| (name.to_string(), subnets))
        .collect();

        let nodes_metadata = create_test_nodes_metadata_with_subnets(node_subnets);

        generate_app_private_keys(dir.path(), &nodes_metadata, false).unwrap();

        // Generate a second copy into a flat directory so we get reference key files
        // at known indices. We reuse the same deterministic derivation.
        let ref_dir = tempdir().unwrap();
        let ref_subnets: IndexMap<String, Vec<String>> = (0..4)
            .map(|i| (format!("ref-{i}"), vec!["default".into()]))
            .collect();
        let ref_metadata = create_test_nodes_metadata_with_subnets(ref_subnets);
        generate_app_private_keys(ref_dir.path(), &ref_metadata, false).unwrap();

        let read_key = |base: &Path, node_name: &str| -> String {
            let key_file = base
                .join(node_name)
                .join("malachite")
                .join("config")
                .join("priv_validator_key.json");
            fs::read_to_string(&key_file).unwrap()
        };

        // Validators get keys at indices 0, 1, 2 (in validator_names order)
        assert_eq!(
            read_key(dir.path(), "validator1"),
            read_key(ref_dir.path(), "ref-0"),
            "validator1 should get key[0]"
        );
        assert_eq!(
            read_key(dir.path(), "validator2"),
            read_key(ref_dir.path(), "ref-1"),
            "validator2 should get key[1]"
        );
        assert_eq!(
            read_key(dir.path(), "validator3"),
            read_key(ref_dir.path(), "ref-2"),
            "validator3 should get key[2]"
        );

        // Non-validator gets the remaining key at index 3
        assert_eq!(
            read_key(dir.path(), "sentry-1"),
            read_key(ref_dir.path(), "ref-3"),
            "sentry-1 should get key[3]"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_includes_required_flags() {
        let flags = generate_consensus_cli_flags("validator-1", None, "172.19.0.5", &[], None, &[])
            .unwrap();

        let flags_str = flags.join(" ");
        assert!(flags_str.contains("--moniker"));
        assert!(flags_str.contains("validator-1"));
        assert!(flags_str.contains("--p2p.addr"));
        assert!(flags_str.contains("172.19.0.5"));
    }

    #[test]
    fn generate_consensus_cli_flags_with_cl_persistent_peers() {
        let peers = vec!["172.19.0.6".to_string(), "172.19.0.7".to_string()];
        let flags =
            generate_consensus_cli_flags("validator-1", None, "172.19.0.5", &peers, None, &[])
                .unwrap();

        let flags_str = flags.join(" ");
        assert!(flags_str.contains("--p2p.persistent-peers"));
        assert!(flags_str.contains("172.19.0.6"));
        assert!(flags_str.contains("172.19.0.7"));
    }

    #[test]
    fn generate_consensus_cli_flags_without_persistent_peers() {
        let flags = generate_consensus_cli_flags("validator-1", None, "172.19.0.5", &[], None, &[])
            .unwrap();

        let flags_str = flags.join(" ");
        // Should not contain persistent peers flag when empty
        assert!(!flags_str.contains("--p2p.persistent-peers"));
    }

    #[test]
    fn generate_consensus_cli_flags_includes_metrics() {
        let flags = generate_consensus_cli_flags("validator-1", None, "172.19.0.5", &[], None, &[])
            .unwrap();

        let flags_str = flags.join(" ");
        assert!(flags_str.contains("--metrics"));
        assert!(flags_str.contains(&format!("172.19.0.5:{}", APP_METRICS_DEFAULT_PORT)));
    }

    #[test]
    fn generate_consensus_cli_flags_includes_rpc() {
        let flags = generate_consensus_cli_flags("validator-1", None, "172.19.0.5", &[], None, &[])
            .unwrap();

        let flags_str = flags.join(" ");
        assert!(flags_str.contains("--rpc.addr"));
        assert!(flags_str.contains(&format!("0.0.0.0:{}", APP_RPC_DEFAULT_PORT)));
    }

    #[test]
    fn generate_consensus_cli_flags_omits_default_value_sync() {
        // value_sync defaults to true in StartCmd, so the flag is not emitted
        // (the binary uses it by default). Only emitted when explicitly disabled.
        let flags = generate_consensus_cli_flags("validator-1", None, "172.19.0.5", &[], None, &[])
            .unwrap();

        let flags_str = flags.join(" ");
        assert!(!flags_str.contains("--value-sync"));
    }

    #[test]
    fn generate_consensus_cli_flags_emits_suggested_fee_recipient() {
        use alloy_primitives::address;
        let recipient = address!("0x98e503f35D0a019cB0a251aD243a4cCFCF371F46");
        let node = manifest::Node {
            cl_suggested_fee_recipient: Some(recipient),
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ").to_lowercase();
        assert!(
            flags_str
                .contains("--suggested-fee-recipient=0x98e503f35d0a019cb0a251ad243a4ccfcf371f46"),
            "missing suggested-fee-recipient: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_omits_suggested_fee_recipient_when_none() {
        let flags = generate_consensus_cli_flags("validator-1", None, "172.19.0.5", &[], None, &[])
            .unwrap();
        let flags_str = flags.join(" ");
        assert!(!flags_str.contains("--suggested-fee-recipient"));
    }

    #[test]
    fn generate_consensus_cli_flags_falls_back_to_default_for_validator_without_recipient() {
        let node = manifest::Node {
            node_type: manifest::NodeType::Validator,
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("val-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ").to_lowercase();
        let expected =
            format!("--suggested-fee-recipient={QUAKE_DEFAULT_FEE_RECIPIENT}").to_lowercase();
        assert!(
            flags_str.contains(&expected),
            "expected default fee recipient fallback for validator: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_no_fallback_for_non_validator_without_recipient() {
        let node = manifest::Node {
            node_type: manifest::NodeType::NonValidator,
            ..Default::default()
        };
        let flags = generate_consensus_cli_flags("fn-1", Some(&node), "172.19.0.5", &[], None, &[])
            .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            !flags_str.contains("--suggested-fee-recipient"),
            "should not emit flag for non-validator without explicit recipient: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_with_remote_signer() {
        let node = manifest::Node {
            node_type: manifest::NodeType::Validator,
            remote_signer: Some(manifest::RemoteKeyId::new(1).unwrap()),
            ..Default::default()
        };

        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();

        let flags_str = flags.join(" ");
        assert!(flags_str.contains("--signing.remote"));
        assert!(flags_str.contains("validator-1-signer-proxy"));
    }

    #[test]
    fn generate_consensus_cli_flags_without_remote_signer() {
        let node = manifest::Node::default();
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(!flags_str.contains("--signing.remote"));
    }

    #[test]
    fn generate_consensus_cli_flags_with_persistent_peers_only() {
        let node = manifest::Node {
            cl_persistent_peers_only: true,
            ..Default::default()
        };
        let peers = vec!["172.19.0.6".to_string()];
        let flags = generate_consensus_cli_flags(
            "validator-1",
            Some(&node),
            "172.19.0.5",
            &peers,
            None,
            &[],
        )
        .unwrap();
        let flags_str = flags.join(" ");
        assert!(flags_str.contains("--p2p.persistent-peers-only"));
    }

    #[test]
    fn generate_consensus_cli_flags_without_persistent_peers_only() {
        let node = manifest::Node::default();
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(!flags_str.contains("--p2p.persistent-peers-only"));
    }

    #[test]
    fn generate_consensus_cli_flags_with_gossipsub_explicit_peering() {
        let node = manifest::Node {
            cl_gossipsub: manifest::ClGossipSubConfig {
                explicit_peering: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(flags_str.contains("--gossipsub.explicit-peering"));
    }

    #[test]
    fn generate_consensus_cli_flags_with_gossipsub_mesh_prioritization() {
        let node = manifest::Node {
            cl_gossipsub: manifest::ClGossipSubConfig {
                mesh_prioritization: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(flags_str.contains("--gossipsub.mesh-prioritization"));
    }

    #[test]
    fn generate_consensus_cli_flags_with_gossipsub_load() {
        let node = manifest::Node {
            cl_gossipsub: manifest::ClGossipSubConfig {
                load: Some("high".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(flags_str.contains("--gossipsub.load=high"));
    }

    #[test]
    fn generate_consensus_cli_flags_without_gossipsub_overrides() {
        let node = manifest::Node::default();
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(!flags_str.contains("--gossipsub.explicit-peering"));
        assert!(!flags_str.contains("--gossipsub.mesh-prioritization"));
        assert!(!flags_str.contains("--gossipsub.load"));
    }

    #[test]
    fn build_trusted_peers_map_returns_empty_for_empty_nodekeys() {
        let nodekeys = IndexMap::new();
        let nodes_metadata = NodesMetadata::default();
        let topology = Subnets::default();
        let trusted_peers =
            build_trusted_peers_map(&nodekeys, None, &nodes_metadata, &topology).unwrap();
        assert!(trusted_peers.is_empty());
    }

    #[test]
    fn build_trusted_peers_map_single_node_has_no_peers() {
        let (nodekeys, nodes_metadata) = create_test_nodekeys(1);
        let trusted_peers =
            build_trusted_peers_map(&nodekeys, None, &nodes_metadata, &nodes_metadata.subnets)
                .unwrap();
        assert_eq!(trusted_peers.len(), 1);
        assert_eq!(trusted_peers.get("node-0"), Some(&None));
    }

    #[test]
    fn build_trusted_peers_map_connects_each_pair_exactly_once() {
        let (nodekeys, nodes_metadata) = create_test_nodekeys(4);
        let trusted_peers =
            build_trusted_peers_map(&nodekeys, None, &nodes_metadata, &nodes_metadata.subnets)
                .unwrap();
        assert_eq!(trusted_peers.len(), 4);

        // Each pair connected exactly once (A→B, not B→A).
        // With 4 nodes on the same subnet there are C(4,2) = 6 unique pairs.
        // node-0 peers: 1,2,3  → 3
        // node-1 peers: 2,3    → 2  (0↔1 already counted)
        // node-2 peers: 3      → 1  (0↔2, 1↔2 already counted)
        // node-3 peers: none   → 0  (all pairs already counted)
        assert_peer_count(&trusted_peers, "node-0", 3);
        assert_peer_count(&trusted_peers, "node-1", 2);
        assert_peer_count(&trusted_peers, "node-2", 1);
        assert_peer_count(&trusted_peers, "node-3", 0);

        let total: usize = trusted_peers
            .values()
            .map(|v| v.as_ref().map_or(0, |s| s.split(',').count()))
            .sum();
        assert_eq!(total, 6);
    }

    #[test]
    fn build_trusted_peers_map_only_peers_nodes_on_shared_subnets() {
        // node-0 on subnet A only; node-1 bridges A and B; node-2 on subnet B only
        let nodes_metadata = create_test_nodes_metadata_with_subnets(IndexMap::from([
            ("node-0".to_string(), vec!["A".to_string()]),
            ("node-1".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node-2".to_string(), vec!["B".to_string()]),
        ]));
        let dir = tempdir().unwrap();
        let node_names = nodes_metadata.node_names();
        let nodekeys = nodekey::load_or_generate_nodekeys(&node_names, dir.path(), false).unwrap();

        let trusted_peers =
            build_trusted_peers_map(&nodekeys, None, &nodes_metadata, &nodes_metadata.subnets)
                .unwrap();

        // node-0 can peer with node-1 (shared subnet A), but NOT node-2 (no shared subnet)
        assert_peer_count(&trusted_peers, "node-0", 1);
        // node-1 can peer with node-2 (shared subnet B); node-0 already connected
        assert_peer_count(&trusted_peers, "node-1", 1);
        // node-2 has no new peers (node-1 already connected from the other direction)
        assert_peer_count(&trusted_peers, "node-2", 0);
    }

    #[test]
    fn build_trusted_peers_map_uses_el_trusted_peers_when_provided() {
        let (nodekeys, nodes_metadata) = create_test_nodekeys(4);
        let mut el_trusted_peers_per_node = IndexMap::new();
        el_trusted_peers_per_node.insert(
            "node-0".to_string(),
            Some(vec!["node-1".to_string(), "node-3".to_string()]),
        );
        el_trusted_peers_per_node.insert("node-1".to_string(), None);
        el_trusted_peers_per_node.insert("node-2".to_string(), Some(vec![]));
        el_trusted_peers_per_node.insert("node-3".to_string(), Some(vec!["node-0".to_string()]));

        let trusted_peers = build_trusted_peers_map(
            &nodekeys,
            Some(&el_trusted_peers_per_node),
            &nodes_metadata,
            &nodes_metadata.subnets,
        )
        .unwrap();

        // node-0: explicit [node-1, node-3] → 2 peers
        assert_peer_count(&trusted_peers, "node-0", 2);
        // node-3: explicit [node-0] → 1 peer
        assert_peer_count(&trusted_peers, "node-3", 1);
        // node-1: full-mesh fallback; node-0↔node-1 already registered by node-0's explicit path,
        // so only node-2 and node-3 are new → 2 peers
        assert_peer_count(&trusted_peers, "node-1", 2);
        // node-2: empty explicit list → full-mesh fallback; node-1↔node-2 already registered by
        // node-1's full-mesh run, but node-0↔node-2 and node-3↔node-2 are not yet registered
        // (explicit paths only track the pairs they actually connect) → 2 new peers
        assert_peer_count(&trusted_peers, "node-2", 2);
        assert_eq!(trusted_peers.len(), 4);
    }

    #[test]
    fn generate_consensus_cli_flags_includes_pruning_distance() {
        let node = manifest::Node {
            cl_config: StartCmd {
                prune_certificates_distance: 500,
                ..StartCmd::default()
            },
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--prune.certificates.distance=500"),
            "missing certificates.distance: {flags_str}"
        );
        assert!(
            !flags_str.contains("--prune.certificates.before"),
            "before should not appear when distance is set: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_includes_pruning_before() {
        let node = manifest::Node {
            cl_config: StartCmd {
                prune_certificates_before: 100,
                ..StartCmd::default()
            },
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--prune.certificates.before=100"),
            "missing certificates.before: {flags_str}"
        );
        assert!(
            !flags_str.contains("--prune.certificates.distance"),
            "distance should not appear when before is set: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_no_prune_when_none_set() {
        for node_type in [
            manifest::NodeType::Validator,
            manifest::NodeType::NonValidator,
        ] {
            let node = manifest::Node {
                node_type,
                ..Default::default()
            };
            let flags = generate_consensus_cli_flags(
                "validator-1",
                Some(&node),
                "172.19.0.5",
                &[],
                None,
                &[],
            )
            .unwrap();
            let flags_str = flags.join(" ");
            assert!(
                !flags_str.contains("--prune.certificates.distance"),
                "unexpected distance flag: {flags_str}"
            );
            assert!(
                !flags_str.contains("--prune.certificates.before"),
                "unexpected before flag: {flags_str}"
            );
            assert!(
                !flags_str.contains("--minimal"),
                "unexpected --minimal: {flags_str}"
            );
            assert!(
                !flags_str.contains("--full"),
                "unexpected --full: {flags_str}"
            );
        }
    }

    #[test]
    fn generate_consensus_cli_flags_prune_distance_emitted() {
        let node = manifest::Node {
            cl_config: StartCmd {
                prune_certificates_distance: 500,
                ..StartCmd::default()
            },
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--prune.certificates.distance=500"),
            "distance should be emitted: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_returns_flags_for_new_version() {
        let flags = generate_consensus_cli_flags(
            "validator-1",
            None,
            "172.19.0.5",
            &[],
            Some("arc_consensus:v0.5.0"),
            &[],
        )
        .unwrap();

        assert!(!flags.is_empty());
        assert!(flags.contains(&"--moniker=validator-1".to_string()));
    }

    #[test]
    fn generate_consensus_cli_flags_includes_follow_mode() {
        let node = manifest::Node {
            node_type: manifest::NodeType::NonValidator,
            follow: true,
            ..Default::default()
        };
        let endpoints = vec![
            "http://validator-1_el:8545".to_string(),
            "http://validator-2_el:8545".to_string(),
        ];
        let flags =
            generate_consensus_cli_flags("rpc-1", Some(&node), "172.19.0.5", &[], None, &endpoints)
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--follow"),
            "missing --follow: {flags_str}"
        );
        assert!(
            flags_str.contains("--follow.endpoint=http://validator-1_el:8545"),
            "missing follow endpoint 1: {flags_str}"
        );
        assert!(
            flags_str.contains("--follow.endpoint=http://validator-2_el:8545"),
            "missing follow endpoint 2: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_no_follow_when_not_enabled() {
        let node = manifest::Node::default();
        let endpoints = vec!["http://validator-1_el:8545".to_string()];
        let flags =
            generate_consensus_cli_flags("rpc-1", Some(&node), "172.19.0.5", &[], None, &endpoints)
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            !flags_str.contains("--follow"),
            "follow should not be present: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_includes_no_consensus() {
        let node = manifest::Node {
            node_type: manifest::NodeType::NonValidator,
            cl_config: StartCmd {
                no_consensus: true,
                ..StartCmd::default()
            },
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("rpc-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--no-consensus"),
            "missing --no-consensus: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_includes_validator_for_validator_nodes() {
        let node = manifest::Node {
            node_type: manifest::NodeType::Validator,
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("val-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--validator"),
            "missing --validator: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_omits_validator_for_non_validator_nodes() {
        let node = manifest::Node {
            node_type: manifest::NodeType::NonValidator,
            ..Default::default()
        };
        let flags = generate_consensus_cli_flags("fn-1", Some(&node), "172.19.0.5", &[], None, &[])
            .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            !flags_str.contains("--validator"),
            "should not contain --validator: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_omits_validator_for_images_that_predate_the_flag() {
        let node = manifest::Node {
            node_type: manifest::NodeType::Validator,
            ..Default::default()
        };
        let flags = generate_consensus_cli_flags(
            "val-1",
            Some(&node),
            "172.19.0.5",
            &[],
            Some("arc_consensus:v0.6.0"),
            &[],
        )
        .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            !flags_str.contains("--validator"),
            "should not contain --validator for a pre-flag image: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_emits_validator_for_images_strictly_newer_than_last_unsupported(
    ) {
        let node = manifest::Node {
            node_type: manifest::NodeType::Validator,
            ..Default::default()
        };
        for tag in ["arc_consensus:v0.6.1", "arc_consensus:v0.7.0"] {
            let flags = generate_consensus_cli_flags(
                "val-1",
                Some(&node),
                "172.19.0.5",
                &[],
                Some(tag),
                &[],
            )
            .unwrap();
            let flags_str = flags.join(" ");
            assert!(
                flags_str.contains("--validator"),
                "missing --validator for tag {tag:?} (expected to postdate the flag): {flags_str}"
            );
        }
    }

    #[test]
    fn generate_consensus_cli_flags_emits_validator_for_latest_image() {
        let node = manifest::Node {
            node_type: manifest::NodeType::Validator,
            ..Default::default()
        };
        let flags = generate_consensus_cli_flags(
            "val-1",
            Some(&node),
            "172.19.0.5",
            &[],
            Some("arc_consensus:latest"),
            &[],
        )
        .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--validator"),
            "missing --validator for the latest image: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_emits_validator_when_image_tag_missing() {
        let node = manifest::Node {
            node_type: manifest::NodeType::Validator,
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("val-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--validator"),
            "missing --validator when no image tag is given: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_includes_cl_prune_preset() {
        let node = manifest::Node {
            cl_prune_preset: Some(manifest::ClPruningPreset::Full),
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("val-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--full"),
            "missing --full preset: {flags_str}"
        );
    }

    #[test]
    fn generate_consensus_cli_flags_prune_distance_overrides_preset() {
        let node = manifest::Node {
            cl_prune_preset: Some(manifest::ClPruningPreset::Minimal),
            cl_config: StartCmd {
                prune_certificates_distance: 500,
                ..StartCmd::default()
            },
            ..Default::default()
        };
        let flags =
            generate_consensus_cli_flags("validator-1", Some(&node), "172.19.0.5", &[], None, &[])
                .unwrap();
        let flags_str = flags.join(" ");
        assert!(
            flags_str.contains("--prune.certificates.distance=500"),
            "explicit distance should take precedence over preset: {flags_str}"
        );
        assert!(
            !flags_str.contains("--minimal"),
            "preset should not be emitted when explicit config is present: {flags_str}"
        );
    }

    #[test]
    fn validate_generated_cl_flags_rejects_unknown_flags() {
        let result = validate_generated_cl_flags(&["--nonexistent-flag".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn generate_jwt_secret_creates_file() {
        let dir = tempdir().unwrap();
        // Create assets directory as expected by the function
        fs::create_dir_all(dir.path().join("assets")).unwrap();

        let result = generate_jwt_secret(dir.path(), false);

        assert!(result.is_ok());
        let jwt_file = dir.path().join("assets").join("jwtsecret");
        assert!(jwt_file.exists());

        // Verify it's a valid hex string
        let contents = fs::read_to_string(&jwt_file).unwrap();
        assert_eq!(contents.len(), 64); // 32 bytes = 64 hex chars
        assert!(contents.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_jwt_secret_with_force_overwrites() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("assets")).unwrap();

        // Generate initial JWT
        generate_jwt_secret(dir.path(), false).unwrap();
        let jwt_file = dir.path().join("assets").join("jwtsecret");
        let original_contents = fs::read_to_string(&jwt_file).unwrap();

        // Generate again with force
        generate_jwt_secret(dir.path(), true).unwrap();
        let new_contents = fs::read_to_string(&jwt_file).unwrap();

        // Should be different (random generation)
        assert_ne!(original_contents, new_contents);
    }

    #[test]
    fn generate_jwt_secret_without_force_skips_existing() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("assets")).unwrap();

        // Generate initial JWT
        generate_jwt_secret(dir.path(), false).unwrap();
        let jwt_file = dir.path().join("assets").join("jwtsecret");
        let original_contents = fs::read_to_string(&jwt_file).unwrap();

        // Try to generate again without force
        generate_jwt_secret(dir.path(), false).unwrap();
        let contents_after = fs::read_to_string(&jwt_file).unwrap();

        // Should be unchanged
        assert_eq!(original_contents, contents_after);
    }

    #[test]
    fn generate_nodes_metadata_file_creates_json() {
        let dir = tempdir().unwrap();
        let metadata_file = dir.path().join("nodes_metadata.json");
        let nodes_metadata = create_test_nodes_metadata(2);

        let result =
            generate_nodes_metadata_file(&metadata_file, &nodes_metadata, InfraType::Local, false);
        assert!(result.is_ok());
        assert!(metadata_file.exists());

        // Verify it's valid JSON
        let contents = fs::read_to_string(&metadata_file).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&contents).is_ok());
    }

    #[test]
    fn generate_nodes_metadata_file_with_force_overwrites() {
        let dir = tempdir().unwrap();
        let metadata_file = dir.path().join("nodes_metadata.json");
        let nodes_metadata = create_test_nodes_metadata(1);

        // Generate initial file
        generate_nodes_metadata_file(&metadata_file, &nodes_metadata, InfraType::Local, false)
            .unwrap();
        let original_contents = fs::read_to_string(&metadata_file).unwrap();

        // Generate again with force
        let nodes_metadata2 = create_test_nodes_metadata(2);
        generate_nodes_metadata_file(&metadata_file, &nodes_metadata2, InfraType::Local, true)
            .unwrap();
        let new_contents = fs::read_to_string(&metadata_file).unwrap();

        // Should be different
        assert_ne!(original_contents, new_contents);
    }

    #[test]
    fn generate_nodes_metadata_file_without_force_skips_existing() {
        let dir = tempdir().unwrap();
        let metadata_file = dir.path().join("nodes_metadata.json");
        let nodes_metadata = create_test_nodes_metadata(1);

        // Generate initial file
        generate_nodes_metadata_file(&metadata_file, &nodes_metadata, InfraType::Local, false)
            .unwrap();
        let original_contents = fs::read_to_string(&metadata_file).unwrap();

        // Try to generate again without force
        let nodes_metadata2 = create_test_nodes_metadata(2);
        generate_nodes_metadata_file(&metadata_file, &nodes_metadata2, InfraType::Local, false)
            .unwrap();
        let contents_after = fs::read_to_string(&metadata_file).unwrap();

        // Should be unchanged
        assert_eq!(original_contents, contents_after);
    }

    #[test]
    fn rewrite_rpc_forwarder_for_remote_rewrites_docker_hostname() {
        let node_subnets: IndexMap<String, Vec<String>> = [
            ("arc".to_string(), vec!["default".to_string()]),
            ("relay".to_string(), vec!["default".to_string()]),
        ]
        .into();
        let mut manifest_nodes = IndexMap::new();
        manifest_nodes.insert("arc".to_string(), manifest::Node::default());
        manifest_nodes.insert("relay".to_string(), manifest::Node::default());
        let infra_data = InfraData::new_local("testnet".to_string(), &manifest_nodes);
        let manifest = Manifest::new(Some("testnet".to_string()), &manifest_nodes, &node_subnets);
        let nodes_metadata = NodesMetadata::new(
            infra_data,
            &manifest,
            &manifest.images.to_local().unwrap(),
            &BTreeSet::new(),
        )
        .unwrap();

        let mut flags = vec!["--rpc.forwarder=http://relay_el:8545".to_string()];
        rewrite_rpc_forwarder_for_remote(&mut flags, "arc", &nodes_metadata, &manifest.subnets);
        assert!(
            !flags[0].contains("relay_el"),
            "expected Docker hostname rewritten: {}",
            flags[0]
        );
        assert!(
            flags[0].starts_with("--rpc.forwarder=http://"),
            "expected http forwarder: {}",
            flags[0]
        );
        assert!(flags[0].ends_with(":8545"), "{}", flags[0]);
    }

    #[test]
    fn rewrite_rpc_forwarder_for_remote_leaves_literal_ips() {
        let nodes_metadata = create_test_nodes_metadata(1);
        let manifest_nodes = IndexMap::from([("node-0".to_string(), manifest::Node::default())]);
        let node_subnets: IndexMap<String, Vec<String>> =
            [("node-0".to_string(), vec!["default".to_string()])].into();
        let manifest = Manifest::new(Some("t".to_string()), &manifest_nodes, &node_subnets);
        let mut flags = vec!["--rpc.forwarder=http://192.168.1.1:8545".to_string()];
        rewrite_rpc_forwarder_for_remote(&mut flags, "node-0", &nodes_metadata, &manifest.subnets);
        assert_eq!(flags[0], "--rpc.forwarder=http://192.168.1.1:8545");
    }
}
