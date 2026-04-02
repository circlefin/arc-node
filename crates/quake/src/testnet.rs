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

use color_eyre::eyre::{self, bail, eyre, Context, Result};
use indexmap::IndexMap;
use itertools::Itertools;
use rand::Rng;
use spammer::{self, Spammer};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use std::{env, fs};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::infra::terraform::Terraform;
use crate::infra::{self, local, remote, BuildProfile, InfraData, InfraProvider, InfraType};
use crate::infra::{local::LocalInfra, remote::RemoteInfra};
use crate::infra::{COMPOSE_PROJECT_NAME, PPROF_PROXY_SSM_PORT, RPC_PROXY_SSM_PORT};
use crate::manifest::Manifest;
use crate::node::{NodeMetadata, NodeName};
use crate::nodes::{NodeOrContainerName, NodesMetadata};
use crate::perturb::{self, Perturbation};
use crate::rpc::RpcClient;
use crate::rpc::{ControllerInfo, Controllers};
use crate::valset::ValidatorPowerUpdate;
use crate::wait::{check_ws_connectable, wait_for_nodes, wait_for_nodes_sync, wait_for_rounds};
use crate::{build, genesis, info as info_mod, latency, monitor, setup, shell};
use crate::{InfoSubcommand, RemoteSubcommand, SSMSubcommand};

pub(crate) const QUAKE_DIR: &str = ".quake";
pub(crate) const LAST_MANIFEST_FILENAME: &str = ".last_manifest";

/// Stores the nodes upgraded using the 'quake upgrade' command on the running
/// testnet, one per line. e.g.,:
///
/// validator1
/// validator4
/// validator5
///
/// Will be deleted after the testnet is torn down with 'quake clean'.
const UPGRADED_CONTAINERS_FILENAME: &str = ".upgraded_containers";

const BLOCKSCOUT_CONTAINERS: [&str; 5] = [
    "blockscout-db-init",
    "blockscout-db",
    "blockscout-backend",
    "blockscout-proxy",
    "blockscout-frontend",
];

#[derive(Debug, thiserror::Error)]
pub enum TestnetError {
    #[error(
        "No path to manifest file provided and no existing path found in {0}\n\
        Run with `--file PATH_TO_MANIFEST`"
    )]
    NoManifestFound(String),
}

/// Resolved Docker images for a running testnet. Base images (`cl`, `el`) are always
/// present; upgrade images remain optional.
#[derive(Debug, serde::Serialize, Clone, PartialEq)]
pub(crate) struct DockerImages {
    /// Consensus layer image (always set after resolution).
    pub cl: String,
    /// Execution layer image (always set after resolution).
    pub el: String,
    /// Consensus layer upgrade image, if an upgrade scenario is configured.
    pub cl_upgrade: Option<String>,
    /// Execution layer upgrade image, if an upgrade scenario is configured.
    pub el_upgrade: Option<String>,
}

impl DockerImages {
    /// All present image references (base + upgrade).
    pub fn all(&self) -> Vec<&str> {
        let mut out = vec![self.cl.as_str(), self.el.as_str()];
        if let Some(ref img) = self.cl_upgrade {
            out.push(img.as_str());
        }
        if let Some(ref img) = self.el_upgrade {
            out.push(img.as_str());
        }
        out
    }
}

pub(crate) struct Testnet {
    pub name: String,
    pub dir: PathBuf,
    pub quake_dir: PathBuf,
    pub repo_root_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: Manifest,
    pub images: DockerImages,
    pub seed: Option<u64>,
    pub infra: Arc<dyn InfraProvider>,
    pub infra_data: InfraData,
    pub nodes_metadata: NodesMetadata,
}

impl Testnet {
    pub async fn from(manifest_file: &Option<PathBuf>, force_remote: bool) -> Result<Self> {
        if let Some(manifest_file) = manifest_file {
            Testnet::from_manifest(manifest_file, force_remote).await
        } else {
            let last_manifest_path = Path::new(QUAKE_DIR).join(LAST_MANIFEST_FILENAME);
            let last_manifest_path_str = last_manifest_path.display().to_string();
            if last_manifest_path.exists() {
                let manifest_path = fs::read_to_string(last_manifest_path)?;
                info!(manifest=%manifest_path, "Using existing path to manifest found in {last_manifest_path_str}");
                Testnet::from_manifest(&PathBuf::from(manifest_path), force_remote).await
            } else {
                Err(TestnetError::NoManifestFound(last_manifest_path_str).into())
            }
        }
    }

    // Build testnet name from manifest file name
    fn testnet_name(manifest_file: &Path) -> Result<String> {
        Ok(manifest_file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| eyre::eyre!("Unable to determine testnet name from manifest file name"))?
            .to_string())
    }

    async fn from_manifest(manifest_path: &Path, force_remote: bool) -> Result<Self> {
        // Load and parse the manifest file
        let manifest = Manifest::from_file(manifest_path)?;

        let testnet_name = Self::testnet_name(manifest_path)?;

        // Create quake directory
        let repo_root_dir =
            env::current_dir().context("Failed to get current working directory")?;
        let quake_dir = repo_root_dir.join(QUAKE_DIR);
        fs::create_dir_all(&quake_dir).wrap_err("Failed to create quake directory")?;

        let dir = quake_dir.join(testnet_name.replace('_', "-"));

        let relative_dir = shell::relative_path(&dir, &repo_root_dir)?;
        let node_names = manifest.nodes.keys().cloned().collect::<Vec<_>>();

        // Create infra data (local) or load from file (remote)
        let infra_data = InfraData::new(&dir, testnet_name.clone(), &manifest.nodes, force_remote)?;

        // Now that we know the infrastructure type, define Docker images for the testnet
        let images = match infra_data.infra_type {
            InfraType::Local => manifest.images.to_local()?,
            InfraType::Remote => manifest.images.to_remote()?,
        };

        // Load list of upgraded containers from file
        let upgraded_containers_file = quake_dir.join(UPGRADED_CONTAINERS_FILENAME);
        let upgraded_containers =
            perturb::load_upgraded_containers_set(&upgraded_containers_file).await?;

        // Build container information for the nodes in the manifest
        let nodes_metadata =
            NodesMetadata::new(infra_data.clone(), &manifest, &upgraded_containers)?;

        // Create infrastructure provider, monitoring manager (local only)
        let infra: Arc<dyn InfraProvider> = match infra_data.infra_type {
            InfraType::Local => {
                let monitoring = local::MonitoringManager::new(&repo_root_dir, &quake_dir)?;
                Arc::new(
                    LocalInfra::new(&repo_root_dir, &relative_dir, monitoring)
                        .wrap_err_with(|| "Failed to create testnet from manifest")?,
                )
            }
            InfraType::Remote => {
                let terraform = Terraform::new(
                    &repo_root_dir.join("crates").join("quake").join("terraform"),
                    &relative_dir,
                    manifest_path,
                    images.clone(),
                    node_names,
                    manifest.build_network_topology(),
                )?;
                let ssm_tunnels = infra::ssm::Ssm::new(infra_data.control_center.as_ref())
                    .wrap_err("Failed to initialize SSM tunnels")?;
                Arc::new(
                    RemoteInfra::new(
                        &repo_root_dir,
                        &relative_dir,
                        infra_data.clone(),
                        terraform,
                        ssm_tunnels,
                    )
                    .wrap_err_with(|| "Failed to create testnet from manifest")?,
                )
            }
        };

        // Save manifest path to .quake/.last_manifest
        let last_manifest_path = quake_dir.join(LAST_MANIFEST_FILENAME);
        fs::write(
            repo_root_dir.join(last_manifest_path),
            manifest_path.to_string_lossy().to_string(),
        )
        .wrap_err("Failed to save last manifest file")?;

        Ok(Testnet {
            name: testnet_name,
            dir,
            quake_dir,
            repo_root_dir,
            manifest_path: manifest_path.to_path_buf(),
            manifest,
            images,
            seed: None,
            infra,
            infra_data,
            nodes_metadata,
        })
    }

    /// Set seed if provided, or generate a random one.
    pub fn with_seed(&mut self, seed: Option<u64>) -> &mut Self {
        self.seed = seed.or({
            let seed = rand::thread_rng().gen_range(0..=u64::MAX);
            info!("Using random seed; to reproduce this execution, run with: --seed {seed}");
            Some(seed)
        });
        self
    }

    /// Set up testnet files locally
    pub async fn setup(&mut self, force: bool, rpc: bool, num_extra_accounts: usize) -> Result<()> {
        debug!(dir=%self.dir.display(), "⚙️ Setting up testnet files");
        debug!(
            "Using {} for the Engine API connection between Consensus Layer (CL) and Execution Layer (EL)",
            if rpc {
                "authenticated RPC"
            } else {
                "default IPC"
            }
        );

        // Create testnet directory, sub-directories, and copy entrypoint scripts
        fs::create_dir_all(&self.dir).with_context(|| {
            format!("Failed to create testnet directory: {}", self.dir.display())
        })?;
        let assets_dir = &self.dir.join("assets");
        fs::create_dir_all(assets_dir)?;
        fs::create_dir_all(self.dir.join("logs"))?;
        let quake_files_dir = self
            .repo_root_dir
            .join("crates")
            .join("quake")
            .join("files");
        for path in ["entrypoint_cl.sh", "entrypoint_el.sh"] {
            fs::copy(quake_files_dir.join(path), assets_dir.join(path))?;
        }

        // Build system contracts and bindings
        setup::generate_system_contracts(&self.repo_root_dir, force)?;

        // Generate genesis file
        let genesis_file_path = self.dir.join("assets").join("genesis.json");
        let validator_names = self.manifest.validator_names();
        if validator_names.is_empty() {
            bail!("No validator nodes found in manifest");
        }

        let voting_powers = self.manifest.validator_voting_powers();
        setup::generate_genesis_file(
            &self.repo_root_dir,
            &genesis_file_path,
            num_extra_accounts,
            &self.manifest.public_key_overrides(),
            &validator_names,
            voting_powers.as_deref(),
            force,
            self.manifest.el_init_hardfork.as_deref(),
        )?;

        // We want access to files outside of the testnet directory.
        let deployments_dir = self.repo_root_dir.join("deployments");
        let relative_deployments_dir = &shell::relative_path(&deployments_dir, &self.dir)?;
        let relative_quake_dir = &shell::relative_path(&self.quake_dir, &self.dir)?;

        // Generate CLI flags for each node BEFORE rendering compose templates
        self.generate_cli_flags_for_nodes()?;

        // Generate nodekeys for Reth P2P identity, write them to disk, and
        // build the trusted_peers map for compose templates.
        let nodekeys = setup::generate_nodekeys(&self.nodes_metadata, &self.dir, force)?;
        let el_trusted_peers_per_node: IndexMap<String, Option<Vec<String>>> = self
            .manifest
            .nodes
            .iter()
            .map(|(name, node)| (name.clone(), node.el_trusted_peers.clone()))
            .collect();
        let trusted_peers = setup::build_trusted_peers_map(
            &nodekeys,
            Some(&el_trusted_peers_per_node),
            &self.nodes_metadata,
            &self.manifest.subnets,
        )?;

        // Generate compose files
        match self.infra_data.infra_type {
            InfraType::Local => {
                let local_infra = self.local_infra()?;

                // The Docker images to build locally
                let (reth_builds, malachite_builds) = build::local_images_to_build(&self.images);

                // Generate Docker Compose files
                let compose_data = setup::ComposeTemplateDataLocal {
                    compose_project_name: COMPOSE_PROJECT_NAME.to_string(),
                    nodes: self.nodes_metadata.values(),
                    networks: setup::build_template_networks(&self.manifest.subnets.cidr_map()),
                    deployments_dir: relative_deployments_dir.to_string_lossy().to_string(),
                    quake_dir: relative_quake_dir.to_string_lossy().to_string(),
                    images: self.images.clone(),
                    rpc,
                    reth_builds,
                    malachite_builds,
                    latency_emulation: self.manifest.latency_emulation,
                    monitoring_bind_host: self.manifest.monitoring_bind_host.clone(),
                    trusted_peers,
                };
                // Compose file for building Docker images
                setup::generate_compose_file(
                    &self.dir.join(infra::local::COMPOSE_BUILD_FILENAME),
                    &compose_data,
                    include_str!("../templates/local/arc_builders.yaml.hbs"),
                    force,
                )?;
                // Compose file for running all testnet nodes and block explorer
                setup::generate_compose_file(
                    &self.dir.join(infra::local::COMPOSE_FILENAME),
                    &compose_data,
                    include_str!("../templates/local/compose.yaml.hbs"),
                    force,
                )?;

                // Setup monitoring directory
                local_infra.monitoring.setup()?;

                // Compose file for running the monitoring services
                setup::generate_compose_file(
                    &local_infra.monitoring.compose_path,
                    &compose_data,
                    include_str!("../templates/local/compose-monitoring.yaml.hbs"),
                    force,
                )?;

                // Generate prometheus configuration
                let path = local_infra.monitoring.dir.join("prometheus.yml");
                setup::generate_prometheus_config(&path, self.nodes_metadata.values())?;
            }
            InfraType::Remote => {
                // Get consensus container IPs for all nodes (needed for persistent peers)
                let consensus_addresses_map = self.nodes_metadata.consensus_ip_addresses_map();

                // Generate a compose file per node with node-specific EL and CL
                // configuration
                for (node_name, node) in self.manifest.nodes.iter() {
                    let el_cli_flags = node
                        .el_cli_flags()
                        .context("Failed to generate EL CLI flags")?;

                    let peers_ips: Vec<String> = if let Some(peers) = &node.cl_persistent_peers {
                        NodesMetadata::peer_consensus_ips(
                            node_name,
                            peers,
                            &consensus_addresses_map,
                        )?
                    } else {
                        consensus_addresses_map
                            .iter()
                            .filter(|&(peer_name, _)| peer_name != node_name)
                            .flat_map(|(_, private_ips)| private_ips.clone())
                            .collect()
                    };

                    // Generate CL CLI flags including persistent peers
                    let cl_cli_flags = setup::generate_node_cli_flags(
                        node_name,
                        Some(node),
                        "0.0.0.0", // Remote nodes listen on all interfaces
                        &peers_ips,
                        Some(self.images.cl.as_str()),
                    );

                    let compose_data = setup::ComposeTemplateDataRemote {
                        compose_project_name: COMPOSE_PROJECT_NAME.to_string(),
                        cl_container_name: remote::CONTAINER_NAME_CONSENSUS.to_string(),
                        el_container_name: remote::CONTAINER_NAME_EXECUTION.to_string(),
                        rpc,
                        remote_home_dir: format!("/home/{}", remote::USER_NAME),
                        images: self.images.clone(),
                        cl_cli_flags,
                        el_cli_flags,
                        trusted_peers: trusted_peers.get(node_name).cloned().unwrap_or_default(),
                    };
                    // Create node directory for compose file
                    let node_dir = self.dir.join(node_name);
                    fs::create_dir_all(&node_dir)?;

                    // Compose file for running consensus layer and execution layer
                    // in this remote node
                    let node_compose_path = node_dir.join(infra::local::COMPOSE_FILENAME);
                    setup::generate_compose_file(
                        &node_compose_path,
                        &compose_data,
                        include_str!("../templates/remote/compose-node.yaml.hbs"),
                        force,
                    )?;
                }
            }
        }

        // Generate Malachite app private keys for each node.
        // Validators must get keys first so their BIP39 indices match the genesis.
        setup::generate_app_private_keys(&self.dir, &self.nodes_metadata, force)?;

        // Generate Malachite app config files for each node (for backward compatibility with older versions)
        setup::generate_app_config_files(&self.dir, &self.nodes_metadata, &self.manifest, force)?;

        // Generate JWT secret for authenticated RPC connection between Reth and Malachite
        if rpc {
            setup::generate_jwt_secret(&self.dir, force)?;
        }

        // Generate latency emulation scripts
        if self.manifest.latency_emulation {
            latency::generate_latency_scripts(
                &self.dir,
                &mut self.manifest.latency_emulation,
                &mut self.manifest.nodes,
                &self.nodes_metadata,
                self.seed.expect("Seed must be set on setup"),
                force,
            )?;
        }

        // Generate file with all node metadata
        let path = self.dir.join("nodes.json");
        setup::generate_nodes_metadata_file(
            &path,
            &self.nodes_metadata,
            self.infra_data.infra_type,
            force,
        )?;

        // For local testnets, create EL reth dirs and set permissions so containers (user arc) can write
        if self.infra_data.infra_type == InfraType::Local {
            let node_names: Vec<String> = self.manifest.nodes.keys().cloned().collect();
            setup::set_local_testnet_directory_permissions(&self.dir, &node_names)?;
        }

        // In remote mode, provision the Control Center server
        if let Ok(remote_infra) = self.remote_infra() {
            remote_infra.provision()?;
        }

        info!(dir=%self.dir.display(), "✅ Testnet setup completed");
        Ok(())
    }

    /// Build Docker images locally as defined in the manifest
    pub async fn build(&self, profile: BuildProfile) -> Result<()> {
        if let Err(err) = self.infra.is_setup(&[]) {
            bail!("Infra is not set up: {err}: run `quake setup` first to create the testnet infrastructure");
        }

        // Pull remote images (local mode only)
        if self.is_local() {
            let remote_images = build::remote_images_to_pull(&self.images);
            remote_images.iter().try_for_each(|image| {
                info!(%image, "Pulling remote image");
                infra::docker::pull(image)
            })?;
        }

        let infra = Arc::clone(&self.infra);
        tokio::task::spawn_blocking(move || infra.build(profile)).await??;

        info!(dir=%self.dir.display(), "✅ Docker images built");
        Ok(())
    }

    /// Start testnet containers using Docker Compose
    pub async fn start(&self, names: Vec<NodeOrContainerName>) -> Result<()> {
        // In remote mode, open long-lived SSM tunnels to the Control Center
        // server ports (required for RPC proxy and monitoring services)
        if let Ok(remote_infra) = self.remote_infra() {
            remote_infra.ssm_tunnels.start().await?;
        }

        if !names.is_empty() {
            // Start the given node and container names ignoring the starting heights in the manifest
            let containers = self.nodes_metadata.expand_to_containers_list(&names)?;

            // Start immediately the given nodes and containers
            info!(containers=%containers.join(", "), "🚀 Starting testnet");
            self.infra.start(&containers)?;
        } else {
            // Start the testnet following the starting heights in the manifest
            self.start_from_manifest().await?;
        }

        info!(dir=%self.dir.display(), "✅ Testnet started");
        println!("📁 Testnet files: {}", self.dir.display());
        self.print_monitoring_info();
        Ok(())
    }

    /// Start nodes in the testnet following their starting heights in the manifest
    async fn start_from_manifest(&self) -> Result<()> {
        // Group nodes by starting height, then sort groups by height
        let nodes_by_height = self
            .manifest
            .nodes
            .iter()
            // produce a HashMap<u64, Vec<(&String, &Node)>>, where the keys
            // are the starting heights of the nodes.
            // The tuple (&String, &Node) represents the node's name as defined in
            // the manifest, and its data.
            .into_group_map_by(|(_, node)| node.start_at.unwrap_or(0))
            .into_iter()
            .sorted_by_key(|(height, _)| *height)
            .map(|(height, nodes)| {
                let names = nodes.iter().map(|(n, _)| (**n).clone()).collect::<Vec<_>>();
                // builds tuples of (height, metadata of nodes scheduled to start at
                // that height)
                (height, self.nodes_metadata.filter_values(&names))
            });

        info!(dir=%self.dir.display(), "🚀 Starting testnet from manifest");

        // Start node groups at their starting height
        let mut started_nodes: Vec<&NodeMetadata> = Vec::new();
        for (height, nodes) in nodes_by_height {
            let started_node_names = started_nodes
                .iter()
                .map(|n| (n.name).clone())
                .collect::<Vec<_>>();

            // Wait for the running nodes to reach the starting height for this group
            if !started_nodes.is_empty() && height > 0 {
                // Note: this timeout is arbitrary, but should be long enough for a test scenario.
                self.wait(height, &started_node_names, Duration::from_secs(60))
                    .await?;
            }

            let node_names = nodes.iter().map(|n| (n.name).clone()).collect::<Vec<_>>();
            let names_str = node_names.as_slice().join(", ");
            info!(nodes=%names_str, "Starting nodes at height {height}");

            // Start containers associated with the node group
            let mut containers: Vec<_> = nodes.iter().flat_map(|n| n.container_names()).collect();
            // In local mode, start monitoring services with the first group of nodes
            if let Ok(local_infra) = self.local_infra() {
                if started_nodes.is_empty() {
                    containers.extend(BLOCKSCOUT_CONTAINERS.map(String::from));

                    let monitoring = local_infra.monitoring.clone();
                    tokio::task::spawn_blocking(move || monitoring.start()).await??;
                }
            }

            debug!(containers=%containers.join(", "), "Starting containers");
            self.infra.start(&containers)?;

            // Check that nodes have reached their starting height
            let height = std::cmp::max(1, height);
            self.wait(height, &node_names, Duration::from_secs(30))
                .await?;

            info!(nodes=%names_str, "✅ Nodes have started at height {height}");
            started_nodes.extend(nodes);
        }

        Ok(())
    }

    /// Wait for given nodes to all reach a certain height
    pub async fn wait(&self, height: u64, nodes: &[NodeName], timeout: Duration) -> Result<()> {
        let node_names = self.nodes_metadata.expand_to_nodes_list(nodes)?;

        // Validate arguments
        self.manifest.contain_nodes(&node_names)?;

        // If no node names are provided, use all nodes
        let node_names = if node_names.is_empty() {
            self.nodes_metadata.node_names()
        } else {
            node_names
        };

        // Get the RPC URLs of the nodes to wait for
        let node_urls = self.nodes_metadata.to_execution_http_urls(&node_names);

        wait_for_nodes(node_urls, height, timeout).await?;
        info!(nodes=%node_names.join(", "), "✅ Nodes have reached height {height}");
        Ok(())
    }

    /// Run tests or list them with --dry-run
    pub async fn run_tests(
        &self,
        spec: &str,
        dry_run: bool,
        rpc_timeout: Duration,
        params: &crate::tests::TestParams,
    ) -> Result<()> {
        crate::tests::run_tests(self, spec, dry_run, rpc_timeout, params).await
    }

    /// Wait for given nodes to finish syncing (eth_syncing returns false)
    pub async fn wait_sync(
        &self,
        nodes: Vec<NodeName>,
        timeout: Duration,
        max_retries: u32,
    ) -> Result<()> {
        let node_names = self.nodes_metadata.expand_to_nodes_list(&nodes)?;

        // Validate arguments
        self.manifest.contain_nodes(&node_names)?;

        // If no node names are provided, use all nodes
        let node_names = if node_names.is_empty() {
            self.nodes_metadata.node_names()
        } else {
            node_names
        };

        // Get the RPC URLs of the nodes to wait for
        let node_urls = self.nodes_metadata.to_execution_http_urls(&node_names);

        wait_for_nodes_sync(node_urls, timeout, max_retries).await?;
        info!(nodes=%node_names.join(", "), "✅ Nodes have finished syncing");
        Ok(())
    }

    /// Wait for consensus rounds to settle at 0 by subscribing to new block headers
    /// and checking the decided round for each block.
    ///
    /// Tries each consensus node in order until one accepts a WebSocket connection.
    /// This handles the case where the first node is down (e.g. mid-upgrade).
    /// Once connected, monitoring errors (including timeout) are returned immediately
    /// without trying other nodes.
    pub async fn wait_rounds(&self, consecutive: u64, timeout: Duration) -> Result<()> {
        let consensus_urls = self.nodes_metadata.all_consensus_rpc_urls();
        if consensus_urls.is_empty() {
            bail!("No consensus nodes found");
        }

        // Try each node until one accepts a WebSocket connection
        for (node_name, cl_url) in &consensus_urls {
            let ws_url = match self.nodes_metadata.execution_ws_url(node_name) {
                Some(url) => url,
                None => continue,
            };

            if let Err(e) = check_ws_connectable(&ws_url, node_name).await {
                warn!("Could not connect to {node_name}, trying next node: {e}");
                continue;
            }

            // Node is reachable — run the full monitoring loop.
            // Errors here (including timeout) are real failures, not connection issues.
            return wait_for_rounds(ws_url, cl_url.clone(), node_name, consecutive, timeout).await;
        }

        bail!("No reachable consensus nodes")
    }

    /// Stop CL and/or EL containers
    pub async fn stop(&self, names: Vec<String>) -> Result<()> {
        let containers = if names.is_empty() {
            self.nodes_metadata.all_container_names()
        } else {
            self.nodes_metadata.expand_to_containers_list(&names)?
        };

        info!(containers=%containers.join(", "), "🛑 Stopping testnet");
        let infra = Arc::clone(&self.infra);
        tokio::task::spawn_blocking(move || infra.stop(&containers)).await??;
        info!(containers=%names.join(", "), "✅ Testnet stopped");
        Ok(())
    }

    /// Apply a perturbation to a set of containers
    pub async fn perturb(
        &self,
        action: Perturbation,
        min_time_off: Duration,
        max_time_off: Duration,
    ) -> Result<()> {
        debug!(%action, "🔀 Applying perturbation");

        // Get the containers to perturb
        let mut containers = self
            .nodes_metadata
            .expand_to_containers_list(action.target_names())?;

        // Apply to all containers if no containers are specified
        if containers.is_empty() {
            containers = self.nodes_metadata.all_container_names()
        }

        // Validate input
        if matches!(action, Perturbation::Upgrade { .. }) {
            if self.images.cl_upgrade.is_none() {
                bail!("No arc_consensus upgrade version specified in the manifest");
            }
            if self.images.el_upgrade.is_none() {
                bail!("No arc_execution upgrade version specified in the manifest");
            }

            // Filter out containers already upgraded; early return if none remain.
            if !perturb::filter_upgraded_containers(&mut containers)? {
                return Ok(());
            }
        }

        action
            .apply(
                self.infra.as_ref(),
                &self.nodes_metadata,
                &containers,
                self.seed.expect("Seed must be set on perturb command!"),
                min_time_off,
                max_time_off,
                self.nodes_metadata.num_nodes(),
            )
            .await?;

        if matches!(action, Perturbation::Upgrade { .. }) {
            let upgraded_containers_file = self.quake_dir.join(UPGRADED_CONTAINERS_FILENAME);

            perturb::persist_upgraded_containers(&upgraded_containers_file, containers)
                .await
                .wrap_err("failed to persist upgraded containers")?;
        }

        info!(%action, "✅ Perturbation applied");
        Ok(())
    }

    /// Output the logs of the given nodes or containers, or all nodes if none are given.
    /// Optionally following the logs.
    pub async fn logs(&self, names: Vec<NodeOrContainerName>, follow: bool) -> Result<()> {
        let containers_str = if names.is_empty() {
            "CL and EL containers of all nodes".to_string()
        } else {
            names.join(", ")
        };
        debug!(containers=%containers_str, "Showing logs");

        let containers = self.nodes_metadata.expand_to_containers_list(&names)?;

        let infra = Arc::clone(&self.infra);
        tokio::task::spawn_blocking(move || infra.logs(&containers, follow)).await?
    }

    /// Show the status of the testnet
    pub async fn info(&self, command: Option<InfoSubcommand>) -> Result<()> {
        if !self.dir.exists() {
            bail!("Testnet directory does not exist: {}", self.dir.display());
        }

        let node_urls = self.nodes_metadata.all_execution_urls();
        let max_name_len = self.nodes_metadata.max_node_name_len();

        match command {
            None => {
                println!("* Nodes, container ports, and follow info");
                info_mod::print_nodes_info(&self.nodes_metadata);
                println!();

                println!("* IP addresses");
                info_mod::print_nodes_ip_addresses(&self.nodes_metadata);
                println!();

                if self.is_remote() {
                    println!("* Remote infrastructure");
                    info_mod::print_remote_infra_data(&self.infra_data);
                    println!();
                }

                println!("* Monitoring");
                self.print_monitoring_info();
                println!();

                let assets_dir = self.dir.join("assets");
                if let Ok(controllers) = Controllers::load_from_file(&assets_dir) {
                    println!("* Keys and addresses");
                    info_mod::print_keys(&node_urls, &controllers, max_name_len).await;
                    println!();

                    // Fetch CL mesh peer counts in parallel with latest data
                    let metrics_urls = self.nodes_metadata.all_consensus_metrics_urls();
                    let raw_metrics = crate::mesh::fetch_all_metrics(&metrics_urls).await;
                    let nodes_data = crate::mesh::parse_all_metrics(&raw_metrics);
                    let cl_mesh_peers: std::collections::HashMap<String, i64> = nodes_data
                        .iter()
                        .map(|n| {
                            let count = n.mesh_counts.get("/consensus").copied().unwrap_or(0);
                            (n.moniker.clone(), count)
                        })
                        .collect();

                    println!("* Latest data");
                    info_mod::print_latest_data(
                        &node_urls,
                        &controllers,
                        &cl_mesh_peers,
                        max_name_len,
                    )
                    .await;
                    println!();
                } else {
                    warn!("Failed to load controllers (is testnet running?)");
                }
            }
            Some(InfoSubcommand::Height { node }) => {
                let height = info_mod::get_node_height(&self.nodes_metadata, &node).await?;
                println!("{height}");
            }
            Some(InfoSubcommand::Heights { number }) => {
                info_mod::loop_print_latest_heights(&node_urls, number).await?;
            }
            Some(InfoSubcommand::Mempool) => {
                println!("* Mempool status -- number of pending and queued transactions");
                info_mod::loop_print_mempool(&node_urls).await?;
            }
            Some(InfoSubcommand::Peers { all }) => {
                info_mod::print_peers_info(&self.nodes_metadata, all).await?;
            }
            Some(InfoSubcommand::Mesh {
                mesh_only,
                peers,
                peers_full,
            }) => {
                let metrics_urls = self.nodes_metadata.all_consensus_metrics_urls();
                let raw_metrics = crate::mesh::fetch_all_metrics(&metrics_urls).await;
                let nodes_data = crate::mesh::parse_all_metrics(&raw_metrics);
                let analysis = crate::mesh::analyze(&nodes_data);
                let options = crate::mesh::MeshDisplayOptions {
                    show_counts: !mesh_only,
                    show_mesh: true,
                    show_peers: peers || peers_full,
                    show_peers_full: peers_full,
                };
                print!("{}", crate::mesh::format_report(&analysis, &options));
            }
            Some(InfoSubcommand::Perf {
                latency_only,
                throughput_only,
            }) => {
                let metrics_urls = self.nodes_metadata.all_consensus_metrics_urls();
                let raw_metrics = arc_checks::fetch_all_metrics(&metrics_urls).await;
                let mut nodes = arc_checks::parse_perf_metrics(&raw_metrics);

                crate::util::assign_node_groups(
                    nodes.iter_mut().map(|n| (n.name.as_str(), &mut n.group)),
                    &self.manifest.nodes,
                );

                if nodes.is_empty() {
                    println!("No nodes responded to metrics requests. Is the testnet running?");
                } else {
                    let options = arc_checks::PerfDisplayOptions {
                        show_latency: !throughput_only,
                        show_throughput: !latency_only,
                        show_summary: !latency_only && !throughput_only,
                    };
                    print!("{}", arc_checks::format_perf_report(&nodes, &options));
                }
            }
            Some(InfoSubcommand::Store { nodes }) => {
                let node_names = if nodes.is_empty() {
                    self.nodes_metadata.node_names()
                } else {
                    nodes
                };
                for node_name in &node_names {
                    println!("* {node_name}");
                    let store_path = self.dir.join(node_name).join("malachite").join("store.db");
                    if store_path.exists() {
                        if let Err(e) = info_mod::print_store_info(&store_path) {
                            println!("  error: {e}");
                            println!();
                        }
                    } else {
                        println!("  store.db not found at {}", store_path.display());
                        println!();
                    }
                }
            }
            Some(InfoSubcommand::Health) => {
                let metrics_urls = self.nodes_metadata.all_consensus_metrics_urls();
                let raw_metrics = arc_checks::fetch_all_metrics(&metrics_urls).await;
                let mut nodes_data = arc_checks::parse_all_health_metrics(&raw_metrics);

                crate::util::assign_node_groups(
                    nodes_data
                        .iter_mut()
                        .map(|n| (n.name.as_str(), &mut n.group)),
                    &self.manifest.nodes,
                );

                print!("{}", arc_checks::format_health_report(&nodes_data));
            }
        }
        Ok(())
    }

    /// Clean up testnet-related files, directories, infrastructure, and running processes
    pub async fn clean(&self, all: bool) -> Result<()> {
        // Take down the testnet infrastructure
        match self.infra_data.infra_type {
            InfraType::Local => {
                if let Err(err) = self.infra.down(&[]) {
                    warn!(%err, "⚠️ Failed to stop and remove containers");
                } else {
                    debug!("✅ Testnet is down");
                }
            }
            InfraType::Remote => {
                let remote_infra = self.remote_infra()?;
                if let Err(err) = remote_infra.ssm_tunnels.stop().await {
                    warn!(%err, "⚠️ Failed to terminate SSM sessions");
                }

                if remote_infra.terraform.has_state() {
                    debug!("⬇️ Destroying remote infrastructure...");
                    if let Err(err) = remote_infra.terraform.destroy(true) {
                        warn!(%err, "⚠️ Failed to destroy remote infrastructure");
                    } else {
                        info!("✅ Remote infrastructure destroyed");
                    }
                } else {
                    info!("No Terraform state found; skipping infrastructure destroy");
                }
            }
        }

        // Remove testnet local data
        if self.dir.exists() {
            debug!(dir=%self.dir.display(), "🗑️  Removing testnet data");
            if let Err(err) = fs::remove_dir_all(&self.dir) {
                warn!(dir=%self.dir.display(), "Failed to remove testnet data: {err}");
            } else {
                debug!(dir=%self.dir.display(), "✅ Testnet data removed");
            }
        }

        // Take down local monitoring services and remove their data, if requested
        if let Ok(local_infra) = self.local_infra() {
            if all {
                if local_infra.monitoring.stop().is_ok() {
                    debug!("✅ Monitoring services stopped");
                }
                if local_infra.monitoring.clean().is_ok() {
                    debug!(dir=%local_infra.monitoring.dir.display(), "✅ Monitoring data removed");
                }
            } else {
                warn!(
                    "Monitoring services are still running; run `quake clean --all` to stop and clean them"
                );
            }
        }

        let _ = fs::remove_file(self.quake_dir.join(UPGRADED_CONTAINERS_FILENAME));

        info!("✅ Testnet cleaned");
        Ok(())
    }

    /// Manage the remote infrastructure
    pub async fn remote(&self, command: RemoteSubcommand) -> Result<()> {
        let infra = self.remote_infra()?;
        match command {
            RemoteSubcommand::Preinit => infra.terraform.init(),
            RemoteSubcommand::Create {
                dry_run,
                yes,
                infra_args,
            } => infra.terraform.create(
                dry_run,
                yes,
                infra_args.node_size.as_deref(),
                infra_args.cc_size.as_deref(),
            ),
            RemoteSubcommand::Status => {
                info_mod::print_remote_infra_data(&self.infra_data);
                Ok(())
            }
            RemoteSubcommand::Monitor {
                node_or_cc,
                follow,
                interval,
            } => {
                let node_urls = self.nodes_metadata.all_execution_urls();
                monitor::monitor_loop(
                    &self.infra_data,
                    &infra,
                    &node_urls,
                    &node_or_cc,
                    follow,
                    interval,
                )
                .await
            }
            RemoteSubcommand::Provision => infra.provision(),
            RemoteSubcommand::Ssm { command } => match command {
                SSMSubcommand::Start => infra.ssm_tunnels.start().await,
                SSMSubcommand::Stop => infra.ssm_tunnels.stop().await,
                SSMSubcommand::List => infra.ssm_tunnels.list().await,
            },
            RemoteSubcommand::Destroy { yes } => infra.terraform.destroy(yes),
            RemoteSubcommand::Ssh {
                node_or_cc,
                command,
            } => {
                if node_or_cc == remote::CC_INSTANCE {
                    infra.ssh_cc(&command.join(" "), false)
                } else {
                    infra.ssh_node(&node_or_cc, &command.join(" "))
                }
            }
            RemoteSubcommand::Load { args } => {
                // Run `spammer` from the Control Center server (backpressure mode, the default)
                let cmd = ["./spammer.sh", "nodes", "--nodes-path", "nodes.json"];
                let mut cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
                cmd.extend(args);

                infra.ssh_cc(&cmd.join(" "), false)
            }
            RemoteSubcommand::Spam { args } => {
                // Run `spammer` from the Control Center server (fire-and-forget mode)
                let cmd = [
                    "./spammer.sh",
                    "nodes",
                    "--nodes-path",
                    "nodes.json",
                    "--fire-and-forget",
                ];
                let mut cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
                cmd.extend(args);

                infra.ssh_cc(&cmd.join(" "), false)
            }
            RemoteSubcommand::Export {
                path,
                exclude_terraform,
            } => {
                let output_path =
                    path.unwrap_or_else(|| self.dir.join(format!("{}-export.json", self.name)));
                infra::export::export_testnet(
                    &self.dir,
                    &output_path,
                    &self.name,
                    &self.manifest_path,
                    exclude_terraform,
                )
            }
            // File import handled in main(); start SSM tunnels so quake commands work immediately
            RemoteSubcommand::Import { .. } => infra.ssm_tunnels.start().await,
        }
    }

    /// Generate and send transaction load to a node
    pub async fn load(&self, target_nodes: Vec<NodeName>, config: &spammer::Config) -> Result<()> {
        // Validate arguments
        self.manifest.contain_nodes(&target_nodes)?;

        let mut target_nodes = self.nodes_metadata.expand_to_nodes_list(&target_nodes)?;

        // If no target nodes are provided, use all nodes
        if target_nodes.is_empty() {
            target_nodes = self.nodes_metadata.node_names();
        }

        // Build EL WebSocket URLs of target nodes
        let target_ws_urls = self.nodes_metadata.to_execution_ws_urls(&target_nodes);

        // Calculate from genesis the number of extra prefunded accounts and update the config
        let num_extra_accounts = genesis::num_prefunded_accounts(
            &self.dir.join("assets").join("genesis.json"),
            self.manifest.num_validators(),
        )?;

        // Store latency CSV under .quake/results/<testnet-name>/
        let testnet_name = self
            .dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| eyre::eyre!("cannot derive testnet name from dir"))?;
        let csv_dir = Path::new(QUAKE_DIR).join("results").join(testnet_name);

        let config = spammer::Config {
            max_num_accounts: std::cmp::min(num_extra_accounts, config.max_num_accounts),
            csv_dir: Some(csv_dir),
            ..*config
        };

        let spammer = Spammer::new(target_ws_urls, &config).await?;
        spammer.run().await
    }

    pub(crate) async fn valset_update(&self, targets: Vec<ValidatorPowerUpdate>) -> Result<()> {
        let assets_dir = self.dir.join("assets");
        let mut controllers = Controllers::load_from_file(&assets_dir)?;
        let mut handles = Vec::new();

        for update in targets {
            let val_name = update.validator_name.clone();
            let node = self
                .nodes_metadata
                .get(&val_name)
                .ok_or_else(|| eyre!("unknown validator: {}", val_name))?;

            let mut controller = controllers.load_controller(&val_name)?;
            let rpc_url = node.execution.http_url.clone();
            let rpc_client = RpcClient::new(rpc_url, Duration::from_secs(5));

            let handle: JoinHandle<Result<(String, ControllerInfo)>> = tokio::spawn(async move {
                rpc_client
                    .update_validator_voting_power(&mut controller, update.new_voting_power)
                    .await
                    .wrap_err_with(|| {
                        format!("failed to update voting power for validator {val_name}")
                    })?;

                info!(node=%val_name, voting_power=%update.new_voting_power, "✅ Voting power updated");
                Ok((val_name, controller))
            });
            handles.push(handle);
        }

        for handle in handles {
            // first '?' unwraps the JoinHandle's return.
            // second '?' unwraps the Result inside the JoinHandle.
            let (validator_name, controller) = handle.await??;
            controllers.store_controller(&validator_name, controller);
        }

        controllers.store_to_file(assets_dir)?;

        Ok(())
    }

    // ===== helpers =====

    pub(crate) fn is_remote(&self) -> bool {
        self.infra_data.infra_type == InfraType::Remote
    }

    pub(crate) fn is_local(&self) -> bool {
        self.infra_data.infra_type == InfraType::Local
    }

    pub(crate) fn remote_infra(&self) -> Result<Arc<RemoteInfra>> {
        if self.is_remote() {
            Ok(Arc::downcast::<RemoteInfra>(self.infra.clone()).unwrap())
        } else {
            bail!("Testnet is not remote");
        }
    }

    fn local_infra(&self) -> Result<Arc<LocalInfra>> {
        if self.is_local() {
            Ok(Arc::downcast::<LocalInfra>(self.infra.clone()).unwrap())
        } else {
            bail!("Testnet is not local");
        }
    }

    /// Generate CLI flags for each node based on their configuration
    fn generate_cli_flags_for_nodes(&mut self) -> Result<()> {
        let consensus_ip_address_map = self.nodes_metadata.consensus_ip_addresses_map();

        for (name, node_metadata) in self.nodes_metadata.nodes.iter_mut() {
            let node_config = self.manifest.nodes.get(name);

            let peers_ips: Vec<String> =
                if let Some(peers) = node_config.and_then(|c| c.cl_persistent_peers.as_ref()) {
                    NodesMetadata::peer_consensus_ips(name, peers, &consensus_ip_address_map)?
                } else {
                    // Use all other nodes as peers
                    consensus_ip_address_map
                        .iter()
                        .filter(|(peer_name, _)| peer_name.as_str() != name)
                        .flat_map(|(_, private_ips)| private_ips.clone())
                        .collect()
                };

            let listen_ip = "0.0.0.0".to_string();

            // Generate CLI flags for the consensus layer
            let cli_flags = setup::generate_node_cli_flags(
                name,
                node_config,
                &listen_ip,
                &peers_ips,
                Some(self.images.cl.as_str()),
            );
            node_metadata.consensus.set_cli_flags(cli_flags);

            // Generate CLI flags for the consensus layer after upgrade
            let cli_flags = setup::generate_node_cli_flags(
                name,
                node_config,
                &listen_ip,
                &peers_ips,
                self.images.cl_upgrade.as_deref(),
            );
            node_metadata.consensus.set_cli_flags_upgraded(cli_flags);
        }

        Ok(())
    }

    fn print_monitoring_info(&self) {
        let (prometheus_port, grafana_port, blockscout_port) = self.infra_data.monitoring_ports();
        println!("  - Prometheus:     http://localhost:{prometheus_port}");
        println!("  - Grafana:        http://localhost:{grafana_port}");
        println!("  - Block explorer: http://localhost:{blockscout_port}");
        if self.is_remote() {
            println!("  - RPC proxy: http://localhost:{RPC_PROXY_SSM_PORT}/<node>/el, http://localhost:{RPC_PROXY_SSM_PORT}/<node>/cl, ws://localhost:{RPC_PROXY_SSM_PORT}/<node>/el/ws");
            println!("               http://localhost:{RPC_PROXY_SSM_PORT}/nodes, http://localhost:{RPC_PROXY_SSM_PORT}/health");
            println!("  - Pprof proxy: http://localhost:{PPROF_PROXY_SSM_PORT}/nodes, http://localhost:{PPROF_PROXY_SSM_PORT}/health");
        }
    }
}
