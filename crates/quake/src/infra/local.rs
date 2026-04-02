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

use color_eyre::eyre::{eyre, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::infra::{docker, BuildProfile, InfraProvider, COMPOSE_PROJECT_NAME};
use crate::node::{Container, ContainerName, NodeName, SubnetName};
use crate::nodes::NodeOrContainerName;

pub(crate) const COMPOSE_FILENAME: &str = "compose.yaml";
pub(crate) const COMPOSE_BUILD_FILENAME: &str = "arc_builders.yaml";
pub(crate) const DEFAULT_IMAGE_CL: &str = "arc_consensus:latest";
pub(crate) const DEFAULT_IMAGE_EL: &str = "arc_execution:latest";

trait DockerArg<'a> {
    fn add_to(self, args: &mut Vec<&'a str>);
}

impl<'a> DockerArg<'a> for &'a str {
    fn add_to(self, args: &mut Vec<&'a str>) {
        args.push(self);
    }
}

impl<'a> DockerArg<'a> for &'a [String] {
    fn add_to(self, args: &mut Vec<&'a str>) {
        args.extend(self.iter().map(|s| s.as_str()));
    }
}

impl<'a> DockerArg<'a> for &'a String {
    fn add_to(self, args: &mut Vec<&'a str>) {
        args.push(self);
    }
}

impl<'a> DockerArg<'a> for &'a Vec<String> {
    fn add_to(self, args: &mut Vec<&'a str>) {
        args.extend(self.iter().map(|s| s.as_str()));
    }
}

/// Build a vector of arguments for a `docker` command.
macro_rules! args {
    ($($arg:expr),+ $(,)?) => {
        {
            let mut args = vec![];
            $(
                DockerArg::add_to($arg, &mut args);
            )+
            args
        }
    };
}

/// Local infrastructure provider, with nodes and other services deployed locally as Docker containers.
pub(crate) struct LocalInfra {
    root_dir: PathBuf,
    compose_path: PathBuf,
    compose_build_path: PathBuf,
    pub monitoring: MonitoringManager,
}

impl LocalInfra {
    pub fn new(root_dir: &Path, testnet_dir: &Path, monitoring: MonitoringManager) -> Result<Self> {
        let compose_path = testnet_dir.join(COMPOSE_FILENAME);
        let compose_build_path = testnet_dir.join(COMPOSE_BUILD_FILENAME);
        Ok(Self {
            root_dir: root_dir.to_path_buf(),
            compose_path,
            compose_build_path,
            monitoring,
        })
    }

    fn docker_exec(&self, args: Vec<&str>) -> Result<()> {
        docker::exec(&self.root_dir, args)
    }

    fn docker_compose_exec(&self, compose_path: &Path, args: Vec<&str>) -> Result<()> {
        docker::compose_exec(&self.root_dir, compose_path, args)
    }
}

impl InfraProvider for LocalInfra {
    fn build(&self, profile: BuildProfile) -> Result<()> {
        let profile_arg = format!("BUILD_PROFILE={profile}");
        let mut args = args!("build", "--build-arg", &profile_arg);

        if let Ok(idempotent_build) = dotenvy::var("ARC_IDEMPOTENT_BUILD") {
            args.extend([
                "--build-arg",
                format!("ARC_IDEMPOTENT_BUILD={idempotent_build}").leak(),
            ]);
        }

        // Export version info
        args.extend([
            "--build-arg",
            format!("GIT_COMMIT_HASH={}", arc_version::GIT_COMMIT_HASH).leak(),
        ]);
        args.extend([
            "--build-arg",
            format!("GIT_VERSION={}", arc_version::GIT_VERSION).leak(),
        ]);

        self.docker_compose_exec(&self.compose_build_path, args)
    }

    fn is_setup(&self, _nodes: &[NodeName]) -> Result<()> {
        docker::compose_file_exists(&self.compose_path)
    }

    fn start(&self, names: &[NodeOrContainerName]) -> Result<()> {
        self.docker_compose_exec(&self.compose_path, args!("up", "-d", names))
    }

    fn stop(&self, names: &[NodeOrContainerName]) -> Result<()> {
        self.docker_compose_exec(&self.compose_path, args!("stop", names))
    }

    fn down(&self, names: &[NodeOrContainerName]) -> Result<()> {
        self.docker_compose_exec(
            &self.compose_path,
            args!(
                "down",
                "--remove-orphans",
                "--volumes",
                "--timeout",
                "5",
                names
            ),
        )
    }

    fn logs(&self, names: &[NodeOrContainerName], follow: bool) -> Result<()> {
        if follow {
            self.docker_compose_exec(&self.compose_path, args!("logs", "-f", names))
        } else {
            self.docker_compose_exec(&self.compose_path, args!("logs", names))
        }
    }

    fn disconnect(&self, containers_subnets: &[(&Container, &[&SubnetName])]) -> Result<()> {
        for (container, subnets) in containers_subnets.iter() {
            for subnet in subnets.iter() {
                let network = format!("{COMPOSE_PROJECT_NAME}_{subnet}");
                self.docker_exec(args!("network", "disconnect", &network, container.name()))?;
            }
        }
        Ok(())
    }

    fn connect(&self, containers_subnets: &[(&Container, &[&SubnetName])]) -> Result<()> {
        for (container, subnets) in containers_subnets.iter() {
            for subnet in subnets.iter() {
                let container_name = container.name();
                let ip = container
                    .private_ip_address_for(subnet)
                    .ok_or_else(|| eyre!("Failed to get private IP address for container {container_name} on subnet {subnet}"))?;
                let network = format!("{COMPOSE_PROJECT_NAME}_{subnet}");
                self.docker_exec(args!(
                    "network",
                    "connect",
                    "--ip",
                    &ip,
                    &network,
                    container_name
                ))?;
            }
        }
        Ok(())
    }

    fn kill(&self, containers: &[ContainerName]) -> Result<()> {
        self.docker_exec(args!("kill", containers))
    }

    fn pause(&self, containers: &[ContainerName]) -> Result<()> {
        self.docker_exec(args!("pause", containers))
    }

    fn unpause(&self, containers: &[ContainerName]) -> Result<()> {
        self.docker_exec(args!("unpause", containers))
    }

    fn restart(&self, containers: &[ContainerName]) -> Result<()> {
        self.docker_exec(args!("restart", containers))
    }
}

#[derive(Clone)]
pub(crate) struct MonitoringManager {
    root_dir: PathBuf,
    pub dir: PathBuf,
    pub compose_path: PathBuf,
}

impl MonitoringManager {
    pub(crate) fn new(root_dir: &Path, quake_dir: &Path) -> Result<Self> {
        let monitoring_dir = quake_dir.join("monitoring");
        Ok(Self {
            root_dir: root_dir.to_path_buf(),
            dir: monitoring_dir.clone(),
            compose_path: monitoring_dir.join(COMPOSE_FILENAME),
        })
    }

    // Create monitoring directory
    pub fn setup(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        Ok(())
    }

    // Remove monitoring directory
    pub fn clean(&self) -> Result<()> {
        if self.dir.exists() {
            fs::remove_dir_all(&self.dir)?;
        }
        Ok(())
    }

    /// Start monitoring services (Prometheus and Grafana).
    pub fn start(&self) -> Result<()> {
        docker::compose_exec(&self.root_dir, &self.compose_path, args!("up", "-d"))
    }

    /// Stop monitoring services.
    pub fn stop(&self) -> Result<()> {
        docker::compose_exec(
            &self.root_dir,
            &self.compose_path,
            args!("down", "--remove-orphans", "--volumes", "--timeout", "5"),
        )
    }
}
