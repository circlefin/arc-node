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

    /// Execution-layer config
    pub execution: ExecutionConfig,

    /// Signing config
    pub signing: SigningConfig,
}

impl Config {
    pub fn validate(&self) -> eyre::Result<()> {
        if self.value_sync.enabled && self.value_sync.batch_size == 0 {
            bail!("when value_sync is enabled, batch_size must be greater than 0");
        }
        if self.execution.persistence_backpressure_threshold == 0 {
            bail!("execution.persistence_backpressure_threshold must be greater than 0");
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

    /// Bearer token required by the privileged RPC routes.
    ///
    /// Set from `--rpc.admin-token-file`. While it is `None` the routes that
    /// mutate node state are not registered at all, so the listener serves only
    /// the read-only monitoring endpoints. It is never read from or written to a
    /// config file, so the credential lives in the token file alone.
    #[serde(skip)]
    pub admin_token: Option<AdminToken>,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_addr: format!("127.0.0.1:{RPC_BASE_PORT}")
                .parse()
                .expect("valid socket address"),
            admin_token: None,
        }
    }
}

/// Bearer credential a caller must present to reach the privileged RPC routes.
///
/// `Debug` is redacted, so dumping the configuration (`trace!(?config)`) cannot
/// leak the token, and comparisons do not exit early on the first wrong byte.
#[derive(Clone)]
pub struct AdminToken(String);

impl AdminToken {
    /// Build a token from the contents of an admin token file.
    ///
    /// Surrounding whitespace is trimmed, which is what an operator gets from
    /// `openssl rand -hex 32 > token`. An empty file is rejected rather than
    /// accepted as an empty credential.
    pub fn from_file_contents(contents: &str) -> eyre::Result<Self> {
        let token = contents.trim();

        if token.is_empty() {
            bail!("admin token file is empty");
        }

        Ok(Self(token.to_owned()))
    }

    /// Whether a presented credential matches this token.
    pub fn matches(&self, presented: &str) -> bool {
        bytes_eq_no_early_exit(self.0.as_bytes(), presented.as_bytes())
    }
}

impl std::fmt::Debug for AdminToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdminToken(redacted)")
    }
}

impl PartialEq for AdminToken {
    fn eq(&self, other: &Self) -> bool {
        bytes_eq_no_early_exit(self.0.as_bytes(), other.0.as_bytes())
    }
}

/// Compare two byte strings without returning early on the first difference, so
/// the count of matching leading bytes does not show up in the response time.
fn bytes_eq_no_early_exit(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }

    diff == 0
}

/// Execution-layer tuning parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Whether persistence backpressure is enabled.
    #[serde(default)]
    pub persistence_backpressure: bool,

    /// Maximum canonical-minus-persisted gap the EL may have before
    /// persistence backpressure is applied during startup replay.
    ///
    /// Backpressure begins once the gap reaches this threshold.
    /// Only takes effect when `persistence_backpressure` is true.
    #[serde(default = "ExecutionConfig::default_persistence_backpressure_threshold")]
    pub persistence_backpressure_threshold: u64,
}

impl ExecutionConfig {
    const fn default_persistence_backpressure_threshold() -> u64 {
        16
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            persistence_backpressure: false,
            persistence_backpressure_threshold: Self::default_persistence_backpressure_threshold(),
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

    mod admin_token {
        use super::AdminToken;
        use crate::config::RpcConfig;

        #[test]
        fn trims_surrounding_whitespace() {
            let token = AdminToken::from_file_contents("  s3cret\n").unwrap();
            assert!(token.matches("s3cret"));
            assert!(!token.matches("  s3cret\n"));
        }

        #[test]
        fn rejects_an_empty_file() {
            assert!(AdminToken::from_file_contents("").is_err());
            assert!(AdminToken::from_file_contents("   \n\t").is_err());
        }

        #[test]
        fn does_not_match_a_prefix_or_a_different_token() {
            let token = AdminToken::from_file_contents("s3cret").unwrap();
            assert!(!token.matches("s3cre"));
            assert!(!token.matches("s3cretx"));
            assert!(!token.matches(""));
            assert!(!token.matches("S3CRET"));
        }

        #[test]
        fn debug_output_is_redacted() {
            let token = AdminToken::from_file_contents("s3cret").unwrap();
            let rendered = format!("{token:?}");
            assert!(!rendered.contains("s3cret"), "rendered: {rendered}");

            let config = RpcConfig {
                admin_token: Some(token),
                ..RpcConfig::default()
            };
            let rendered = format!("{config:?}");
            assert!(!rendered.contains("s3cret"), "rendered: {rendered}");
        }

        #[test]
        fn is_never_serialised_into_a_config_file() {
            let config = RpcConfig {
                admin_token: Some(AdminToken::from_file_contents("s3cret").unwrap()),
                ..RpcConfig::default()
            };

            let serialised = serde_json::to_string(&config).unwrap();
            assert!(!serialised.contains("s3cret"), "serialised: {serialised}");
            assert!(
                !serialised.contains("admin_token"),
                "serialised: {serialised}"
            );

            let round_tripped: RpcConfig = serde_json::from_str(&serialised).unwrap();
            assert_eq!(round_tripped.admin_token, None);
        }
    }

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

        #[test]
        fn config_rejects_zero_persistence_backpressure_threshold() {
            let mut config = Config::default();
            assert!(config.validate().is_ok());

            config.execution.persistence_backpressure_threshold = 0;
            assert!(config.validate().is_err());
        }
    }
}
