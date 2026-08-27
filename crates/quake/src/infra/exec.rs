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

//! Cheaply-cloneable handle for executing a command inside a node's CL or EL
//! container. Carries only what `docker exec` actually needs (a directory plus
//! shared node metadata for local, an `Arc<RemoteInfra>` for remote), so each
//! clone can be moved into a `tokio::task::spawn_blocking` worker for concurrent
//! fan-out.

use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::{bail, Result};

use crate::infra::remote::{RemoteInfra, CONTAINER_NAME_CONSENSUS, CONTAINER_NAME_EXECUTION};
use crate::infra::InfraType;
use crate::node::{NodeName, CONSENSUS_SUFFIX, EXECUTION_SUFFIX};
use crate::nodes::NodesMetadata;
use crate::shell;
use crate::testnet::Testnet;

#[derive(Clone)]
pub(crate) enum ExecBackend {
    Local {
        dir: PathBuf,
        nodes: Arc<NodesMetadata>,
    },
    Remote(Arc<RemoteInfra>),
}

impl From<&Testnet> for ExecBackend {
    fn from(testnet: &Testnet) -> Self {
        match testnet.infra_data.infra_type {
            InfraType::Local => Self::Local {
                dir: testnet.dir.clone(),
                nodes: Arc::new(testnet.nodes_metadata.clone()),
            },
            InfraType::Remote => Self::Remote(
                testnet
                    .remote_infra()
                    .expect("infra_type Remote implies RemoteInfra"),
            ),
        }
    }
}

impl ExecBackend {
    /// Execute a command inside a node's CL or EL container and return its stdout.
    ///
    /// Local: `docker exec <container> <argv…>`.
    /// Remote: SSH to the node's EC2 host (routed through CC), then run
    /// `docker exec <cl|el> <argv…>` because each remote host runs only one node.
    pub fn exec_in_container(
        &self,
        node: &NodeName,
        container: &str,
        argv: &[&str],
    ) -> Result<String> {
        match self {
            Self::Local { dir, nodes } => {
                let container = nodes.running_container_name(node, container)?;
                let mut args = vec!["exec", container.as_str()];
                args.extend_from_slice(argv);
                shell::exec_with_output("docker", args, dir)
            }
            Self::Remote(remote) => {
                let container = match container {
                    CONSENSUS_SUFFIX => CONTAINER_NAME_CONSENSUS,
                    EXECUTION_SUFFIX => CONTAINER_NAME_EXECUTION,
                    _ => bail!("unsupported container suffix '{container}'"),
                };
                let remote_cmd = format!("docker exec {container} {}", argv.join(" "));
                remote.ssh_node_with_output(node, &remote_cmd)
            }
        }
    }
}
