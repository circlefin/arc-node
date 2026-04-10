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

use crate::testnet::Testnet;
use std::fs;
use tracing::{debug, info, warn};

pub const RETH_DATA_SUBDIRS: [&str; 4] = ["db", "static_files", "blobstore", "invalid_block_hooks"];
pub const MALACHITE_DATA_SUBDIRS: [&str; 2] = ["store.db", "wal"];

use crate::infra::InfraType;

/// Controls which node data [`crate::testnet::Testnet::clean`] removes.
#[derive(PartialEq, Debug, Clone, Copy)]
pub enum CleanScope {
    /// Don't remove any node data.
    Skip,
    /// Remove both data and configuration from consensus and execution layer.
    /// On remote infrastructure, also destroys AWS resources.
    Full,
    /// Remove only execution layer data, preserving configuration.
    ExecutionData,
    /// Remove only consensus layer data, preserving configuration.
    ConsensusData,
    /// Remove both consensus and execution layer data, preserving configuration.
    Data,
}

/// Derive a [`CleanScope`] from individual boolean flags.
pub fn clean_scope(
    data: bool,
    execution_data: bool,
    consensus_data: bool,
    monitoring: bool,
) -> CleanScope {
    match (data || execution_data, data || consensus_data) {
        (true, true) => CleanScope::Data,
        (true, false) => CleanScope::ExecutionData,
        (false, true) => CleanScope::ConsensusData,
        // No data flags: clean only monitoring without touching node data.
        (false, false) if monitoring => CleanScope::Skip,
        (false, false) => CleanScope::Full,
    }
}

/// Clean up testnet-related files, directories, infrastructure, and running processes.
///
/// `mode` controls which node data is removed. See [`CleanScope`] for the different strategies.
/// `include_monitoring` is orthogonal — any mode can be combined with monitoring cleanup.
pub async fn clean(testnet: &Testnet, mode: CleanScope, include_monitoring: bool) {
    // Stop containers first
    if let Err(err) = testnet.infra.down(&[]) {
        warn!(%err, "⚠️ Failed to stop and remove containers");
    } else {
        debug!("✅ Testnet is down");
    }
    if include_monitoring {
        match testnet.infra_data.infra_type {
            InfraType::Local => {
                if let Ok(local_infra) = testnet.local_infra() {
                    match local_infra.monitoring.stop() {
                        Ok(()) => debug!("✅ Monitoring containers stopped"),
                        Err(err) => warn!("⚠️ Failed to stop monitoring containers: {err:#}"),
                    }
                    match local_infra.monitoring.clean() {
                        Ok(()) => {
                            debug!(dir=%local_infra.monitoring.dir.display(), "✅ Monitoring data removed")
                        }
                        Err(err) => warn!("⚠️ Failed to remove monitoring data: {err:#}"),
                    }
                }
            }
            InfraType::Remote => {
                if let Ok(remote_infra) = testnet.remote_infra() {
                    match remote_infra.stop_monitoring() {
                        Ok(output) => info!(%output, "✅ Monitoring stopped on CC"),
                        Err(err) => warn!("⚠️ Failed to stop monitoring on CC: {err:#}"),
                    }
                    match remote_infra.clean_monitoring_data() {
                        Ok(output) => info!(%output, "✅ Monitoring data removed on CC"),
                        Err(err) => warn!("⚠️ Failed to remove monitoring data on CC: {err:#}"),
                    }
                }
            }
        }
    }

    match mode {
        // Nothing to do.
        CleanScope::Skip => {}
        CleanScope::Full => {
            if matches!(testnet.infra_data.infra_type, InfraType::Remote) {
                if let Ok(remote_infra) = testnet.remote_infra() {
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
                } else {
                    warn!("⚠️ No configuration for remote infrastructure found");
                }
            }
            if testnet.dir.exists() {
                debug!(dir=%testnet.dir.display(), "🗑️  Removing testnet data");
                if let Err(err) = fs::remove_dir_all(&testnet.dir) {
                    warn!(dir=%testnet.dir.display(), "Failed to remove testnet data: {err}");
                } else {
                    debug!(dir=%testnet.dir.display(), "✅ Testnet data removed");
                }
            }
        }
        CleanScope::ExecutionData | CleanScope::ConsensusData | CleanScope::Data => {
            match testnet.infra_data.infra_type {
                InfraType::Local => clean_node_data(testnet, &mode),
                InfraType::Remote => clean_remote_node_data(testnet, &mode),
            }
        }
    }
}

/// Remove per-node data subdirectories according to `mode`, leaving all configuration intact.
fn clean_node_data(testnet: &Testnet, mode: &CleanScope) {
    let Ok(local_infra) = testnet.local_infra() else {
        warn!("⚠️ Cannot access local infrastructure to clean node data");
        return;
    };
    for name in testnet.nodes_metadata.node_names() {
        match mode {
            CleanScope::ExecutionData => {
                local_infra.clean_reth_data(&name);
            }
            CleanScope::ConsensusData => {
                local_infra.clean_malachite_data(&name);
            }
            CleanScope::Data => {
                local_infra.clean_reth_data(&name);
                local_infra.clean_malachite_data(&name);
            }
            _ => unreachable!("clean_node_data called with unexpected mode"),
        }
    }
}

/// Remove per-node data on remote nodes according to `mode`, leaving all configuration intact.
fn clean_remote_node_data(testnet: &Testnet, mode: &CleanScope) {
    let remote_infra = match testnet.remote_infra() {
        Ok(r) => r,
        Err(err) => {
            warn!(%err, "⚠️ Cannot access remote infrastructure to clean node data");
            return;
        }
    };
    match mode {
        CleanScope::ExecutionData => {
            remote_infra.clean_reth_data();
        }
        CleanScope::ConsensusData => {
            remote_infra.clean_malachite_data();
        }
        CleanScope::Data => {
            remote_infra.clean_reth_data();
            remote_infra.clean_malachite_data();
        }
        _ => unreachable!("clean_remote_node_data called with unexpected mode"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_scope_no_flags() {
        assert_eq!(clean_scope(false, false, false, false), CleanScope::Full);
    }

    #[test]
    fn test_clean_scope_data() {
        assert_eq!(clean_scope(true, false, false, false), CleanScope::Data);
    }

    #[test]
    fn test_clean_scope_execution_data() {
        assert_eq!(
            clean_scope(false, true, false, false),
            CleanScope::ExecutionData
        );
    }

    #[test]
    fn test_clean_scope_consensus_data() {
        assert_eq!(
            clean_scope(false, false, true, false),
            CleanScope::ConsensusData
        );
    }

    #[test]
    fn test_clean_scope_monitoring_only() {
        assert_eq!(clean_scope(false, false, false, true), CleanScope::Skip);
    }

    #[test]
    fn test_clean_scope_data_and_monitoring() {
        assert_eq!(clean_scope(true, false, false, true), CleanScope::Data);
    }

    #[test]
    fn test_clean_scope_execution_and_consensus_data() {
        assert_eq!(clean_scope(false, true, true, false), CleanScope::Data);
    }
}
