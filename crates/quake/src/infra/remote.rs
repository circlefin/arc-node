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

use color_eyre::eyre::{eyre, Context, Result};
use itertools::Itertools;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::infra::export::SSH_KEY_FILENAME;
use crate::infra::terraform::Terraform;
use crate::infra::{ssm, BuildProfile, InfraData, InfraProvider};
use crate::node::{Container, ContainerName, IpAddress, NodeName, SubnetName};
use crate::node::{CONSENSUS_SUFFIX, EXECUTION_SUFFIX, UPGRADED_SUFFIX};
use crate::nodes::NodeOrContainerName;
use crate::shell;

pub(crate) const DEFAULT_IMAGE_CL: &str = "${IMAGE_REGISTRY_URL}/arc-consensus:latest";
pub(crate) const DEFAULT_IMAGE_EL: &str = "${IMAGE_REGISTRY_URL}/arc-execution:latest";
pub(crate) const USER_NAME: &str = "ssm-user";
pub(crate) const CC_INSTANCE: &str = "cc";

pub(crate) const CONTAINER_NAME_CONSENSUS: &str = "cl";
pub(crate) const CONTAINER_NAME_EXECUTION: &str = "el";

/// SSH options for CC to nodes.
pub(crate) const CC_SSH_OPTS: &str =
    "-o StrictHostKeyChecking=accept-new -o LogLevel=ERROR -i /home/ssm-user/.ssh/id_rsa";

/// Remote infrastructure provider
pub(crate) struct RemoteInfra {
    root_dir: PathBuf,
    testnet_dir: PathBuf,
    infra_data: InfraData,
    pub terraform: Terraform,
    pub ssm_tunnels: ssm::Ssm,
}

impl RemoteInfra {
    pub fn new(
        root_dir: &Path,
        testnet_dir: &Path,
        infra_data: InfraData,
        terraform: Terraform,
        ssm_tunnels: ssm::Ssm,
    ) -> Result<Self> {
        Ok(Self {
            root_dir: root_dir.to_path_buf(),
            testnet_dir: testnet_dir.to_path_buf(),
            infra_data,
            terraform,
            ssm_tunnels,
        })
    }

    fn private_key_path(&self) -> String {
        self.testnet_dir
            .join(SSH_KEY_FILENAME)
            .to_string_lossy()
            .to_string()
    }

    /// Get the instance ID of the given node or CC server.
    pub fn instance_id(&self, node_or_cc: &str) -> Result<String> {
        self.infra_data.get_data(node_or_cc)?.instance_id()
    }

    /// Get the (primary) private IP of a node, i.e. the first subnet IP.
    fn node_private_ip(&self, node: &NodeName) -> Result<&IpAddress> {
        let data = self.infra_data.get_data(node)?;
        Ok(data.first_private_ip())
    }

    /// SSH a command to the CC server. If no command is provided, an interactive shell will be opened.
    ///
    /// If `force_tty` is true, a PTY is allocated even when a command is provided.
    pub fn ssh_cc(&self, cmd: &str, force_tty: bool) -> Result<()> {
        shell::ssh(
            &self.instance_id(CC_INSTANCE)?,
            USER_NAME,
            &self.private_key_path(),
            &self.root_dir,
            cmd,
            force_tty,
        )
    }

    /// Run a command on the CC server via SSH and return its stdout.
    pub fn ssh_cc_with_output(&self, cmd: &str) -> Result<String> {
        shell::ssh_with_output(
            &self.instance_id(CC_INSTANCE)?,
            USER_NAME,
            &self.private_key_path(),
            &self.root_dir,
            cmd,
        )
        .wrap_err_with(|| format!("SSH to CC failed for command: {cmd}"))
    }

    /// SSH a command to a node. If no command is provided, an interactive shell will be opened.
    ///
    /// Routes through CC using the node's private IP to avoid creating additional SSM sessions.
    pub fn ssh_node(&self, node: &NodeName, cmd: &str) -> Result<()> {
        let node_ip = self.node_private_ip(node)?;
        let is_interactive = cmd.is_empty();
        let nested_cmd = if is_interactive {
            // Interactive session: force PTY allocation with -t
            format!("ssh -tt {CC_SSH_OPTS} {USER_NAME}@{node_ip}")
        } else {
            format!("ssh {CC_SSH_OPTS} {USER_NAME}@{node_ip} \"{cmd}\"")
        };
        // For nested interactive SSH, the outer SSH also needs -t
        self.ssh_cc(&nested_cmd, is_interactive)
    }

    /// Run a command on a node via SSH (routed through CC) and return its stdout.
    pub fn ssh_node_with_output(&self, node: &NodeName, cmd: &str) -> Result<String> {
        let node_ip = self.node_private_ip(node)?;
        let nested_cmd = format!("ssh {CC_SSH_OPTS} {USER_NAME}@{node_ip} \"{cmd}\"");
        self.ssh_cc_with_output(&nested_cmd)
            .wrap_err_with(|| format!("SSH to node {node} failed for command: {cmd}"))
    }

    /// Run `cmd` on CC itself and on every node IP in parallel, returning the
    /// combined output with `HOST:<ip>` delimiters.
    ///
    /// CC collects its own output first, then fans out to all `node_ips` via
    /// backgrounded SSH subshells. Uses single quotes around the inner command
    /// so it survives two layers of shell interpretation.
    pub fn ssh_fanout_with_output(
        &self,
        cc_ip: &str,
        node_ips: &[&str],
        cmd: &str,
    ) -> Result<String> {
        let mut script = String::from("rm -f /tmp/qr_$$_*\n");
        script.push_str(&format!("echo HOST:{cc_ip}\n{cmd}"));

        for (i, ip) in node_ips.iter().enumerate() {
            script.push_str(&format!(
                "\n(echo HOST:{ip}; ssh {CC_SSH_OPTS} {USER_NAME}@{ip} '{cmd}') > /tmp/qr_$$_{i} 2>&1 &"
            ));
        }

        if !node_ips.is_empty() {
            script.push_str("\nwait\ncat /tmp/qr_$$_*\nrm -f /tmp/qr_$$_*");
        }

        self.ssh_cc_with_output(&script)
    }

    /// Copy files to CC.
    fn scp_to_cc(&self, sources: &[&str], dest: &str, recursive: bool) -> Result<()> {
        shell::scp(
            &self.instance_id(CC_INSTANCE)?,
            USER_NAME,
            &self.private_key_path(),
            &self.root_dir,
            sources,
            dest,
            recursive,
        )
    }

    /// For each (node, command) pair, execute in parallel each command in the corresponding node.
    ///
    /// Routes all SSH connections through CC using nodes' private IPs to avoid
    /// creating multiple SSM sessions. Only one SSM session (to CC) is used.
    fn pssh_multi_cmd(&self, node_commands: &[(NodeName, String)]) -> Result<()> {
        if node_commands.is_empty() {
            return Ok(());
        }

        // Build commands that CC will execute: SSH to each node using the key in CC.
        // Each job is backgrounded and its PID collected, then we wait on each PID
        // individually so that any failure is propagated.
        let parallel_cmd = node_commands
            .iter()
            .map(|(node, cmd)| {
                let node_ip = self.node_private_ip(node)?;
                Ok(format!(
                    "(ssh {CC_SSH_OPTS} {USER_NAME}@{node_ip} '{cmd}') & pids=\"$pids $!\""
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join("; ");
        let parallel_cmd = format!(
            "pids=\"\"; {parallel_cmd}; fail=0; for pid in $pids; do wait $pid || fail=1; done; exit $fail"
        );

        self.ssh_cc(&parallel_cmd, false)
    }

    /// Run the same command on the given nodes in parallel by calling pssh.sh in CC.
    fn pssh_single_cmd(&self, nodes: &[&NodeName], cmd: &str) -> Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }

        let node_ips = nodes
            .iter()
            .map(|node| self.node_private_ip(node).map(String::as_str))
            .collect::<Result<Vec<_>>>()?;

        let pssh_cmd = format!("./pssh.sh \"{cmd}\" {}", node_ips.join(" "));
        self.ssh_cc(&pssh_cmd, false)
            .wrap_err_with(|| format!("Failed to run '{cmd}' on {nodes:?}"))
    }

    /// Given a list of container names, return a list of pairs (node name,
    /// list of remote container names). A remote container name is either
    /// `cl` or `el`.
    ///
    /// Handles both normal (`<node>_<cl|el>`) and upgraded (`<node>_<cl|el>_u`)
    /// container name formats.
    fn map_containers_to_nodes(
        &self,
        containers: &[NodeOrContainerName],
    ) -> Vec<(NodeName, Vec<ContainerName>)> {
        let upgraded_suffix = format!("_{UPGRADED_SUFFIX}");
        let mut node_containers: Vec<(NodeName, ContainerName)> = containers
            .iter()
            .map(|container| {
                let base = container
                    .strip_suffix(&upgraded_suffix)
                    .unwrap_or(container);
                let (node_name, suffix) = base.split_once("_").unwrap_or((base, ""));
                let suffix = if suffix.is_empty() {
                    None
                } else {
                    Some(suffix)
                };
                let container = match suffix {
                    Some(CONSENSUS_SUFFIX) => CONTAINER_NAME_CONSENSUS.to_string(),
                    Some(EXECUTION_SUFFIX) => CONTAINER_NAME_EXECUTION.to_string(),
                    _ => {
                        warn!("Invalid container suffix {suffix:?} for {container}");
                        "".to_string()
                    }
                };
                (node_name.to_string(), container)
            })
            .collect::<Vec<_>>();

        // Sort by node name
        node_containers.sort_by_key(|(node, _)| node.clone());

        // Group containers by node name
        let grouped_node_containers = node_containers
            .into_iter()
            .chunk_by(|(node, _)| node.clone());

        // Return list of (node_name, containers)
        grouped_node_containers
            .into_iter()
            .map(|(node, containers)| (node, containers.map(|(_, c)| c).collect()))
            .collect()
    }

    /// Execute a command on the given containers.
    ///
    /// All SSH connections are routed through CC using nodes' private IPs.
    fn exec_on_containers(&self, cmd: &str, containers: &[NodeOrContainerName]) -> Result<()> {
        debug!(%cmd, targets=%containers.join(","), "Applying command");
        if containers.is_empty() {
            // Run command on all nodes
            self.pssh_single_cmd(&self.infra_data.node_names(), cmd)
        } else {
            // Run command on the given containers
            let node_commands: Vec<(String, String)> = self
                .map_containers_to_nodes(containers)
                .into_iter()
                .flat_map(|(node, containers)| {
                    vec![(node, format!("{cmd} {}", containers.join(" ")))]
                })
                .collect();

            self.pssh_multi_cmd(&node_commands)
                .wrap_err_with(|| format!("Failed to run '{cmd}' on containers {containers:?}"))
        }
    }

    /// Collect the peer IPs to block on each target host (unidirectional).
    ///
    /// Returns a map: target_node_name → set of peer IPs to block on that host.
    ///
    /// Rules are only installed on the target's host, blocking all peer VPC IPs
    /// on shared subnets. This is sufficient because the target drops both
    /// inbound and outbound packets via INPUT/OUTPUT/FORWARD chains. Peers will
    /// see TCP timeouts, which is realistic for network partition testing.
    ///
    /// Nodes with no peers on shared subnets are intentionally omitted from the
    /// result — there is nothing to block.
    fn collect_iptables_block_ips(
        &self,
        containers_subnets: &[(&Container, &[&SubnetName])],
    ) -> HashMap<NodeName, HashSet<IpAddress>> {
        let mut node_block_ips: HashMap<NodeName, HashSet<IpAddress>> = HashMap::new();

        for (container, subnets) in containers_subnets {
            let target_node = &container.node_name;

            for (peer_name, peer_data) in &self.infra_data.nodes {
                if peer_name == target_node {
                    continue;
                }
                for subnet in *subnets {
                    if let Some(peer_ip) = peer_data.subnet_ips().get(*subnet) {
                        node_block_ips
                            .entry(target_node.clone())
                            .or_default()
                            .insert(peer_ip.clone());
                    }
                }
            }
        }
        node_block_ips
    }

    /// Build per-host iptables commands by applying `per_ip_cmd` to each IP
    /// and joining with `; ` so that each IP is handled independently.
    fn build_iptables_cmds(
        &self,
        node_block_ips: &HashMap<NodeName, HashSet<IpAddress>>,
        per_ip_cmd: impl Fn(&str) -> String,
    ) -> Vec<(NodeName, String)> {
        node_block_ips
            .iter()
            .map(|(node, ips)| {
                let cmd = ips
                    .iter()
                    .map(|ip| per_ip_cmd(ip))
                    .collect::<Vec<_>>()
                    .join("; ");
                (node.clone(), cmd)
            })
            .collect()
    }

    /// Idempotently remove all existing DROP rules for the given IP.
    ///
    /// Uses `2>/dev/null` to suppress the expected "rule not found" exit code
    /// from `iptables -D` when no matching rule exists. Note: this also hides
    /// unexpected errors (e.g. missing iptables binary); if debugging iptables
    /// issues, check the host directly.
    fn iptables_cleanup_for_ip(ip: &str) -> String {
        format!(
            "while sudo iptables -D INPUT -s {ip} -j DROP 2>/dev/null; do :; done; \
             while sudo iptables -D OUTPUT -d {ip} -j DROP 2>/dev/null; do :; done; \
             while sudo iptables -D FORWARD -s {ip} -j DROP 2>/dev/null; do :; done; \
             while sudo iptables -D FORWARD -d {ip} -j DROP 2>/dev/null; do :; done"
        )
    }

    /// Build iptables commands to block traffic on target nodes only.
    ///
    /// For each target, blocks all peer IPs on shared subnets by installing
    /// DROP rules in INPUT, OUTPUT, and FORWARD chains. First removes any
    /// existing matching DROP rules (idempotent), then inserts at the top of
    /// each chain (`-I ... 1`) so our rules take precedence over Docker's own
    /// ACCEPT rules in the FORWARD chain.
    fn build_iptables_block_cmds(
        &self,
        containers_subnets: &[(&Container, &[&SubnetName])],
    ) -> Vec<(NodeName, String)> {
        let block_ips = self.collect_iptables_block_ips(containers_subnets);
        self.build_iptables_cmds(&block_ips, |ip| {
            format!(
                "{}; \
                 sudo iptables -I INPUT 1 -s {ip} -j DROP && \
                 sudo iptables -I OUTPUT 1 -d {ip} -j DROP && \
                 sudo iptables -I FORWARD 1 -s {ip} -j DROP && \
                 sudo iptables -I FORWARD 1 -d {ip} -j DROP",
                Self::iptables_cleanup_for_ip(ip)
            )
        })
    }

    /// Build iptables commands to unblock traffic on target nodes only.
    ///
    /// Removes ALL matching DROP rules (not just one) using a loop, so that
    /// duplicate rules from previous runs or crashes are cleaned up.
    fn build_iptables_unblock_cmds(
        &self,
        containers_subnets: &[(&Container, &[&SubnetName])],
    ) -> Vec<(NodeName, String)> {
        let block_ips = self.collect_iptables_block_ips(containers_subnets);
        self.build_iptables_cmds(&block_ips, |ip| {
            format!("{}; true", Self::iptables_cleanup_for_ip(ip))
        })
    }

    /// Provision the remote nodes and the Control Center server.
    pub fn provision(&self) -> Result<()> {
        info!("🌈 Provisioning testnet files to Control Center server...");

        // We copy the following files to CC. Each node will access them via NFS.
        // - config files for each node
        // - assets directory contains the genesis file and other assets
        // - compose.yaml contains CL and EL services and is shared by all nodes
        let mut sources = vec!["assets"];
        sources.extend(self.infra_data.node_names().iter().map(|s| s.as_str()));

        // 1. Create a compressed archive of all files
        let archive_name = "testnet-files.tar.gz";
        let archive_path = self.testnet_dir.join(archive_name);
        debug!("📦 Compressing testnet files to {}", archive_path.display());

        // Suppress macOS metadata in the archive:
        // - --no-mac-metadata: prevents ACLs and Mac-specific metadata
        // - --no-xattrs: prevents extended attributes from being stored in PAX headers
        // - COPYFILE_DISABLE=1: prevents AppleDouble (._*) resource fork files
        let tar_args: Vec<&str> = ["--no-mac-metadata", "--no-xattrs", "-czf", archive_name]
            .into_iter()
            .chain(sources)
            .collect();
        let env = vec![("COPYFILE_DISABLE".to_string(), "1".to_string())];

        shell::exec("tar", tar_args, &self.testnet_dir, Some(env), false)
            .wrap_err("Failed to compress testnet files")?;

        // 2. Upload the single compressed archive to CC
        debug!("📤 Uploading compressed archive to Control Center");
        let archive_path_str = archive_path.to_string_lossy().to_string();
        self.scp_to_cc(&[archive_path_str.as_str()], "shared/", false)
            .wrap_err("Failed to copy compressed archive to Control Center")?;

        // 3. Extract the archive on CC and clean up
        debug!("📂 Extracting archive on Control Center");
        let extract_cmd =
            format!("cd /home/{USER_NAME}/shared && tar -xzf {archive_name} && rm {archive_name}");
        self.ssh_cc(&extract_cmd, false)
            .wrap_err("Failed to extract archive on Control Center")?;

        // 4. Copy nodes.json to CC's home directory, needed by the spammer tool
        debug!("📂 Copying nodes.json to Control Center");
        let nodes_json_path = self.testnet_dir.join("nodes.json");
        let nodes_json_path_str = nodes_json_path.to_string_lossy().to_string();
        self.scp_to_cc(&[nodes_json_path_str.as_str()], "", false)
            .wrap_err("Failed to copy nodes.json to Control Center")?;

        info!("✅ Provisioning for remote infrastructure completed");
        Ok(())
    }
}

impl InfraProvider for RemoteInfra {
    fn build(&self, _profile: BuildProfile) -> Result<()> {
        info!("Nothing to build; images pulled from GitHub repo directly to remote nodes");
        Ok(())
    }

    /// A remote testnet is considered set up if
    /// 1) files were generated locally, such as the genesis, compose.yaml, node config files, etc.,
    /// 2) files were provisioned to CC, and
    /// 3) all nodes have NFS mounted correctly (so symlinks can be resolved).
    fn is_setup(&self, nodes: &[NodeName]) -> Result<()> {
        // Check if the genesis file exists locally
        let genesis_file_path = self.testnet_dir.join("assets").join("genesis.json");
        if !genesis_file_path.exists() {
            return Err(eyre!("genesis file not found"));
        }

        // Check if nodes.json exists in CC
        let cmd = format!("[ -f /home/{USER_NAME}/nodes.json ]");
        self.ssh_cc(&cmd, false)
            .wrap_err("nodes.json missing in CC")?;

        // Get a node (the first one in the list) to check if NFS is mounted correctly
        let nodes = if nodes.is_empty() {
            self.infra_data.node_names()
        } else {
            nodes.iter().collect::<Vec<_>>()
        };
        let first_node = nodes.first().ok_or_else(|| eyre!("no nodes found"))?;

        // Check if compose.yaml exists in the first node (via NFS from CC)
        let cmd = format!("[ -f /home/{USER_NAME}/compose.yaml ]");
        self.ssh_node(first_node, &cmd)
            .wrap_err_with(|| format!("NFS mount check failed on node {first_node}"))
    }

    fn start(&self, containers: &[NodeOrContainerName]) -> Result<()> {
        self.exec_on_containers("docker compose up -d", containers)
    }

    fn stop(&self, containers: &[NodeOrContainerName]) -> Result<()> {
        self.exec_on_containers("docker compose stop", containers)
    }

    fn down(&self, containers: &[NodeOrContainerName]) -> Result<()> {
        self.exec_on_containers("docker compose down", containers)
    }

    fn logs(&self, containers: &[NodeOrContainerName], follow: bool) -> Result<()> {
        let cmd = format!("docker compose logs {}", if follow { "-f" } else { "" });
        self.exec_on_containers(cmd.as_str(), containers)
    }

    fn connect(&self, containers_subnets: &[(&Container, &[&SubnetName])]) -> Result<()> {
        // Remove iptables DROP rules on target hosts. Malachite's persistent
        // peer reconnection (1s periodic timer with unlimited retries) handles
        // re-establishing connections on both sides once the path is open.
        let node_commands = self.build_iptables_unblock_cmds(containers_subnets);
        self.pssh_multi_cmd(&node_commands)
            .wrap_err("Failed to remove iptables DROP rules for reconnect")
    }

    fn disconnect(&self, containers_subnets: &[(&Container, &[&SubnetName])]) -> Result<()> {
        let node_commands = self.build_iptables_block_cmds(containers_subnets);
        self.pssh_multi_cmd(&node_commands)
            .wrap_err("Failed to add iptables DROP rules for disconnect")
    }

    fn kill(&self, containers: &[ContainerName]) -> Result<()> {
        self.exec_on_containers("docker compose kill", containers)
    }

    fn pause(&self, containers: &[ContainerName]) -> Result<()> {
        self.exec_on_containers("docker compose pause", containers)
    }

    fn unpause(&self, containers: &[ContainerName]) -> Result<()> {
        self.exec_on_containers("docker compose unpause", containers)
    }

    fn restart(&self, containers: &[ContainerName]) -> Result<()> {
        self.exec_on_containers("docker compose restart", containers)
    }
}
