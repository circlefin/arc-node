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

use color_eyre::eyre::{bail, Result};
use std::path::Path;
use std::{fs, sync::Arc};
use tracing::{debug, info, warn};

use crate::infra::InfraProvider;
use crate::infra::{local::LocalInfra, remote::RemoteInfra, InfraType};
use crate::testnet::Testnet;

pub const RETH_DATA_SUBDIRS: [&str; 4] = ["db", "static_files", "blobstore", "invalid_block_hooks"];
pub const MALACHITE_DATA_SUBDIRS: [&str; 2] = ["store.db", "wal"];

/// Define which data and/or infrastructure to clean.
#[derive(PartialEq, Debug, Clone, Copy)]
pub(crate) struct Scope {
    /// Remove infrastructure (includes node data and monitoring)
    pub testnet_infra: bool,
    /// Remove monitoring services data
    pub monitoring_data: bool,
    /// Remove execution layer data
    execution_data: bool,
    /// Remove consensus layer data
    consensus_data: bool,
}

impl Scope {
    pub fn from_cli_flags(
        all: bool,
        data: bool,
        execution_data: bool,
        consensus_data: bool,
    ) -> Self {
        let execution_data = data || execution_data;
        let consensus_data = data || consensus_data;
        let no_data_specified = !execution_data && !consensus_data;
        Self {
            testnet_infra: all || no_data_specified,
            monitoring_data: all,
            consensus_data,
            execution_data,
        }
    }
}

/// Clean up testnet-related files, directories, infrastructure, and running processes.
///
/// `scope` defines which node data is removed. Cleanup is best-effort:
/// failed steps are logged as warnings and later cleanup steps still run.
pub async fn clean(testnet: &Testnet, scope: Scope) {
    match testnet.infra_data.infra_type {
        InfraType::Local => {
            let Ok(local_infra) = testnet.local_infra() else {
                warn!("⚠️ Cannot access local infrastructure to clean");
                return;
            };
            clean_local(scope, testnet, local_infra).await
        }
        InfraType::Remote => {
            let Ok(remote_infra) = testnet.remote_infra() else {
                warn!("⚠️ Cannot access remote infrastructure to clean");
                return;
            };
            clean_remote(scope, testnet, remote_infra).await
        }
    }
    info!("✅ Testnet cleaned");
}

/// Clean local infrastructure and/or data according to `scope`
async fn clean_local(scope: Scope, testnet: &Testnet, local_infra: Arc<LocalInfra>) {
    stop_containers(&testnet.infra);

    if scope.monitoring_data {
        clean_monitoring(&testnet.infra);
    } else {
        warn!("Monitoring services may still be running; run `quake monitoring` to stop and clean them");
    }

    if scope.testnet_infra {
        remove_testnet_dir(&testnet.dir);
    } else {
        for name in testnet.nodes_metadata.node_names() {
            if scope.execution_data {
                local_infra.clean_reth_data(&name);
            }
            if scope.consensus_data {
                local_infra.clean_malachite_data(&name);
            }
        }
    }
}

/// Clean remote infrastructure and/or data according to `scope`
///
/// On remote infra, cleaning the infrastructure will destroy every EC2 instance.
/// There's no point in stopping nodes or wiping monitoring data first.
async fn clean_remote(scope: Scope, testnet: &Testnet, remote_infra: Arc<RemoteInfra>) {
    if scope.testnet_infra {
        if let Err(err) = remote_infra.ssm_tunnels.stop().await {
            warn!(%err, "⚠️ Failed to terminate SSM sessions");
        }
        if let Err(err) = destroy_remote_infra(&remote_infra) {
            // Keep the testnet directory: it may contain the Terraform state
            warn!(%err, "⚠️ Failed to destroy remote infrastructure");
        } else {
            remove_testnet_dir(&testnet.dir);
        }
    } else {
        stop_containers(&testnet.infra);
        if scope.execution_data {
            remote_infra.clean_reth_data();
        }
        if scope.consensus_data {
            remote_infra.clean_malachite_data();
        }
    }
}

fn stop_containers(infra: &Arc<dyn InfraProvider>) {
    if let Err(err) = infra.down(&[]) {
        warn!(%err, "⚠️ Failed to stop and remove containers");
    } else {
        debug!("Testnet is down");
    }
}

fn remove_testnet_dir(dir: &Path) {
    if !dir.exists() {
        return;
    }
    if let Err(err) = fs::remove_dir_all(dir) {
        warn!(dir=%dir.display(), "⚠️ Failed to remove testnet data: {err}");
    } else {
        debug!(dir=%dir.display(), "Testnet data removed");
    }
}

fn clean_monitoring(infra: &Arc<dyn InfraProvider>) {
    if let Err(err) = infra.stop_monitoring() {
        warn!(%err, "⚠️ Failed to stop monitoring services; not cleaning monitoring data");
    } else {
        debug!("Monitoring services stopped");
        if let Err(err) = infra.clean_monitoring_data() {
            warn!(%err, "⚠️ Failed to remove monitoring data");
        } else {
            debug!("Monitoring data removed");
        }
    }
}

fn destroy_remote_infra(remote_infra: &Arc<RemoteInfra>) -> Result<()> {
    if remote_infra.terraform.has_state() {
        debug!("⬇️ Destroying remote infrastructure...");
        if let Err(err) = remote_infra.terraform.destroy(true) {
            bail!("Failed to destroy remote infrastructure: {err}");
        } else {
            info!("Remote infrastructure destroyed");
        }
    } else {
        warn!("No Terraform state found; skipping infrastructure destroy");
    }
    Ok(())
}
