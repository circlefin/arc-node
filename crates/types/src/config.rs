// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

use eyre::bail;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

use malachitebft_core_types::Height as _;

pub use malachitebft_app::config::{
    ConsensusConfig, LogFormat, LogLevel, LoggingConfig, MetricsConfig, NodeConfig, RuntimeConfig,
    ValueSyncConfig,
};

use crate::Height;

/// Base port for consensus (p2p) communication. Actual port is base port + node index.
pub const CONSENSUS_BASE_PORT: usize = 27000;

/// Base port for metrics endpoint. Actual port is base port + node index.
pub const METRICS_BASE_PORT: usize = 29000;

/// Base port for RPC server. Actual port is base port + node index.
pub const RPC_BASE_PORT: usize = 31000;

/// Malachite configuration options
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// A custom human-readable name for this node
    pub moniker: String,

    /// Log configuration options
    pub logging: LoggingConfig,

    /// Consensus configuration options
    pub consensus: ConsensusConfig,

    /// ValueSync configuration options
    pub value_sync: ValueSyncConfig,

    /// Metrics configuration options
    pub metrics: MetricsConfig,

    /// Runtime configuration options
    pub runtime: RuntimeConfig,

    /// Pruning configuration
    pub prune: PruningConfig,

    /// RPC config
    pub rpc: RpcConfig,

    /// Signing config
    pub signing: SigningConfig,
}

impl Config {
    pub fn validate(&self) -> eyre::Result<()> {
        if self.value_sync.enabled && self.value_sync.batch_size == 0 {
            bail!("when value_sync is enabled, batch_size must be greater than 0");
        }
        Ok(())
    }
}

impl NodeConfig for Config {
    fn moniker(&self) -> &str {
        &self.moniker
    }

    fn consensus(&self) -> &ConsensusConfig {
        &self.consensus
    }

    fn consensus_mut(&mut self) -> &mut ConsensusConfig {
        &mut self.consensus
    }

    fn value_sync(&self) -> &ValueSyncConfig {
        &self.value_sync
    }

    fn value_sync_mut(&mut self) -> &mut ValueSyncConfig {
        &mut self.value_sync
    }
}

/// Pruning configuration for consensus-layer data (commit certificates).
///
/// Historical blocks are always retrieved from EL — these settings only govern
/// how many commit certificates the CL stores locally.
///
/// Default: No pruning, run as archive node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PruningConfig {
    /// Keep certificates for the last N heights. Certificates for heights older than
    /// `current_height - certificates_distance` will be pruned.
    ///
    /// Mirrors reth's `--prune.*.distance` semantics: "keep last N blocks".
    /// Mutually exclusive with `certificates_before`.
    /// Setting this to 0 disables distance-based pruning.
    #[serde(default)]
    pub certificates_distance: u64,

    /// Prune all certificates at heights strictly below this value.
    ///
    /// Mutually exclusive with `certificates_distance`.
    /// Setting this to 0 disables height-based pruning.
    #[serde(default)]
    pub certificates_before: Height,
}

impl PruningConfig {
    /// Returns true if pruning is enabled, false otherwise.
    pub fn enabled(&self) -> bool {
        self.certificates_distance > 0 || self.certificates_before > Height::ZERO
    }

    /// Calculates the effective minimum certificates height to keep based on
    /// the current height.
    pub fn effective_certificates_min_height(&self, current_height: Height) -> Height {
        if self.certificates_before > Height::ZERO {
            self.certificates_before
        } else if self.certificates_distance > 0 {
            current_height.saturating_sub(self.certificates_distance)
        } else {
            Height::ZERO
        }
    }
}

/// RPC server configuration options.
///
/// Default: RPC disabled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcConfig {
    /// Enable the RPC server.
    pub enabled: bool,

    /// Address to bind the RPC server to
    pub listen_addr: SocketAddr,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_addr: format!("127.0.0.1:{RPC_BASE_PORT}")
                .parse()
                .expect("valid socket address"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SigningConfig {
    #[default]
    Local,
    Remote(RemoteSigningConfig),
}

/// Configuration for the consensus remote signing client
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteSigningConfig {
    pub endpoint: String,
    #[serde(with = "humantime_serde", default = "default_remote_signing_timeout")]
    pub timeout: Duration,
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default)]
    pub enable_tls: bool,
    #[serde(default)]
    pub tls_cert_path: Option<String>,
}

fn default_remote_signing_timeout() -> Duration {
    Duration::from_secs(30)
}

impl Default for RemoteSigningConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://0.0.0.0:10340".to_string(),
            timeout: default_remote_signing_timeout(),
            retry: RetryConfig::default(),
            enable_tls: false,
            tls_cert_path: None,
        }
    }
}

/// Retry configuration for gRPC calls
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_retries: usize,
    #[serde(with = "humantime_serde")]
    pub initial_backoff: Duration,
    #[serde(with = "humantime_serde")]
    pub max_backoff: Duration,
    pub backoff_multiplier: f32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            backoff_multiplier: 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod pruning {
        use super::Config;
        use crate::config::PruningConfig;
        use crate::Height;

        #[test]
        fn effective_certificates_min_height_both() {
            let config = PruningConfig {
                certificates_distance: 100,
                certificates_before: Height::new(50),
            };

            assert_eq!(
                config.effective_certificates_min_height(Height::new(200)),
                Height::new(50)
            );
            assert_eq!(
                config.effective_certificates_min_height(Height::new(120)),
                Height::new(50)
            );
            assert_eq!(
                config.effective_certificates_min_height(Height::new(80)),
                Height::new(50)
            );
            assert_eq!(
                config.effective_certificates_min_height(Height::new(50)),
                Height::new(50)
            );
            assert_eq!(
                config.effective_certificates_min_height(Height::new(10)),
                Height::new(50)
            );
            assert_eq!(
                config.effective_certificates_min_height(Height::new(0)),
                Height::new(50)
            );
        }

        #[test]
        fn effective_certificates_min_height_min_height_only() {
            let config_no_interval = PruningConfig {
                certificates_distance: 0,
                certificates_before: Height::new(50),
            };

            assert_eq!(
                config_no_interval.effective_certificates_min_height(Height::new(200)),
                Height::new(50)
            );
            assert_eq!(
                config_no_interval.effective_certificates_min_height(Height::new(120)),
                Height::new(50)
            );
            assert_eq!(
                config_no_interval.effective_certificates_min_height(Height::new(80)),
                Height::new(50)
            );
            assert_eq!(
                config_no_interval.effective_certificates_min_height(Height::new(50)),
                Height::new(50)
            );
            assert_eq!(
                config_no_interval.effective_certificates_min_height(Height::new(10)),
                Height::new(50)
            );
            assert_eq!(
                config_no_interval.effective_certificates_min_height(Height::new(0)),
                Height::new(50)
            );
        }

        #[test]
        fn effective_certificates_min_height_distance_only() {
            let config_no_min_height = PruningConfig {
                certificates_distance: 100,
                certificates_before: Height::new(0),
            };

            assert_eq!(
                config_no_min_height.effective_certificates_min_height(Height::new(200)),
                Height::new(100)
            );
            assert_eq!(
                config_no_min_height.effective_certificates_min_height(Height::new(120)),
                Height::new(20)
            );
            assert_eq!(
                config_no_min_height.effective_certificates_min_height(Height::new(80)),
                Height::new(0)
            );
            assert_eq!(
                config_no_min_height.effective_certificates_min_height(Height::new(50)),
                Height::new(0)
            );
            assert_eq!(
                config_no_min_height.effective_certificates_min_height(Height::new(10)),
                Height::new(0)
            );
            assert_eq!(
                config_no_min_height.effective_certificates_min_height(Height::new(0)),
                Height::new(0)
            );
        }

        #[test]
        fn config_validates_batch_size() {
            let mut config = Config::default();
            assert!(config.validate().is_ok());

            config.value_sync.batch_size = 10;
            assert!(config.validate().is_ok());

            config.value_sync.batch_size = 1;
            assert!(config.validate().is_ok());

            config.value_sync.batch_size = 0;
            assert!(config.validate().is_err());

            config.value_sync.enabled = false;
            assert!(config.validate().is_ok());
        }
    }
}
