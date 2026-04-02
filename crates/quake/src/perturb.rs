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

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use clap::Subcommand;
use color_eyre::eyre::{bail, Context, Result};
use rand::rngs::StdRng;
use rand::{seq::IteratorRandom, Rng, SeedableRng};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::infra::InfraProvider;
use crate::node::{Container, ContainerName, SubnetName, UPGRADED_SUFFIX};
use crate::nodes::{NodeOrContainerName, NodesMetadata};
use crate::parse_duration;

pub(crate) const PERTURB_MIN_TIME_OFF: &str = "5s";
pub(crate) const PERTURB_MAX_TIME_OFF: &str = "10s";
const CHAOS_DURATION: &str = "5m";
const CHAOS_MIN_WAIT_BETWEEN_PERTURBATIONS: &str = "10s";
const CHAOS_MAX_WAIT_BETWEEN_PERTURBATIONS: &str = "20s";

/// A perturbation is an action that temporarily affects the node components.
#[derive(Debug, Subcommand, Clone)]
pub(crate) enum Perturbation {
    /// Disconnect containers from the network, wait some time, then reconnect them.
    Disconnect {
        /// Node or container name as defined in the manifest
        targets: Vec<NodeOrContainerName>,
        /// How long the node should be offline.
        #[arg(short, long, value_parser = parse_duration)]
        time_off: Option<Duration>,
    },
    /// Kill containers, wait some time, then restart them.
    Kill {
        /// Node or container name as defined in the manifest
        targets: Vec<NodeOrContainerName>,
        /// How long the node should remain killed before being restarted.
        #[arg(short, long, value_parser = parse_duration)]
        time_off: Option<Duration>,
    },
    /// Pause containers, wait some time, then unpause them.
    Pause {
        /// Node or container name as defined in the manifest
        targets: Vec<NodeOrContainerName>,
        /// How long the node should be paused.
        #[arg(short, long, value_parser = parse_duration)]
        time_off: Option<Duration>,
    },
    /// Restart containers.
    Restart {
        /// Node or container name as defined in the manifest
        targets: Vec<NodeOrContainerName>,
    },
    /// Upgrade containers by stopping them and restarting them with a different image tag.
    Upgrade {
        /// Node or container name as defined in the manifest
        targets: Vec<NodeOrContainerName>,
        /// How long the node should remain stopped before being restarted with upgraded image.
        #[arg(short, long, value_parser = parse_duration)]
        time_off: Option<Duration>,
    },
    /// Chaos testing: apply random perturbations to the given nodes or containers.
    ///
    /// Perturbations are applied during a specified time, at random intervals.
    ///
    /// Each perturbation is picked at random from an allowed list of perturbations and applied to a random subset of the targets.
    /// The subset of targets are at most 1/3 of all the targets.
    /// A target is either a Consensus Layer (CL) container or an Execution Layer (EL) container.
    ///
    /// The time the targets are offline before recovering from the last perturbation
    /// is a random value between the configured minimum and maximum time-off values.
    ///
    /// The time to wait before applying the next perturbation is a random value between
    /// the configured minimum and maximum wait values.
    #[clap(alias = "shake")]
    #[command(verbatim_doc_comment)]
    Chaos {
        /// Node or container name (as defined in the manifest) to target perturbations (all if not specified)
        targets: Vec<NodeOrContainerName>,
        /// Duration of the chaos test event
        #[arg(short, long, value_parser = parse_duration, default_value = CHAOS_DURATION)]
        duration: Duration,
        /// Perturbations to apply (comma-separated list of 'disconnect', 'kill', 'pause', 'restart', 'upgrade')
        #[arg(
            short = 'p',
            long,
            value_parser = Perturbation::from_str,
            value_delimiter = ',',
            default_value = "disconnect,kill,pause,restart"
        )]
        perturbations: Vec<Perturbation>,
        /// Minimum time to wait before applying the next perturbation
        #[arg(short = 'w', long, value_parser = parse_duration, default_value = CHAOS_MIN_WAIT_BETWEEN_PERTURBATIONS)]
        min_wait: Duration,
        /// Maximum time to wait before applying the next perturbation
        #[arg(short = 'W', long, value_parser = parse_duration, default_value = CHAOS_MAX_WAIT_BETWEEN_PERTURBATIONS)]
        max_wait: Duration,
    },
}

impl Perturbation {
    pub fn from_str(s: &str) -> Result<Self> {
        let targets = Vec::new();
        let time_off = None;
        match s.trim().to_lowercase().as_str() {
            "disconnect" => Ok(Perturbation::Disconnect { targets, time_off }),
            "kill" => Ok(Perturbation::Kill { targets, time_off }),
            "pause" => Ok(Perturbation::Pause { targets, time_off }),
            "restart" => Ok(Perturbation::Restart { targets }),
            "upgrade" => Ok(Perturbation::Upgrade {
                targets,
                time_off: None,
            }),
            "chaos" => bail!("chaos is not an individual perturbation"),
            value => bail!("invalid perturbation: {value}"),
        }
    }

    pub fn target_names(&self) -> &Vec<NodeOrContainerName> {
        match self {
            Perturbation::Disconnect { targets, .. } => targets,
            Perturbation::Kill { targets, .. } => targets,
            Perturbation::Pause { targets, .. } => targets,
            Perturbation::Restart { targets } => targets,
            Perturbation::Upgrade { targets, .. } => targets,
            Perturbation::Chaos { targets, .. } => targets,
        }
    }

    /// The list of containers overrides the targets in the perturbation.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply(
        &self,
        infra: &dyn InfraProvider,
        nodes_metadata: &NodesMetadata,
        containers: &[ContainerName],
        seed: u64,
        min_time_off: Duration,
        max_time_off: Duration,
        num_nodes: usize,
    ) -> Result<()> {
        let mut rng = StdRng::seed_from_u64(seed);
        match self {
            Perturbation::Disconnect { time_off, .. } => {
                apply_disconnect(
                    infra,
                    &nodes_metadata.to_containers(containers),
                    *time_off,
                    min_time_off,
                    max_time_off,
                    &mut rng,
                )
                .await?
            }
            Perturbation::Kill { time_off, .. } => {
                apply_kill(
                    infra,
                    containers,
                    *time_off,
                    min_time_off,
                    max_time_off,
                    &mut rng,
                )
                .await?;
            }
            Perturbation::Pause { time_off, .. } => {
                apply_pause(
                    infra,
                    containers,
                    *time_off,
                    min_time_off,
                    max_time_off,
                    &mut rng,
                )
                .await?;
            }
            Perturbation::Restart { .. } => {
                apply_restart(infra, containers).await?;
            }
            Perturbation::Upgrade { time_off, .. } => {
                apply_upgrade(
                    infra,
                    containers,
                    *time_off,
                    min_time_off,
                    max_time_off,
                    &mut rng,
                )
                .await?;
            }
            Perturbation::Chaos {
                duration,
                perturbations,
                min_wait,
                max_wait,
                ..
            } => {
                let containers_str = containers.join(", ");
                info!("🌪️ Chaos testing for {duration:?} on {containers_str}");

                apply_chaos(
                    infra,
                    nodes_metadata,
                    num_nodes,
                    containers,
                    *duration,
                    perturbations,
                    min_time_off,
                    max_time_off,
                    min_wait,
                    max_wait,
                    &mut rng,
                )
                .await?;

                info!("🌪️ Finished chaos testing on {containers_str}");
            }
        };
        Ok(())
    }
}

impl std::fmt::Display for Perturbation {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Perturbation::Disconnect {
                targets,
                time_off: _,
            } => {
                write!(f, "'disconnect {}'", targets.join(", "))
            }
            Perturbation::Kill {
                targets,
                time_off: _,
            } => {
                write!(f, "'kill {}'", targets.join(", "))
            }
            Perturbation::Pause {
                targets,
                time_off: _,
            } => {
                write!(f, "'pause {}'", targets.join(", "))
            }
            Perturbation::Restart { targets } => {
                write!(f, "'restart {}'", targets.join(", "))
            }
            Perturbation::Upgrade {
                targets,
                time_off: _,
            } => {
                write!(f, "'upgrade {}'", targets.join(", "))
            }
            Perturbation::Chaos { targets, .. } => {
                write!(f, "'chaos {}'", targets.join(", "))
            }
        }
    }
}

/// Generate a random duration between the min and max perturbation times
pub(crate) fn rand_duration(rng: &mut StdRng, min: Duration, max: Duration) -> Duration {
    let range = min.as_millis()..=max.as_millis();
    if range.is_empty() {
        warn!("range of random duration is empty: min ({min:?}) is greater or equal to max ({max:?}); using the lower value");
        return min;
    }
    Duration::from_millis(rng.gen_range(range) as u64)
}

async fn apply_disconnect(
    infra: &dyn InfraProvider,
    containers: &[&Container],
    time_off: Option<Duration>,
    min_time_off: Duration,
    max_time_off: Duration,
    rng: &mut StdRng,
) -> Result<()> {
    let time_off = time_off.unwrap_or_else(|| rand_duration(rng, min_time_off, max_time_off));

    let containers_str = containers
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    info!("🛜 Disconnecting from the network for {time_off:?}: {containers_str}");

    // Get all the subnets each container is connected to
    let containers_subnets = containers
        .iter()
        .map(|c| (*c, c.subnet_ip_map().keys().collect::<Vec<&SubnetName>>()))
        .collect::<Vec<_>>();
    let containers_subnets = containers_subnets
        .iter()
        .map(|(c, subnets)| (*c, subnets.as_slice()))
        .collect::<Vec<_>>();

    // Disconnect and reconnect the containers from all their subnets
    infra.disconnect(&containers_subnets)?;
    debug!("Waiting {time_off:?} before reconnecting...");
    tokio::time::sleep(time_off).await;
    infra.connect(&containers_subnets)?;

    info!("🛜 Reconnected after {time_off:?}: {containers_str}");
    Ok(())
}

async fn apply_kill(
    infra: &dyn InfraProvider,
    containers: &[ContainerName],
    time_off: Option<Duration>,
    min_time_off: Duration,
    max_time_off: Duration,
    rng: &mut StdRng,
) -> Result<()> {
    let time_off = time_off.unwrap_or_else(|| rand_duration(rng, min_time_off, max_time_off));

    let containers_str = containers.join(", ");
    info!("💀 Killing and waiting {time_off:?}: {containers_str}");

    infra.kill(containers)?;
    debug!("Waiting {time_off:?} before restarting...");
    tokio::time::sleep(time_off).await;
    infra.start(containers)?;

    info!("💀 Killed and restarted after {time_off:?}: {containers_str}");
    Ok(())
}

async fn apply_pause(
    infra: &dyn InfraProvider,
    containers: &[ContainerName],
    time_off: Option<Duration>,
    min_time_off: Duration,
    max_time_off: Duration,
    rng: &mut StdRng,
) -> Result<()> {
    let time_off = time_off.unwrap_or_else(|| rand_duration(rng, min_time_off, max_time_off));

    let containers_str = containers.join(", ");
    info!("⏯️  Pausing for {time_off:?}: {containers_str}");

    infra.pause(containers)?;
    debug!("Waiting {time_off:?} before unpausing...");
    tokio::time::sleep(time_off).await;
    infra.unpause(containers)?;

    info!("⏯️  Unpaused after {time_off:?}: {containers_str}");
    Ok(())
}

async fn apply_restart(infra: &dyn InfraProvider, containers: &[ContainerName]) -> Result<()> {
    let containers_str = containers.join(", ");
    info!("🔄 Restarting {containers_str}");

    infra.restart(containers)?;

    info!("🔄 Restarted {containers_str}");
    Ok(())
}

async fn apply_upgrade(
    infra: &dyn InfraProvider,
    containers: &[ContainerName],
    time_off: Option<Duration>,
    min_time_off: Duration,
    max_time_off: Duration,
    rng: &mut StdRng,
) -> Result<()> {
    let time_off = time_off.unwrap_or_else(|| rand_duration(rng, min_time_off, max_time_off));

    let containers_str = containers.join(", ");
    info!("⬆️  Upgrading (waiting {time_off:?} before restart): {containers_str}");

    infra.stop(containers)?;
    tokio::time::sleep(time_off).await;

    // Update the container names to use the '_u' suffix denoting the containers
    // using the upgraded image in the docker compose file.
    let upgraded_containers: Vec<ContainerName> = containers
        .iter()
        .map(|c| format!("{c}_{UPGRADED_SUFFIX}"))
        .collect();

    infra.start(&upgraded_containers)?;

    info!("⬆️  Upgraded after {time_off:?}: {containers_str}");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn apply_chaos(
    infra: &dyn InfraProvider,
    nodes_metadata: &NodesMetadata,
    num_nodes: usize,
    containers: &[ContainerName],
    duration: Duration,
    perturbations: &[Perturbation],
    min_time_off: Duration,
    max_time_off: Duration,
    min_wait: &Duration,
    max_wait: &Duration,
    rng: &mut StdRng,
) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < duration {
        // Pick a perturbation at random
        let perturbation = perturbations.iter().choose(&mut *rng).unwrap();

        // Pick a subset of targets at random, with size at most 1/3 of all the containers
        let num_containers = num_nodes * 2;
        let num_targets = std::cmp::min(containers.len(), num_containers / 3);
        let size = rng.gen_range(1..=num_targets);
        let targets = containers.iter().cloned().choose_multiple(&mut *rng, size);

        match *perturbation {
            Perturbation::Disconnect { .. } => {
                let containers = nodes_metadata.to_containers(&targets);
                apply_disconnect(infra, &containers, None, min_time_off, max_time_off, rng).await?
            }
            Perturbation::Kill { .. } => {
                apply_kill(infra, &targets, None, min_time_off, max_time_off, rng).await?;
            }
            Perturbation::Pause { .. } => {
                apply_pause(infra, &targets, None, min_time_off, max_time_off, rng).await?
            }
            Perturbation::Restart { .. } => {
                apply_restart(infra, &targets).await?;
            }
            Perturbation::Upgrade { .. } => {
                apply_upgrade(infra, &targets, None, min_time_off, max_time_off, rng).await?;
            }
            _ => bail!("invalid perturbation for chaos testing: {perturbation}"),
        }

        // Wait for a random duration before applying the next perturbation
        let duration = rand_duration(rng, *min_wait, *max_wait);
        info!("Waiting {duration:?} before applying the next perturbation");
        tokio::time::sleep(duration).await;
    }
    Ok(())
}

/// Load the set of upgraded containers recorded in the tracking file
/// .quake/upgraded_containers.
/// It assumes that the given file path is an existing file.
/// Returns an empty set if the file does not exist or is empty.
pub(crate) async fn load_upgraded_containers_set<P: AsRef<Path>>(
    upgraded_containers_file_path: P,
) -> Result<BTreeSet<ContainerName>> {
    let path = upgraded_containers_file_path.as_ref();

    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeSet::new());
        }
        Err(e) => {
            return Err(e).wrap_err_with(|| format!("failed to read file {}", path.display()));
        }
    };

    let set = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect::<BTreeSet<_>>();

    Ok(set)
}

/// Filter out containers that were previously upgraded.
/// Returns Ok(true) if there are containers left to process after filtering, Ok
/// (false) if all were skipped.
pub(crate) fn filter_upgraded_containers(containers: &mut Vec<ContainerName>) -> Result<bool> {
    if containers.is_empty() {
        return Ok(false);
    }

    let original_len = containers.len();
    let mut skipped: Vec<ContainerName> = Vec::new();

    // Keep only containers that do NOT have the `_u` suffix (i.e. have not been
    // upgraded yet)
    let upgraded_suffix = format!("_{}", UPGRADED_SUFFIX);
    containers.retain(|c| {
        let upgraded = c.ends_with(&upgraded_suffix);
        if upgraded {
            skipped.push(c.clone());
        }
        !upgraded
    });

    if !skipped.is_empty() {
        info!(
            "Skipping already-upgraded containers: {}",
            skipped.join(", ")
        );
    }

    if containers.is_empty() {
        info!(
            "All target containers ({} total) were already upgraded; nothing to do",
            original_len
        );
        return Ok(false);
    }

    Ok(true)
}

/// Persist the set of upgraded containers to a file, so that quake can correctly
/// reference them in subsequent runs of the same testnet.
/// It assumes that the given file path is an existing file.
pub(crate) async fn persist_upgraded_containers<P: AsRef<Path>>(
    upgraded_containers_file_path: P,
    containers: Vec<ContainerName>,
) -> Result<()> {
    if containers.is_empty() {
        return Ok(());
    }

    let mut upgraded_containers = load_upgraded_containers_set(&upgraded_containers_file_path)
        .await
        .wrap_err("failed to load the upgraded containers list")?;

    for c in containers.into_iter() {
        upgraded_containers.insert(c);
    }

    save_upgraded_containers_set(upgraded_containers_file_path, upgraded_containers)
        .await
        .wrap_err("failed to save the upgraded containers list")?;

    Ok(())
}

/// Updates the .upgraded_containers file with the current set of upgraded
/// containers.
/// It assumes that the given file path is an existing file.
pub(crate) async fn save_upgraded_containers_set<P: AsRef<Path>>(
    upgraded_containers_file_path: P,
    containers_set: BTreeSet<ContainerName>,
) -> Result<()> {
    if containers_set.is_empty() {
        return Ok(());
    }

    let path = upgraded_containers_file_path.as_ref().to_path_buf();

    let write_to_file = tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .wrap_err_with(|| {
                format!(
                    "failed to open file to write upgraded containers list: {}",
                    path.display()
                )
            })?;

        let mut w = std::io::BufWriter::new(file);

        for container in containers_set {
            writeln!(w, "{container}").wrap_err_with(|| {
                format!(
                    "failed to write to file the upgraded containers list: {}",
                    path.display()
                )
            })?;
        }
        w.flush()?;

        Ok(())
    });

    write_to_file.await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    #[test]
    fn filter_upgraded_containers_all_upgraded() {
        let upgraded_suffix = format!("_{}", UPGRADED_SUFFIX);
        let mut containers = vec![
            format!("validator1_cl{}", upgraded_suffix),
            format!("validator2_el{}", upgraded_suffix),
        ];
        let kept = filter_upgraded_containers(&mut containers).unwrap();
        assert!(!kept);
        assert!(containers.is_empty());
    }

    #[test]
    fn filter_upgraded_containers_none_upgraded() {
        let mut containers = vec!["validator1_cl".to_string(), "validator2_el".to_string()];
        let kept = filter_upgraded_containers(&mut containers).unwrap();
        assert!(kept);
        assert_eq!(
            containers,
            vec!["validator1_cl".to_string(), "validator2_el".to_string()]
        );
    }

    #[test]
    fn filter_upgraded_containers_empty_vec() {
        let mut containers: Vec<ContainerName> = vec![];
        let kept = filter_upgraded_containers(&mut containers).unwrap();
        assert!(!kept);
        assert!(containers.is_empty());
    }

    #[tokio::test]
    async fn load_upgraded_containers_set_returns_empty_if_missing() {
        let test_dir = env::temp_dir().join("upgraded_empty_set");

        fs::create_dir_all(&test_dir).unwrap();

        let file_path = test_dir.join(".upgraded_containers");
        let set = load_upgraded_containers_set(file_path).await.unwrap();

        fs::remove_dir_all(&test_dir).unwrap();

        assert_eq!(set.len(), 0);
    }

    #[tokio::test]
    async fn load_upgraded_containers_set_parses_lines() {
        let test_dir = env::temp_dir().join("upgraded_set");
        fs::create_dir_all(&test_dir).unwrap();

        let file_path = test_dir.join(".upgraded_containers");
        fs::write(&file_path, "validator1_cl\nvalidator2_el\nvalidator3_cl\n").unwrap();

        let set = load_upgraded_containers_set(file_path).await.unwrap();

        fs::remove_dir_all(&test_dir).unwrap();

        assert_eq!(set.len(), 3);
        assert!(set.contains("validator1_cl"));
        assert!(set.contains("validator2_el"));
        assert!(set.contains("validator3_cl"));
    }

    #[tokio::test]
    async fn persist_upgraded_containers_writes_expected_lines() {
        let test_dir = env::temp_dir().join("persist_upgraded_write");
        fs::create_dir_all(&test_dir).unwrap();

        let file_path = test_dir.join(".upgraded_containers");
        let containers = vec!["val1_cl".to_string(), "val2_el".to_string()];
        persist_upgraded_containers(&file_path, containers)
            .await
            .unwrap();

        let contents = fs::read_to_string(file_path).unwrap();

        fs::remove_dir_all(&test_dir).unwrap();

        assert_eq!(contents, "val1_cl\nval2_el\n");
    }

    #[tokio::test]
    async fn persist_upgraded_containers_overwrites_existing_file() {
        let test_dir = env::temp_dir().join("persist_upgraded_overwrite");
        fs::create_dir_all(&test_dir).unwrap();

        let file_path = test_dir.join(".upgraded_containers");
        fs::write(&file_path, "val1_cl\n").unwrap();

        let containers = vec!["val1_cl".to_string(), "val2_cl".to_string()];
        persist_upgraded_containers(&file_path, containers)
            .await
            .unwrap();

        let contents = fs::read_to_string(file_path).unwrap();

        fs::remove_dir_all(&test_dir).unwrap();

        assert_eq!(contents, "val1_cl\nval2_cl\n");
    }

    #[test]
    fn from_str_upgrade_has_no_time_off() {
        let p = Perturbation::from_str("upgrade").unwrap();
        match p {
            Perturbation::Upgrade { targets, time_off } => {
                assert!(targets.is_empty());
                assert!(time_off.is_none());
            }
            _ => panic!("expected Perturbation::Upgrade"),
        }
    }

    #[test]
    fn rand_duration_returns_value_within_range() {
        let mut rng = StdRng::seed_from_u64(42);
        let min = Duration::from_secs(10);
        let max = Duration::from_secs(20);
        let result = rand_duration(&mut rng, min, max);
        assert!(result >= min && result <= max);
    }

    #[test]
    fn rand_duration_with_equal_min_max() {
        let mut rng = StdRng::seed_from_u64(42);
        let d = Duration::from_secs(5);
        let result = rand_duration(&mut rng, d, d);
        assert_eq!(result, d);
    }

    #[test]
    fn rand_duration_with_min_greater_than_max_returns_min() {
        let mut rng = StdRng::seed_from_u64(42);
        let min = Duration::from_secs(30);
        let max = Duration::from_secs(10);
        let result = rand_duration(&mut rng, min, max);
        assert_eq!(result, min);
    }

    #[test]
    fn rand_duration_is_deterministic_for_same_seed() {
        let min = Duration::from_secs(1);
        let max = Duration::from_secs(100);
        let mut rng_a = StdRng::seed_from_u64(99);
        let mut rng_b = StdRng::seed_from_u64(99);
        let a = rand_duration(&mut rng_a, min, max);
        let b = rand_duration(&mut rng_b, min, max);
        assert_eq!(a, b);
    }

    #[test]
    fn rand_duration_varies_with_different_seeds() {
        let min = Duration::from_secs(1);
        let max = Duration::from_secs(1000);
        let mut rng_a = StdRng::seed_from_u64(1);
        let mut rng_b = StdRng::seed_from_u64(2);
        let a = rand_duration(&mut rng_a, min, max);
        let b = rand_duration(&mut rng_b, min, max);
        assert_ne!(a, b);
    }

    #[test]
    fn rand_duration_advances_rng_state() {
        let mut rng = StdRng::seed_from_u64(42);
        let min = Duration::from_secs(1);
        let max = Duration::from_secs(1000);
        let a = rand_duration(&mut rng, min, max);
        let b = rand_duration(&mut rng, min, max);
        assert_ne!(a, b);
    }
}
