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

/// Random manifests generator.
///
/// It generates a manifest file with random configuration for the testnet based on the seed.
/// Manifest parameters are generated based on a predefined per-parameter distributions.
/// This is the minimal implementation that serves as a baseline for future randomization.
use arc_consensus_types::{
    Config as ClConfigOverride, ConsensusConfig, Height, LogFormat, LogLevel, LoggingConfig,
    PruningConfig, RpcConfig, RuntimeConfig, ValueSyncConfig,
};
use std::time::Duration;

use crate::latency::{Region, AWS_LATENCY_MATRIX};
use crate::manifest::subnets::Subnets;
use crate::manifest::{
    self, DockerImages, ElBuilderConfig, ElConfigOverride, ElEngineConfig, ElTxpoolConfig,
    EngineApiConnection, Manifest, Node, NodeType,
};
use crate::node::{NodeName, SubnetName};
use color_eyre::eyre::{eyre, Context, Result};
use indexmap::IndexMap;
use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use std::path::Path;
use strum::IntoEnumIterator;
use strum_macros::{Display, EnumIter, EnumString};
use tracing::{debug, info};

/// Network topology configuration for generated manifests
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, Display, EnumIter)]
pub enum NetworkTopology {
    /// Single validator node
    #[strum(serialize = "1node")]
    Single,
    /// Five validator nodes (default)
    #[strum(serialize = "5nodes")]
    FiveNodes,
    /// Complex topology: two sentry groups (1–4 validators each, fully meshed behind sentry-1/sentry-2),
    /// a relayer connected to both sentries and to 1–2 full nodes (themselves fully meshed).
    /// All nodes use persistent peer connections.
    #[strum(serialize = "complex")]
    Complex,
}

/// Starting height randomization strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, Display, EnumIter)]
pub enum HeightStrategy {
    /// All nodes start at height 0 (None)
    #[strum(serialize = "h0")]
    AllZero,
    /// Some nodes start at height 100 with probability 30-50%
    #[strum(serialize = "h100")]
    SomeAtHundred,
}

/// Region assignment strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, Display, EnumIter)]
pub enum RegionStrategy {
    /// Single region assigned to all nodes
    #[strum(serialize = "singleregion")]
    SingleRegion,
    /// Uniform random across all available regions
    #[strum(serialize = "uniform")]
    UniformRandom,
    /// Most nodes in a single region or nearby regions, rest in far regions
    #[strum(serialize = "clustered")]
    Clustered,
}

/// Configuration for generating random manifests
#[derive(Debug, Clone)]
pub struct GenerationConfig {
    pub topology: NetworkTopology,
    pub height_strategy: HeightStrategy,
    pub region_strategy: RegionStrategy,
}

/// Generates a given number of random manifests per each combination and saves them to the output directory.
///
/// # Arguments
///
/// * `count` - The number of manifests to generate per each combination.
/// * `output_dir` - The directory to save the manifests to.
/// * `seed` - The seed to use for the random number generator.
pub fn generate_manifests(count: usize, output_dir: &Path, seed: Option<u64>) -> Result<()> {
    if count == 0 {
        return Err(eyre!("Count must be at least 1"));
    }

    // Create output directory if it doesn't exist
    std::fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "Failed to create output directory: {}",
            output_dir.display()
        )
    })?;

    let base_seed = seed.unwrap_or_else(rand::random);

    // Generate all combinations of topology, height strategy, and region strategy
    let topologies = NetworkTopology::iter().skip(1);
    let height_strategies = HeightStrategy::iter();
    let region_strategies = RegionStrategy::iter();

    let mut seed_counter = base_seed;
    let mut generated_count = 0;

    // Generate the single node topology manifests separately
    // Note: only one combination for the single node topology is needed,
    //       since each combination generates redundant manifest.
    generate_manifests_for_combination(
        NetworkTopology::Single,
        HeightStrategy::AllZero,
        RegionStrategy::SingleRegion,
        count,
        output_dir,
        seed_counter,
    )?;

    generated_count += count;
    seed_counter = seed_counter.wrapping_add(count as u64);

    for topology in topologies {
        for height_strategy in height_strategies.clone() {
            for region_strategy in region_strategies.clone() {
                if region_strategy != RegionStrategy::SingleRegion
                    && topology == NetworkTopology::Complex
                {
                    continue;
                }

                generate_manifests_for_combination(
                    topology,
                    height_strategy,
                    region_strategy,
                    count,
                    output_dir,
                    seed_counter,
                )?;

                generated_count += count;
                seed_counter = seed_counter.wrapping_add(count as u64);
            }
        }
    }

    info!(dir=%output_dir.display(), "Successfully generated {generated_count} manifest files.");
    Ok(())
}

fn generate_manifests_for_combination(
    topology: NetworkTopology,
    height_strategy: HeightStrategy,
    region_strategy: RegionStrategy,
    count: usize,
    output_dir: &Path,
    seed_counter: u64,
) -> Result<()> {
    let config = GenerationConfig {
        topology,
        height_strategy,
        region_strategy,
    };

    info!(
        "Generating {count} manifests for combination: topology={}, height={}, region={}",
        topology, height_strategy, region_strategy
    );

    for seed in seed_counter..seed_counter.wrapping_add(count as u64) {
        let manifest = Manifest::generate_random(seed, &config)?;

        // Extract the name from the manifest for the filename
        let filename = format!(
            "{}.toml",
            manifest
                .name
                .as_ref()
                .unwrap_or(&format!("quake-random-{}", seed))
        );
        let output_path = output_dir.join(&filename);

        manifest.into_file(&output_path)?;

        debug!(
            "Generated manifest with seed {seed} -> {}",
            output_path.display()
        );
    }
    Ok(())
}

impl Manifest {
    /// Generate a random manifest with the given configuration.
    pub fn generate_random(seed: u64, config: &GenerationConfig) -> Result<Self> {
        let manifest = Self::random_parameters(seed, config)?;
        manifest
            .validate()
            .context("Failed to validate randomly generated manifest. This might indicate a bug in the generator.")?;
        Ok(manifest)
    }

    fn random_parameters(seed: u64, config: &GenerationConfig) -> Result<Self> {
        let mut rng = StdRng::seed_from_u64(seed);

        // Spec: 70% rpc, 30% ipc
        let engine_api_connection = if rng.gen_bool(0.7) {
            EngineApiConnection::Rpc
        } else {
            EngineApiConnection::Ipc
        };
        // Spec: 95% zero6, 5% zero5
        let el_init_hardfork = Some(
            match rng.gen_range(0..20u32) {
                0 => "zero5",
                _ => "zero6",
            }
            .to_string(),
        );

        let mut nodes = Self::generate_nodes_for_topology(config.topology, &mut rng);
        nodes = Self::apply_height_strategy(nodes, config.height_strategy, &mut rng);
        nodes = Self::apply_region_strategy(nodes, config.region_strategy, &mut rng)
            .context("Failed to apply region strategy")?;

        for (_name, node) in nodes.iter_mut() {
            node.cl_config = Self::random_cl_node_config(&mut rng);
            node.el_config = Self::random_el_node_config(&mut rng);
        }

        let config_suffix = format!(
            "{}-{}-{}",
            config.topology, config.height_strategy, config.region_strategy
        );

        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = nodes
            .iter()
            .map(|(name, _)| (name.clone(), manifest::default_subnet_singleton()))
            .collect();

        Ok(Self {
            name: Some(format!("quake-random-{}-{}", seed, config_suffix)),
            description: Some(format!(
                "Random manifest generated with seed {} (topology: {}, height: {}, region: {})",
                seed, config.topology, config.height_strategy, config.region_strategy
            )),
            latency_emulation: true,
            monitoring_bind_host: None,
            engine_api_connection: Some(engine_api_connection),
            subnets: Subnets::new(&node_subnets),
            images: DockerImages::default(),
            nodes,
            el_init_hardfork,
        })
    }

    /// Build random per-node CL (Consensus Layer) config.
    fn random_cl_node_config(rng: &mut StdRng) -> ClConfigOverride {
        let parallel_requests = if rng.gen_bool(0.7) {
            5
        } else {
            rng.gen_range(1..=20)
        };

        // Runtime: 30% single_threaded, 70% multi_threaded; worker_threads 1-16 when multi.
        let runtime = if rng.gen_bool(0.7) {
            RuntimeConfig::MultiThreaded {
                worker_threads: rng.gen_range(1..=16),
            }
        } else {
            RuntimeConfig::SingleThreaded
        };

        let distance: u64 = match rng.gen_range(0..100) {
            0..=29 => 0,
            30..=69 => rng.gen_range(100..=1000),
            _ => rng.gen_range(1000..=10000),
        };
        let min_height: u64 = if rng.gen_bool(0.8) {
            0
        } else {
            rng.gen_range(1..=1000)
        };

        ClConfigOverride {
            logging: LoggingConfig {
                log_level: LogLevel::Debug,
                log_format: LogFormat::Plaintext,
            },
            consensus: ConsensusConfig {
                enabled: true,
                ..Default::default()
            },
            value_sync: ValueSyncConfig {
                enabled: true,
                parallel_requests,
                batch_size: rng.gen_range(1..=1000),
                request_timeout: Duration::from_secs(rng.gen_range(5..=30)),
                status_update_interval: Duration::from_secs(rng.gen_range(0..=10)),
                ..Default::default()
            },
            runtime,
            prune: PruningConfig {
                certificates_distance: distance,
                certificates_before: Height::new(min_height),
            },
            rpc: RpcConfig {
                // RPC: always enabled so quake wait height and test can query block height
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Build random per-node EL (Execution Layer) config override.
    fn random_el_node_config(rng: &mut StdRng) -> ElConfigOverride {
        ElConfigOverride {
            txpool: ElTxpoolConfig {
                pending_max_count: Some(rng.gen_range(5000..=15000)),
                basefee_max_count: Some(rng.gen_range(5000..=15000)),
                queued_max_count: Some(rng.gen_range(5000..=15000)),
                max_account_slots: Some(rng.gen_range(8..=32)),
                lifetime: Some(rng.gen_range(3600..=21600)),
                max_batch_size: Some(rng.gen_range(1..=10)),
                ..ElTxpoolConfig::default()
            },
            builder: ElBuilderConfig {
                interval: Some(rng.gen_range(1..=5)),
                deadline: rng.gen_range(1..=12),
                max_tasks: Some(rng.gen_range(1..=5)),
            },
            engine: ElEngineConfig {
                disable_state_cache: Some(rng.gen_bool(0.1)),
                cross_block_cache_size: Some(rng.gen_range(2048..=8192)),
                ..ElEngineConfig::default()
            },
            ..ElConfigOverride::default()
        }
    }

    fn generate_nodes_for_topology(
        topology: NetworkTopology,
        rng: &mut StdRng,
    ) -> IndexMap<String, Node> {
        match topology {
            NetworkTopology::Single => {
                let mut nodes = IndexMap::new();
                nodes.insert(
                    "val-1".to_string(),
                    Node::default().with_node_type(NodeType::Validator),
                );
                nodes
            }
            NetworkTopology::FiveNodes => (1..=5)
                .map(|i| {
                    (
                        format!("val-{}", i),
                        Node::default().with_node_type(NodeType::Validator),
                    )
                })
                .collect(),
            NetworkTopology::Complex => Self::apply_complex_topology(rng),
        }
    }

    /// Apply complex topology with persistent peer connections
    fn apply_complex_topology(rng: &mut StdRng) -> IndexMap<String, Node> {
        const MAX_NUM_OF_VALIDATORS_SENTRY_1: usize = 3;
        const MAX_NUM_OF_VALIDATORS_SENTRY_2: usize = 3;
        const NUM_OF_FULL_NODES: usize = 1;

        let mut nodes = IndexMap::new();

        let num_of_validators_sentry1 = rng.gen_range(1..=MAX_NUM_OF_VALIDATORS_SENTRY_1);
        let num_of_validators_sentry2 = rng.gen_range(1..=MAX_NUM_OF_VALIDATORS_SENTRY_2);

        let mut sentry1_peers = Self::generate_fully_meshed_peers(
            &mut nodes,
            0,
            num_of_validators_sentry1,
            &["sentry-1".to_string()],
            "val",
            NodeType::Validator,
        );
        let mut sentry2_peers = Self::generate_fully_meshed_peers(
            &mut nodes,
            num_of_validators_sentry1,
            num_of_validators_sentry2,
            &["sentry-2".to_string()],
            "val",
            NodeType::Validator,
        );
        let full_nodes_peers = Self::generate_fully_meshed_peers(
            &mut nodes,
            0,
            NUM_OF_FULL_NODES,
            &["relayer".to_string()],
            "full",
            NodeType::NonValidator,
        );

        let relayer_peers = ["sentry-1", "sentry-2"]
            .into_iter()
            .chain(full_nodes_peers.iter().map(|s| s.as_str()));
        let relayer_peers_vec: Vec<String> = relayer_peers.map(String::from).collect();
        nodes.insert(
            "relayer".to_string(),
            Node::default()
                .with_cl_persistent_peers(relayer_peers_vec.clone())
                .with_el_trusted_peers(relayer_peers_vec)
                .with_node_type(NodeType::NonValidator),
        );

        sentry1_peers.extend(["relayer".to_string(), "sentry-2".to_string()]);
        sentry2_peers.extend(["relayer".to_string(), "sentry-1".to_string()]);

        nodes.insert(
            "sentry-1".to_string(),
            Node::default()
                .with_cl_persistent_peers(sentry1_peers.clone())
                .with_el_trusted_peers(sentry1_peers)
                .with_node_type(NodeType::NonValidator),
        );

        nodes.insert(
            "sentry-2".to_string(),
            Node::default()
                .with_cl_persistent_peers(sentry2_peers.clone())
                .with_el_trusted_peers(sentry2_peers)
                .with_node_type(NodeType::NonValidator),
        );

        nodes
    }

    /// Adds fully meshed peers to the nodes map and returns the list of peers
    /// Returns the list of peers for the given prefix
    fn generate_fully_meshed_peers(
        nodes: &mut IndexMap<String, Node>,
        current_num_of_nodes_given_prefix: usize,
        num_of_validators: usize,
        extra_peers: &[String],
        prefix: &str,
        node_type: NodeType,
    ) -> Vec<String> {
        let start = 1 + current_num_of_nodes_given_prefix;
        let indices = start..start + num_of_validators;
        let peers: Vec<String> = indices.clone().map(|i| format!("{prefix}-{i}")).collect();

        indices.for_each(|i| {
            let node_peers: Vec<String> = peers
                .iter()
                .filter(|p| p.as_str() != format!("{}-{}", prefix, i).as_str())
                .cloned()
                .chain(extra_peers.iter().cloned())
                .collect();
            let node = Node::default()
                .with_cl_persistent_peers(node_peers.clone())
                .with_el_trusted_peers(node_peers)
                .with_node_type(node_type.clone());
            nodes.insert(format!("{}-{}", prefix, i), node);
        });
        peers
    }

    fn apply_height_strategy(
        mut nodes: IndexMap<String, Node>,
        strategy: HeightStrategy,
        rng: &mut StdRng,
    ) -> IndexMap<String, Node> {
        match strategy {
            HeightStrategy::AllZero => nodes,
            HeightStrategy::SomeAtHundred => {
                let mut validator_names: Vec<String> = nodes
                    .iter()
                    .filter(|(_, node)| node.node_type == NodeType::Validator)
                    .map(|(k, _)| k.clone())
                    .collect();
                let mut full_node_names: Vec<String> = nodes
                    .iter()
                    .filter(|(_, node)| node.node_type != NodeType::Validator)
                    .filter(|(name, _)| name.starts_with("full"))
                    .map(|(k, _)| k.clone())
                    .collect();

                let num_delayed_validators = validator_names.len().saturating_sub(1) / 3;
                let num_delayed_full_nodes = full_node_names.len() / 2;

                validator_names.shuffle(rng);
                full_node_names.shuffle(rng);

                for node_name in validator_names.iter().take(num_delayed_validators) {
                    if let Some(node) = nodes.get_mut(node_name) {
                        node.start_at = Some(100);
                    }
                }

                for node_name in full_node_names.iter().take(num_delayed_full_nodes) {
                    if let Some(node) = nodes.get_mut(node_name) {
                        node.start_at = Some(100);
                    }
                }

                nodes
            }
        }
    }

    fn apply_region_strategy(
        mut nodes: IndexMap<String, Node>,
        strategy: RegionStrategy,
        rng: &mut StdRng,
    ) -> Result<IndexMap<String, Node>> {
        if nodes.len() <= 1 {
            return Ok(nodes);
        }

        match strategy {
            RegionStrategy::SingleRegion => {
                let random_region = *Region::all().choose(rng).ok_or(eyre!("No regions"))?;
                for (_, node) in nodes.iter_mut() {
                    node.region = Some(random_region.to_string());
                }

                Ok(nodes)
            }
            RegionStrategy::UniformRandom => {
                let regions = Region::all();
                for (_, node) in nodes.iter_mut() {
                    node.region = Some(regions.choose(rng).ok_or(eyre!("No regions"))?.to_string());
                }

                Ok(nodes)
            }
            RegionStrategy::Clustered => {
                let all_regions = Region::all();
                let primary_region = all_regions.choose(rng).ok_or(eyre!("No regions"))?;
                let (nearby_regions, far_regions) =
                    Self::categorize_regions_by_latency(primary_region);

                let nearby_pool = [
                    *primary_region,
                    *nearby_regions
                        .choose(rng)
                        .ok_or(eyre!("No nearby regions"))?,
                ];

                let num_nodes = nodes.len();
                let num_nearby = rng.gen_range(1..=(num_nodes * 8 / 10).max(1));

                let mut node_names: Vec<String> = nodes.keys().cloned().collect();
                node_names.shuffle(rng);

                for (i, node_name) in node_names.iter().enumerate() {
                    if let Some(node) = nodes.get_mut(node_name) {
                        let region = if i < num_nearby {
                            nearby_pool.choose(rng).ok_or(eyre!("No nearby regions"))?
                        } else {
                            far_regions.choose(rng).unwrap_or(primary_region)
                        };
                        node.region = Some(region.to_string());
                    }
                }

                Ok(nodes)
            }
        }
    }

    /// Categorize regions into nearby and far based on latency from a primary region
    /// Nearby regions have latency < NEARBY_THRESHOLD_MS, all others are considered far.
    /// Returns (nearby_regions, far_regions)
    fn categorize_regions_by_latency(primary: &Region) -> (Vec<Region>, Vec<Region>) {
        const NEARBY_THRESHOLD_MS: u32 = 50;

        let nearby = Region::all()
            .into_iter()
            .filter(|region| {
                AWS_LATENCY_MATRIX[*primary as usize][*region as usize] < NEARBY_THRESHOLD_MS
            })
            .collect();

        let far = Region::all()
            .into_iter()
            .filter(|region| {
                AWS_LATENCY_MATRIX[*primary as usize][*region as usize] >= NEARBY_THRESHOLD_MS
            })
            .collect();

        (nearby, far)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_manifest_roundtrip() {
        use crate::manifest::raw::RawManifest;

        for topology in NetworkTopology::iter() {
            for height_strategy in HeightStrategy::iter() {
                for region_strategy in RegionStrategy::iter() {
                    let config = GenerationConfig {
                        topology,
                        height_strategy,
                        region_strategy,
                    };
                    let mut manifest = Manifest::generate_random(42, &config).unwrap();

                    // TODO: ByteSize v1.3 has an issue - the serializer produces lossy output.
                    //       This is fixed in a newer version; the dependency should be upgraded upstream first.
                    for (_name, node) in manifest.nodes.iter_mut() {
                        node.cl_config.value_sync.max_request_size = bytesize::ByteSize::kib(1000);
                        node.cl_config.value_sync.max_response_size = bytesize::ByteSize::kib(1000);
                        node.cl_config.consensus.p2p.rpc_max_size = bytesize::ByteSize::kib(1000);
                        node.cl_config.consensus.p2p.pubsub_max_size =
                            bytesize::ByteSize::kib(1000);
                    }
                    manifest
                        .validate()
                        .context("Failed to validate manifest")
                        .unwrap();
                    let raw_manifest = RawManifest::try_from(manifest.clone()).unwrap();
                    let manifest_from_raw = Manifest::try_from(raw_manifest)
                        .context("Failed to parse manifest")
                        .unwrap();
                    assert_eq!(manifest_from_raw, manifest);
                }
            }
        }
    }

    #[test]
    fn test_single_node_topology() {
        let config = GenerationConfig {
            topology: NetworkTopology::Single,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::SingleRegion,
        };
        let manifest = Manifest::generate_random(1, &config).unwrap();
        assert_eq!(manifest.nodes.len(), 1);
    }

    #[test]
    fn test_five_nodes_topology() {
        let config = GenerationConfig {
            topology: NetworkTopology::FiveNodes,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::SingleRegion,
        };
        let manifest = Manifest::generate_random(2, &config).unwrap();
        assert_eq!(manifest.nodes.len(), 5);
    }

    #[test]
    fn test_complex_topology() {
        let config = GenerationConfig {
            topology: NetworkTopology::Complex,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::SingleRegion,
        };
        let manifest = Manifest::generate_random(3, &config).unwrap();

        // Verify complex topology has persistent peers configured
        let sentry_1 = manifest.nodes.get("sentry-1");
        let sentry_2 = manifest.nodes.get("sentry-2");
        let relayer = manifest.nodes.get("relayer");

        assert!(
            sentry_1.is_some(),
            "sentry-1 should exist in complex topology"
        );
        assert!(
            sentry_2.is_some(),
            "sentry-2 should exist in complex topology"
        );
        assert!(
            relayer.is_some(),
            "relayer should exist in complex topology"
        );

        // Verify complex topology structure
        // All validators should have persistent peers configured
        let validators: Vec<_> = manifest
            .nodes
            .iter()
            .filter(|(_, n)| n.node_type == NodeType::Validator)
            .collect();
        assert!(
            validators
                .iter()
                .all(|(_, n)| n.cl_persistent_peers.is_some()),
            "All validators should have persistent peers configured"
        );

        // Verify relayer exists and is connected to sentries and other nodes
        let relayer_node = manifest.nodes.get("relayer");
        assert!(
            relayer_node.is_some(),
            "Relayer should exist in complex topology"
        );
        let relayer_peers = relayer_node.unwrap().cl_persistent_peers.as_ref().unwrap();
        assert!(
            relayer_peers.contains(&"sentry-1".to_string()),
            "Relayer should be connected to sentry-1"
        );
        assert!(
            relayer_peers.contains(&"sentry-2".to_string()),
            "Relayer should be connected to sentry-2"
        );

        // Verify third-party full nodes are connected to the relayer
        let full_nodes: Vec<_> = manifest
            .nodes
            .iter()
            .filter(|(name, n)| n.node_type == NodeType::NonValidator && name.starts_with("full"))
            .collect();
        assert!(
            full_nodes.iter().all(|(_, n)| {
                let peers = n.cl_persistent_peers.as_ref().unwrap();
                peers.contains(&"relayer".to_string())
            }),
            "Full nodes should be connected to the relayer and the sentry nodes"
        );

        // Verify sentry nodes exist and have persistent peers
        assert!(
            manifest.nodes.get("sentry-1").is_some(),
            "Sentry-1 should exist in complex topology"
        );
        assert!(
            manifest.nodes.get("sentry-2").is_some(),
            "Sentry-2 should exist in complex topology"
        );

        // Verify persistent peers symmetry: if S has P in cl_persistent_peers, then P must have S in cl_persistent_peers
        for (s_name, s_node) in manifest.nodes.iter() {
            let Some(s_peers) = s_node.cl_persistent_peers.as_ref() else {
                continue;
            };
            for p_name in s_peers {
                let p_node = manifest.nodes.get(p_name).unwrap_or_else(|| {
                    panic!("Node '{s_name}' has peer '{p_name}' in cl_persistent_peers but '{p_name}' does not exist in manifest")
                });
                let p_peers = p_node.cl_persistent_peers.as_ref().unwrap_or_else(|| {
                    panic!("Node '{s_name}' has peer '{p_name}' in cl_persistent_peers but '{p_name}' has no cl_persistent_peers")
                });
                assert!(
                    p_peers.contains(s_name),
                    "Persistent peers symmetry violated: node '{s_name}' has '{p_name}' in cl_persistent_peers, but '{p_name}' does not have '{s_name}' in its cl_persistent_peers (peers of '{p_name}': {:?})",
                    p_peers
                );
            }
        }
    }

    #[test]
    fn test_height_strategies() {
        let config_all_zero = GenerationConfig {
            topology: NetworkTopology::FiveNodes,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::SingleRegion,
        };
        let manifest = Manifest::generate_random(4, &config_all_zero).unwrap();
        assert!(manifest.nodes.values().all(|n| n.start_at.is_none()));

        let config_some_hundred = GenerationConfig {
            topology: NetworkTopology::FiveNodes,
            height_strategy: HeightStrategy::SomeAtHundred,
            region_strategy: RegionStrategy::SingleRegion,
        };
        let manifest = Manifest::generate_random(5, &config_some_hundred).unwrap();
        let delayed_count = manifest
            .nodes
            .values()
            .filter(|n| n.start_at.is_some())
            .count();
        assert!(
            delayed_count > 0,
            "At least one node should have delayed start"
        );
        assert!(
            delayed_count < manifest.nodes.len(),
            "Not all nodes should have delayed start"
        );
    }

    #[test]
    fn test_region_strategies() {
        // Test single region
        let config_no_region = GenerationConfig {
            topology: NetworkTopology::FiveNodes,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::SingleRegion,
        };
        let manifest = Manifest::generate_random(6, &config_no_region).unwrap();
        let first_region = manifest
            .nodes
            .values()
            .next()
            .and_then(|n| n.region.as_ref())
            .expect("Region should be assigned");
        assert!(
            manifest
                .nodes
                .values()
                .all(|n| n.region.as_ref() == Some(first_region)),
            "Not all nodes have the same region",
        );

        // Test uniform random regions
        let config_uniform = GenerationConfig {
            topology: NetworkTopology::FiveNodes,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::UniformRandom,
        };
        let manifest = Manifest::generate_random(7, &config_uniform).unwrap();
        assert!(manifest.latency_emulation);
        assert!(manifest.nodes.values().all(|n| n.region.is_some()));

        // Test clustered regions
        let config_clustered = GenerationConfig {
            topology: NetworkTopology::FiveNodes,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::Clustered,
        };
        let manifest = Manifest::generate_random(8, &config_clustered).unwrap();
        assert!(manifest.latency_emulation);
        assert!(manifest.nodes.values().all(|n| n.region.is_some()));

        // In clustered mode, we expect some concentration of regions
        // (not all nodes in different regions like uniform random would likely produce)
        let unique_regions: std::collections::HashSet<_> = manifest
            .nodes
            .values()
            .filter_map(|n| n.region.as_ref())
            .collect();
        // With 5 nodes and clustering, we expect fewer unique regions than uniform random
        assert!(
            unique_regions.len() <= 4,
            "Clustered should concentrate nodes in fewer regions"
        );
    }

    #[test]
    fn test_cl_node_config_structure_and_bounds() {
        let config = GenerationConfig {
            topology: NetworkTopology::FiveNodes,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::SingleRegion,
        };
        let manifest = Manifest::generate_random(100, &config).unwrap();

        for (node_id, node) in &manifest.nodes {
            let g = &node.cl_config;
            assert!(
                g != &ClConfigOverride::default(),
                "node {node_id} cl_config should not be default/empty"
            );

            // Logging: fixed debug, plaintext
            assert_eq!(g.logging.log_level, LogLevel::Debug);
            assert_eq!(g.logging.log_format, LogFormat::Plaintext);

            // Consensus: always enabled
            assert!(g.consensus.enabled);

            // ValueSync: enabled, parallel_requests and batch_size in spec ranges
            assert!(g.value_sync.enabled);
            let pr = g.value_sync.parallel_requests as i64;
            assert!((1..=20).contains(&pr), "parallel_requests = {pr}");
            let bs = g.value_sync.batch_size as i64;
            assert!((1..=1000).contains(&bs), "batch_size = {bs}");

            // Runtime: single_threaded or multi_threaded with 1-16 worker threads
            match g.runtime {
                RuntimeConfig::SingleThreaded => {}
                RuntimeConfig::MultiThreaded { worker_threads } => {
                    assert!(
                        (1..=16).contains(&worker_threads),
                        "worker_threads = {worker_threads}"
                    );
                }
            }

            // Prune: certificates_distance and certificates_before in spec ranges
            let bi = g.prune.certificates_distance;
            assert!(bi <= 10000, "certificates_distance = {bi}");
            let mh = g.prune.certificates_before.as_u64();
            assert!(mh <= 1000, "certificates_before = {mh}");

            // RPC: always enabled
            assert!(g.rpc.enabled);
        }
    }

    #[test]
    fn test_el_node_config_structure_and_bounds() {
        let config = GenerationConfig {
            topology: NetworkTopology::FiveNodes,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::SingleRegion,
        };
        let manifest = Manifest::generate_random(200, &config).unwrap();

        for (node_id, node) in &manifest.nodes {
            let g = &node.el_config;
            assert!(
                g != &ElConfigOverride::default(),
                "node {node_id} el_config should not be default/empty"
            );

            let pending = g.txpool.pending_max_count.unwrap() as i64;
            assert!(
                (5000..=15000).contains(&pending),
                "pending-max-count out of range"
            );
            let queued = g.txpool.queued_max_count.unwrap() as i64;
            assert!(
                (5000..=15000).contains(&queued),
                "queued-max-count out of range"
            );
            let batch = g.txpool.max_batch_size.unwrap() as i64;
            assert!((1..=10).contains(&batch), "max-batch-size out of range");

            let interval = g.builder.interval.unwrap() as i64;
            assert!((1..=5).contains(&interval), "builder.interval out of range");
            let deadline = g.builder.deadline as i64;
            assert!(
                (1..=12).contains(&deadline),
                "builder.deadline out of range"
            );
            let max_tasks = g.builder.max_tasks.unwrap() as i64;
            assert!(
                (1..=5).contains(&max_tasks),
                "builder.max-tasks out of range"
            );

            assert!(g.engine.disable_state_cache.is_some());
            let cache = g.engine.cross_block_cache_size.unwrap() as i64;
            assert!(
                (2048..=8192).contains(&cache),
                "cross-block-cache-size = {cache}"
            );
        }
    }

    #[test]
    fn test_el_init_hardfork_and_engine_api_connection() {
        let config = GenerationConfig {
            topology: NetworkTopology::Single,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::SingleRegion,
        };

        let manifest = Manifest::generate_random(300, &config).unwrap();
        let hardfork = manifest
            .el_init_hardfork
            .as_deref()
            .expect("el_init_hardfork should be set");
        assert!(
            hardfork == "zero5" || hardfork == "zero6",
            "el_init_hardfork = {hardfork}"
        );

        let conn = manifest
            .engine_api_connection
            .expect("engine_api_connection should be set");
        assert!(
            matches!(conn, EngineApiConnection::Rpc | EngineApiConnection::Ipc),
            "engine_api_connection = {conn:?}"
        );
    }

    #[test]
    fn test_different_seeds_produce_different_configs() {
        let config = GenerationConfig {
            topology: NetworkTopology::Single,
            height_strategy: HeightStrategy::AllZero,
            region_strategy: RegionStrategy::SingleRegion,
        };

        let m1 = Manifest::generate_random(500, &config).unwrap();
        let m2 = Manifest::generate_random(501, &config).unwrap();

        // At least one of global_config, global_el_config, el_init_hardfork, or engine_api_connection should differ
        let configs_differ = m1.el_init_hardfork != m2.el_init_hardfork
            || m1.engine_api_connection != m2.engine_api_connection;
        assert!(
            configs_differ,
            "different seeds should yield different configs"
        );
    }
}
