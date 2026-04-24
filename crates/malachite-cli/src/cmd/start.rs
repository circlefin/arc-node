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

use std::net::SocketAddr;
use std::path::PathBuf;

use arc_consensus_types::rpc_sync::SyncEndpointUrl;
use clap::Parser;
use color_eyre::eyre;
use serde::{Deserialize, Serialize};
use tracing::info;
use url::Url;

use arc_consensus_types::Address;
use malachitebft_app::consensus::Multiaddr;

use crate::file::save_priv_validator_key;
use crate::new::generate_private_keys;

/// Tokio single-threaded runtime flavor.
pub const RUNTIME_SINGLE_THREADED: &str = "single-threaded";

/// Tokio multi-threaded runtime flavor.
pub const RUNTIME_MULTI_THREADED: &str = "multi-threaded";

/// Start command to run a node.
///
/// Derives `clap::Parser` for CLI parsing and `serde::Serialize` /
/// `serde::Deserialize` for TOML-based deserialization by external
/// tooling. Deployment-specific fields are marked `#[serde(skip)]`, so
/// TOML cannot set them. They inherit their values from
/// `StartCmd::default()` via the container-level `#[serde(default)]`.
#[derive(Parser, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StartCmd {
    // ===== Node Identity =====
    /// A custom human-readable name for this node.
    ///
    /// If not provided, a random moniker will be generated.
    #[clap(long, value_name = "NAME")]
    #[serde(skip)]
    pub moniker: Option<String>,

    // ===== P2P Networking =====
    /// P2P listen multiaddr
    ///
    /// Example: /ip4/172.19.0.5/tcp/27000
    #[clap(
        long = "p2p.addr",
        value_name = "MULTIADDR",
        default_value = "/ip4/0.0.0.0/tcp/27000"
    )]
    #[serde(skip)]
    pub p2p_addr: Multiaddr,

    /// Comma-separated list of persistent peer multiaddrs to connect to
    ///
    /// Example: /ip4/172.19.0.21/tcp/27000,/ip4/172.19.0.22/tcp/27000
    #[clap(long = "p2p.persistent-peers", value_delimiter = ',', num_args = 0..)]
    #[serde(skip)]
    pub p2p_persistent_peers: Vec<Multiaddr>,

    /// Only allow connections to/from persistent peers.
    ///
    /// When set, the node will reject connections from peers that are not
    /// in the persistent peers list. Useful for sentry node setups where
    /// a validator should only communicate with known trusted peers.
    #[clap(long = "p2p.persistent-peers-only")]
    #[serde(skip)]
    pub p2p_persistent_peers_only: bool,

    /// Enable gossipsub explicit peering for persistent peers.
    ///
    /// When enabled, persistent peers are added as explicit peers in GossipSub,
    /// meaning a node always sends and forwards messages to its explicit peers,
    /// regardless of mesh membership.
    #[clap(long = "gossipsub.explicit-peering", help_heading = "GossipSub")]
    #[serde(skip)]
    pub gossipsub_explicit_peering: bool,

    /// Enable gossipsub mesh peer scoring / prioritization.
    ///
    /// When enabled, peers are scored and prioritized based on their type
    /// during mesh formation.
    #[clap(long = "gossipsub.mesh-prioritization", help_heading = "GossipSub")]
    #[serde(skip)]
    pub gossipsub_mesh_prioritization: bool,

    /// Gossipsub network load profile controlling mesh size and bandwidth.
    ///
    /// - low:     fewer mesh peers, lower bandwidth (mesh_n=3)
    /// - average: balanced for typical deployments (mesh_n=6) [default]
    /// - high:    more mesh peers, higher bandwidth (mesh_n=10)
    #[clap(
        long = "gossipsub.load",
        value_name = "PROFILE",
        help_heading = "GossipSub",
        value_parser = ["low", "average", "high"]
    )]
    #[serde(skip)]
    pub gossipsub_load: Option<String>,

    // ===== Logging =====
    /// Log level
    #[clap(long, value_name = "LOG_LEVEL")]
    pub log_level: Option<malachitebft_config::LogLevel>,

    /// Log format
    #[clap(long, value_name = "LOG_FORMAT")]
    pub log_format: Option<malachitebft_config::LogFormat>,

    // ===== Discovery =====
    /// Enable peer discovery
    #[clap(long)]
    pub discovery: bool,

    /// Number of outbound peers for discovery
    #[clap(
        long = "discovery.num-outbound-peers",
        value_name = "COUNT",
        default_value = "20"
    )]
    pub discovery_num_outbound_peers: usize,

    /// Number of inbound peers for discovery
    #[clap(
        long = "discovery.num-inbound-peers",
        value_name = "COUNT",
        default_value = "20"
    )]
    pub discovery_num_inbound_peers: usize,

    // ===== Consensus =====
    /// Disable consensus protocol participation.
    ///
    /// When set, the node only runs the synchronization protocol
    /// and does not subscribe to consensus-related gossip topics.
    /// Use for sync-only full nodes.
    #[clap(long, conflicts_with = "validator")]
    pub no_consensus: bool,

    /// Run as a validator node.
    ///
    /// When set, the node loads its consensus signing key,
    /// signs a validator proof (ADR-006), and advertises a
    /// validator identity on the P2P network. Requires
    /// `--suggested-fee-recipient` so tips and rewards go to
    /// an explicit address rather than being silently burned.
    ///
    /// Without this flag the node runs as a full node: it
    /// participates in gossip but does not sign votes or proposals.
    #[clap(
        long,
        conflicts_with_all = ["no_consensus", "follow"],
        requires = "suggested_fee_recipient",
    )]
    pub validator: bool,

    // ===== Value Sync =====
    /// Enable value sync
    #[clap(
        long,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
    )]
    pub value_sync: bool,

    // ===== Execution Layer Connection =====
    /// The path to the Ethereum IPC socket. For reth with default settings,
    /// this will be /tmp/reth.ipc. To change the path in reth, you need to
    /// provide the `--ipcpath` flag.
    ///
    /// This is recommended option if the consensus and execution layers are colocated on the same machine.
    #[clap(long, value_name = "PATH")]
    #[serde(skip)]
    pub eth_socket: Option<String>,

    /// The path to the execution engine socket. To enable this in reth, you
    /// need to provide the `--auth-ipc` and `--auth-ipc.path` flags.
    ///
    /// This is recommended option if the consensus and execution layers are colocated on the same machine.
    #[clap(long, value_name = "PATH")]
    #[serde(skip)]
    pub execution_socket: Option<String>,

    /// The URL of the Ethereum JSON-RPC API. If the Ethereum full node is
    /// running on the same computer with the default port, this will be
    /// http://localhost:8545. Most of the execution clients provide this
    /// functionality.
    ///
    /// Use this option if the consensus and executation layer are on different machines.
    #[clap(long, value_name = "URL")]
    #[serde(skip)]
    pub eth_rpc_endpoint: Option<Url>,

    /// The URL of the execution engine API. If the execution engine is running
    /// on the same computer with the default port, this will be
    /// http://localhost:8551.
    ///
    /// Use this option if the consensus and executation layer are on different machines.
    #[clap(long, value_name = "URL")]
    #[serde(skip)]
    pub execution_endpoint: Option<Url>,

    /// The WebSocket URL of the execution engine. Used for subscribing to
    /// real-time execution layer events (e.g. persisted block notifications).
    ///
    /// If omitted, derived from --eth-rpc-endpoint using the convention
    /// (scheme http→ws / https→wss, port + 1).
    ///
    /// Example: ws://localhost:8546
    #[clap(long, value_name = "URL")]
    #[serde(skip)]
    pub execution_ws_endpoint: Option<Url>,

    /// Enable persistence backpressure during startup replay. When enabled,
    /// the consensus layer waits for the execution layer to persist blocks
    /// before replaying further.
    ///
    /// Requires --execution-ws-endpoint (RPC mode) or IPC mode.
    #[clap(long = "execution-persistence-backpressure")]
    pub execution_persistence_backpressure: bool,

    /// Number of blocks the execution layer is allowed to lag behind the
    /// consensus layer before persistence backpressure is applied.
    ///
    /// Only takes effect when --execution-persistence-backpressure is enabled.
    /// Large values weaken backpressure and may allow the execution layer
    /// to accumulate a significant unpersisted block buffer.
    #[clap(
        long = "execution-persistence-backpressure-threshold",
        value_name = "BLOCKS",
        default_value = "16"
    )]
    pub execution_persistence_backpressure_threshold: u64,

    /// The path to the JWT secret file shared by Malachite and the execution
    /// engine. This is a mandatory form of authentication which ensures that
    /// Malachite has the authority to control the execution engine.
    ///
    /// Use this option if the consensus and executation layer are on different machines.
    #[clap(long, value_name = "PATH")]
    #[serde(skip)]
    pub execution_jwt: Option<String>,

    // ===== Metrics =====
    /// Enable Prometheus metrics and set listen address.
    ///
    /// If omitted, metrics are disabled.
    /// If provided, metrics are enabled on the given address.
    ///
    /// Example: 0.0.0.0:29000
    #[clap(long, value_name = "ADDR")]
    #[serde(skip)]
    pub metrics: Option<SocketAddr>,

    // ===== RPC =====
    /// Enable RPC server and set listen address.
    ///
    /// If omitted, RPC is disabled.
    /// If provided, RPC is enabled on the given address.
    ///
    /// Example: 0.0.0.0:31000
    #[clap(long = "rpc.addr", value_name = "ADDR")]
    #[serde(skip)]
    pub rpc_addr: Option<SocketAddr>,

    // ===== Runtime =====
    /// Tokio runtime flavor to use.
    #[clap(
        long = "runtime.flavor",
        value_name = "FLAVOR",
        default_value = RUNTIME_MULTI_THREADED,
        value_parser = [RUNTIME_SINGLE_THREADED, RUNTIME_MULTI_THREADED]
    )]
    pub runtime_flavor: String,

    /// Number of worker threads for the Tokio multi-threaded runtime.
    ///
    /// If not set, the runtime will default to the number of CPU cores.
    /// This option is ignored if the single-threaded runtime is selected.
    #[clap(long = "runtime.worker-threads", value_name = "COUNT")]
    pub worker_threads: Option<usize>,

    // ===== Pruning presets =====
    /// Full-node pruning preset. Sets --prune.certificates.distance 237600.
    /// Mutually exclusive with --minimal and the individual --prune.certificates.* flags.
    /// Note: on the CL, both --full and --minimal retain the same certificate
    /// history for now; the distinction applies at the EL layer.
    #[clap(
        long = "full",
        default_value_t = false,
        conflicts_with_all = &["minimal", "prune_certificates_distance", "prune_certificates_before"],
        help_heading = "Arc pruning presets"
    )]
    #[serde(skip)]
    pub full: bool,

    /// Minimal-storage pruning preset. Sets --prune.certificates.distance 237600.
    /// Mutually exclusive with --full and the individual --prune.certificates.* flags.
    /// Note: on the CL, both --full and --minimal retain the same certificate
    /// history for now; the distinction applies at the EL layer.
    #[clap(
        long = "minimal",
        default_value_t = false,
        conflicts_with_all = &["full", "prune_certificates_distance", "prune_certificates_before"],
        help_heading = "Arc pruning presets"
    )]
    #[serde(skip)]
    pub minimal: bool,

    // ===== Pruning =====
    /// Keep certificates for the last N heights. Certificates for heights older than
    /// current_height - N will be pruned. Mirrors reth's --prune.*.distance semantics.
    /// Setting this to 0 disables distance-based pruning.
    /// Mutually exclusive with --prune.certificates.before and the pruning presets.
    #[clap(
        long = "prune.certificates.distance",
        alias = "pruning.block-interval",
        value_name = "COUNT",
        default_value = "0",
        conflicts_with_all = &["prune_certificates_before", "full", "minimal"]
    )]
    pub prune_certificates_distance: u64,

    /// Prune all certificates at heights strictly below this value.
    /// Setting this to 0 disables height-based pruning.
    /// Mutually exclusive with --prune.certificates.distance and the pruning presets.
    #[clap(
        long = "prune.certificates.before",
        alias = "pruning.min-height",
        value_name = "HEIGHT",
        default_value = "0",
        conflicts_with_all = &["prune_certificates_distance", "full", "minimal"]
    )]
    pub prune_certificates_before: u64,

    // ===== Other =====
    /// The path to the validator private key file.
    ///
    /// This file contains the private key used for:
    /// - P2P/libp2p network identity (always required)
    /// - Consensus message signing (only when using local signing, not with --signing.remote)
    ///
    /// When using --signing.remote, if this file doesn't exist, it will be automatically
    /// generated with a random key for P2P network identity purposes.
    ///
    /// Default: {home_dir}/config/priv_validator_key.json
    /// where `home_dir` is the directory provided with the `--home` global option
    #[clap(long, value_name = "PATH")]
    #[serde(skip)]
    pub private_key: Option<PathBuf>,

    /// Profiling server bind address
    #[clap(
        long = "pprof.addr",
        value_name = "ADDR",
        default_value = "0.0.0.0:6060"
    )]
    #[serde(skip)]
    pub pprof_addr: String,

    /// Activate jemalloc heap profiling at startup.
    ///
    /// When built with the `pprof` feature, heap profiling infrastructure is
    /// always available but inactive by default. This flag activates it so
    /// that the `/debug/pprof/allocs` endpoint returns meaningful data.
    #[clap(long = "pprof.heap-prof", default_value_t = false)]
    pub pprof_heap_prof: bool,

    /// 20-byte ethereum-style address to receive tips (transactions' priority fee)
    /// and rewards.
    ///
    /// The execution layer deposits fees and rewards to this address whenever the
    /// validator successfully proposes a new block. The zero address is rejected
    /// because rewards credited to it bypass the native-coin blocklist and are
    /// unrecoverable.
    #[clap(long, value_name = "ADDRESS", value_parser = parse_non_zero_address)]
    #[serde(skip)]
    pub suggested_fee_recipient: Option<Address>,

    /// Skip database schema upgrade on startup.
    ///
    /// WARNING: This flag should only be used when a database upgrade failed.
    /// Not upgrading the database may lead to errors or data corruption.
    #[clap(long = "db.skip-upgrade")]
    #[serde(skip)]
    pub skip_db_upgrade: bool,

    // ===== Signing =====
    /// Use remote signing with the specified endpoint URL
    ///
    /// If not provided, local signing will be used (default behavior).
    ///
    /// Only meaningful together with `--validator`; for full and sync-only nodes
    /// no consensus signing occurs.
    ///
    /// Example: http://validator-signer-proxy:10340
    #[clap(
        long = "signing.remote",
        value_name = "ENDPOINT",
        requires = "validator"
    )]
    #[serde(skip)]
    pub signing_remote: Option<String>,

    /// Path to TLS certificate for remote signing
    ///
    /// Only used when --signing.remote is specified.
    /// If provided, TLS will be automatically enabled.
    #[clap(
        long = "signing.tls-cert-path",
        value_name = "PATH",
        requires = "signing_remote"
    )]
    #[serde(skip)]
    pub signing_tls_cert_path: Option<String>,

    /// Enable RPC sync mode (follow with verification).
    ///
    /// In RPC sync mode, the node fetches blocks from trusted RPC endpoints
    /// instead of participating in consensus. This is useful for running
    /// read-only nodes that sync from validators.
    ///
    /// When no --follow.endpoint is provided, a default endpoint is resolved
    /// from the chain ID at startup.
    #[clap(long = "follow")]
    #[serde(skip)]
    pub follow: bool,

    /// RPC endpoint to fetch blocks from in RPC sync mode.
    /// This flag can be repeated.
    /// Optional when --follow is set; defaults are resolved from the chain ID.
    ///
    /// Format:
    /// <http_url>[,<ws_protocol>=<port_or_host_or_host:port>]
    /// where <http_url> is an http:// or https:// URL,
    /// and <ws_protocol> is either ws or wss.
    ///
    /// The WebSocket override value can be:
    /// - A port number (e.g., wss=8546) — same host, explicit port
    /// - A hostname (e.g., wss=ws.example.com) — different host, default port
    /// - A host:port pair (e.g., wss=ws.example.com:1212) — different host and port
    ///
    /// If not specified, the WebSocket URL is derived from the HTTP URL
    /// (scheme http->ws / https->wss, port HTTP+1 if non-default).
    ///
    /// Examples:
    ///   http://validator1:8545,ws=8546
    ///   https://validator1:8545,wss=8546
    ///   https://example.com,wss=ws.example.com
    ///   https://example.com,wss=ws.example.com:1212
    #[clap(long = "follow.endpoint", value_name = "ENDPOINT", requires = "follow")]
    #[serde(skip)]
    pub follow_endpoints: Vec<SyncEndpointUrl>,
}

fn parse_non_zero_address(s: &str) -> Result<Address, String> {
    let addr: Address = s.parse().map_err(|e| format!("invalid address: {e}"))?;
    if addr.to_alloy_address().is_zero() {
        return Err(
            "must not be the zero address; rewards credited to 0x0 bypass the \
             native-coin blocklist and are unrecoverable"
                .to_string(),
        );
    }
    Ok(addr)
}

impl Default for StartCmd {
    fn default() -> Self {
        Self {
            moniker: None,
            p2p_addr: "/ip4/0.0.0.0/tcp/27000".parse().expect("valid multiaddr"),
            p2p_persistent_peers: Vec::new(),
            p2p_persistent_peers_only: false,
            gossipsub_explicit_peering: false,
            gossipsub_mesh_prioritization: false,
            gossipsub_load: None,
            log_level: None,
            log_format: None,
            discovery: false,
            discovery_num_outbound_peers: 20,
            discovery_num_inbound_peers: 20,
            no_consensus: false,
            validator: false,
            value_sync: true,
            eth_socket: None,
            execution_socket: None,
            eth_rpc_endpoint: None,
            execution_endpoint: None,
            execution_ws_endpoint: None,
            execution_persistence_backpressure: false,
            execution_persistence_backpressure_threshold: 16,
            execution_jwt: None,
            metrics: None,
            rpc_addr: None,
            runtime_flavor: RUNTIME_MULTI_THREADED.to_string(),
            worker_threads: None,
            full: false,
            minimal: false,
            prune_certificates_distance: 0,
            prune_certificates_before: 0,
            private_key: None,
            pprof_addr: "0.0.0.0:6060".to_string(),
            pprof_heap_prof: false,
            suggested_fee_recipient: None,
            skip_db_upgrade: false,
            signing_remote: None,
            signing_tls_cert_path: None,
            follow: false,
            follow_endpoints: Vec::new(),
        }
    }
}

impl StartCmd {
    /// Generate CLI flag strings for all non-default, manifest-configurable fields.
    ///
    /// Each field maps 1:1 to its `#[clap(long = "...")]` name. Only fields
    /// whose values differ from `StartCmd::default()` are emitted. Deployment-
    /// specific fields (marked `#[serde(skip)]`) are included here because
    /// callers populate these before invoking.
    pub fn to_cli_flags(&self) -> Vec<String> {
        let defaults = Self::default();
        let mut flags = Vec::new();

        macro_rules! push_each {
            ($flag:literal, $items:expr) => {
                for item in $items {
                    flags.push(format!(concat!("--", $flag, "={}"), item));
                }
            };
        }
        macro_rules! push_if_some {
            ($flag:literal, $opt:expr) => {
                if let Some(ref v) = $opt {
                    flags.push(format!(concat!("--", $flag, "={}"), v));
                }
            };
        }
        macro_rules! push_if {
            ($flag:literal, $cond:expr) => {
                if $cond {
                    flags.push(concat!("--", $flag).to_string());
                }
            };
        }
        macro_rules! push_if_non_default {
            ($flag:literal, $field:ident) => {
                if self.$field != defaults.$field {
                    flags.push(format!(concat!("--", $flag, "={}"), self.$field));
                }
            };
        }

        // --- Deployment-specific fields (set by quake at runtime) ---

        push_if_some!("moniker", self.moniker);
        push_if_non_default!("p2p.addr", p2p_addr);
        if !self.p2p_persistent_peers.is_empty() {
            let peers: Vec<String> = self
                .p2p_persistent_peers
                .iter()
                .map(|p| p.to_string())
                .collect();
            flags.push(format!("--p2p.persistent-peers={}", peers.join(",")));
        }
        push_if!("p2p.persistent-peers-only", self.p2p_persistent_peers_only);
        push_if!(
            "gossipsub.explicit-peering",
            self.gossipsub_explicit_peering
        );
        push_if!(
            "gossipsub.mesh-prioritization",
            self.gossipsub_mesh_prioritization
        );
        push_if_some!("gossipsub.load", self.gossipsub_load);

        // --- Manifest-configurable fields ---

        push_if_some!("log-level", self.log_level);
        push_if_some!("log-format", self.log_format);
        push_if!("discovery", self.discovery);
        push_if_non_default!("discovery.num-outbound-peers", discovery_num_outbound_peers);
        push_if_non_default!("discovery.num-inbound-peers", discovery_num_inbound_peers);
        push_if!("no-consensus", self.no_consensus);
        push_if!("validator", self.validator);
        push_if_non_default!("value-sync", value_sync);
        push_if!(
            "execution-persistence-backpressure",
            self.execution_persistence_backpressure
        );
        push_if_non_default!(
            "execution-persistence-backpressure-threshold",
            execution_persistence_backpressure_threshold
        );
        push_if_non_default!("runtime.flavor", runtime_flavor);
        push_if_some!("runtime.worker-threads", self.worker_threads);
        push_if_non_default!("prune.certificates.distance", prune_certificates_distance);
        push_if_non_default!("prune.certificates.before", prune_certificates_before);

        // --- Deployment-specific fields (set by quake, continued) ---

        push_if_some!("eth-socket", self.eth_socket);
        push_if_some!("execution-socket", self.execution_socket);
        push_if_some!("eth-rpc-endpoint", self.eth_rpc_endpoint);
        push_if_some!("execution-endpoint", self.execution_endpoint);
        push_if_some!("execution-ws-endpoint", self.execution_ws_endpoint);
        push_if_some!("execution-jwt", self.execution_jwt);
        push_if_some!("metrics", self.metrics);
        push_if_some!("rpc.addr", self.rpc_addr);
        push_if!("full", self.full);
        push_if!("minimal", self.minimal);
        if let Some(ref path) = self.private_key {
            flags.push(format!("--private-key={}", path.display()));
        }
        push_if_non_default!("pprof.addr", pprof_addr);
        push_if!("pprof.heap-prof", self.pprof_heap_prof);
        push_if_some!("suggested-fee-recipient", self.suggested_fee_recipient);
        push_if!("db.skip-upgrade", self.skip_db_upgrade);
        push_if_some!("signing.remote", self.signing_remote);
        push_if_some!("signing.tls-cert-path", self.signing_tls_cert_path);
        push_if!("follow", self.follow);
        push_each!("follow.endpoint", &self.follow_endpoints);

        flags
    }

    /// Validates that conflicting options are not provided simultaneously.
    ///
    /// This method ensures that users don't specify both IPC and RPC options
    /// at the same time, as they represent different communication methods.
    pub fn validate(&self) -> eyre::Result<()> {
        // Check if both IPC and RPC options are provided
        let has_ipc_options = self.eth_socket.is_some() || self.execution_socket.is_some();
        let has_rpc_options = self.eth_rpc_endpoint.is_some()
            || self.execution_endpoint.is_some()
            || self.execution_jwt.is_some();

        if has_ipc_options && has_rpc_options {
            return Err(eyre::eyre!(
                "Conflicting options detected: Cannot specify both IPC and RPC options simultaneously.\n\
                IPC options: --eth-socket, --execution-socket\n\
                RPC options: --eth-rpc-endpoint, --execution-endpoint, --execution-jwt\n\
                Please choose either IPC (for local communication) or RPC (for remote communication)."
            ));
        }

        // Validate persistent-peers-only configuration
        if self.p2p_persistent_peers_only && self.p2p_persistent_peers.is_empty() {
            return Err(eyre::eyre!(
                "--p2p.persistent-peers-only requires at least one --p2p.persistent-peers entry.\n\
                Without persistent peers, the node would reject all connections."
            ));
        }

        Ok(())
    }

    pub fn private_key_file(&self, default: PathBuf) -> eyre::Result<PathBuf> {
        let priv_key_path = self.private_key.as_ref().unwrap_or(&default);

        if priv_key_path.exists() {
            info!(path = %priv_key_path.display(), "Using existing private key file");
            return Ok(priv_key_path.clone());
        }

        // The private key file does not exist.
        if self.signing_remote.is_some() {
            // With remote signing, we can auto-generate a key for P2P identity.
            info!(file = %priv_key_path.display(), "Generating private key for P2P network identity");
            let private_keys = generate_private_keys(1, false)?;
            let priv_validator_key = private_keys[0].clone();
            save_priv_validator_key(priv_key_path, &priv_validator_key)?;
            info!(
                path = %priv_key_path.display(),
                "✅ Private key generated successfully for P2P network identity"
            );
            Ok(priv_key_path.clone())
        } else if self.private_key.is_some() {
            // A specific key file was requested but not found.
            Err(eyre::eyre!(
                "The specified private key file does not exist: {}",
                priv_key_path.display()
            ))
        } else {
            // Using default path, but the key file is not found.
            Err(eyre::eyre!(
                "The default private key file does not exist: {}. \n\n\
                 You can generate it by running 'arc-node-consensus init' or \
                 provide a path to the existing file using the --private-key option.",
                priv_key_path.display()
            ))
        }
    }

    /// Get the moniker, generating a random one if not provided.
    pub fn get_moniker(&self) -> String {
        self.moniker.clone().unwrap_or_else(|| {
            use rand::Rng;
            let adjectives = [
                "happy", "brave", "swift", "wise", "quiet", "bright", "calm", "eager", "gentle",
                "kind", "noble", "proud", "swift", "witty", "zesty",
            ];
            let nouns = [
                "node",
                "validator",
                "sentinel",
                "guardian",
                "keeper",
                "watcher",
                "beacon",
                "herald",
                "oracle",
                "pilot",
                "ranger",
                "scout",
            ];
            let mut rng = rand::thread_rng();
            let adj = adjectives[rng.gen_range(0..adjectives.len())];
            let noun = nouns[rng.gen_range(0..nouns.len())];
            let num = rng.gen_range(100..999);
            format!("{}-{}-{}", adj, noun, num)
        })
    }

    /// Get the P2P listen multiaddr.
    pub fn p2p_listen_addr(&self) -> eyre::Result<Multiaddr> {
        Ok(self.p2p_addr.clone())
    }

    /// Get persistent peers multiaddrs
    pub fn persistent_peers(&self) -> Vec<Multiaddr> {
        self.p2p_persistent_peers.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    const TEST_FEE_RECIPIENT: &str = "0xf97e180c050e5ab072211ad2c213eb5aee4df134";

    fn new_start_cmd() -> StartCmd {
        StartCmd {
            moniker: Some("test-node".to_string()),
            p2p_addr: "/ip4/127.0.0.1/tcp/27000".parse().unwrap(),
            ..Default::default()
        }
    }

    fn dummy_url() -> Url {
        Url::parse("http://localhost:8545").unwrap()
    }

    /// Assert that a file has secure permissions (0600 - read/write for owner only) on Unix systems
    #[cfg(unix)]
    fn assert_file_permissions_secure(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path).unwrap();
        let permissions = metadata.permissions();
        assert_eq!(
            permissions.mode() & 0o777,
            0o600,
            "File permissions should be 0600 (read/write for owner only)"
        );
    }

    #[test]
    fn validate_ok_with_no_conflicting_options() {
        let cmd = new_start_cmd();
        assert!(
            cmd.validate().is_ok(),
            "Command with valid options should be valid"
        );
    }

    #[test]
    fn validate_ok_with_only_ipc_options() {
        let mut cmd = new_start_cmd();
        cmd.eth_socket = Some("/tmp/reth.ipc".to_string());
        cmd.execution_socket = Some("/tmp/reth-auth.ipc".to_string());
        assert!(
            cmd.validate().is_ok(),
            "Should be valid with only IPC options"
        );
    }

    #[test]
    fn validate_ok_with_only_rpc_options() {
        let mut cmd = new_start_cmd();
        cmd.eth_rpc_endpoint = Some(dummy_url());
        cmd.execution_endpoint = Some(dummy_url());
        cmd.execution_jwt = Some("/path/to/jwt.hex".to_string());
        assert!(
            cmd.validate().is_ok(),
            "Should be valid with only RPC options"
        );
    }

    #[test]
    fn validate_err_when_mixing_ipc_and_rpc() {
        let mut cmd = new_start_cmd();
        cmd.eth_socket = Some("/tmp/reth.ipc".to_string());
        cmd.eth_rpc_endpoint = Some(dummy_url());

        let result = cmd.validate();
        assert!(result.is_err(), "Should fail when mixing IPC and RPC");

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Conflicting options detected"));
    }

    #[test]
    fn validate_err_with_another_mix_of_options() {
        let mut cmd = new_start_cmd();
        cmd.execution_socket = Some("/tmp/reth-auth.ipc".to_string());
        cmd.execution_jwt = Some("/path/to/jwt.hex".to_string());

        let result = cmd.validate();
        assert!(
            result.is_err(),
            "Should fail with another mix of IPC and RPC"
        );
    }

    #[test]
    fn private_key_file_uses_provided_path_if_it_exists() {
        let dir = tempdir().unwrap();
        let key_path = dir.path().join("priv_validator_key.json");
        File::create(&key_path).unwrap();

        let mut cmd = new_start_cmd();
        cmd.private_key = Some(key_path.clone());

        let default_path = PathBuf::from("/non/existent/path");

        let result = cmd.private_key_file(default_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), key_path);
    }

    #[test]
    fn private_key_file_errs_if_provided_path_does_not_exist() {
        let non_existent_path = PathBuf::from("/some/made/up/path/key.json");
        let mut cmd = new_start_cmd();
        cmd.private_key = Some(non_existent_path.clone());

        let result = cmd.private_key_file(PathBuf::from("/another/path"));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("private key file does not exist"));
        assert!(err_msg.contains(non_existent_path.to_str().unwrap()));
    }

    #[test]
    fn private_key_file_uses_default_path_if_it_exists_and_none_provided() {
        let dir = tempdir().unwrap();
        let default_key_path = dir.path().join("default_key.json");
        File::create(&default_key_path).unwrap();

        let cmd = new_start_cmd();

        let result = cmd.private_key_file(default_key_path.clone());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), default_key_path);
    }

    #[test]
    fn private_key_file_errs_if_default_path_does_not_exist_and_none_provided() {
        let non_existent_default = PathBuf::from("/another/made/up/path/default_key.json");
        let cmd = new_start_cmd();

        let result = cmd.private_key_file(non_existent_default.clone());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("default private key file does not exist"));
        assert!(err_msg.contains(non_existent_default.to_str().unwrap()));
    }

    #[test]
    fn private_key_file_auto_generates_when_remote_signing_and_default_missing() {
        let dir = tempdir().unwrap();
        let key_path = dir.path().join("priv_validator_key.json");

        let mut cmd = new_start_cmd();
        cmd.signing_remote = Some("http://remote-signer:10340".to_string());

        // Key doesn't exist yet
        assert!(!key_path.exists());

        // Should auto-generate the key
        let result = cmd.private_key_file(key_path.clone());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), key_path);

        // Key should now exist
        assert!(key_path.exists());

        // Verify the file contains valid JSON
        let contents = std::fs::read_to_string(&key_path).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&contents).is_ok());

        // Verify file permissions on Unix systems
        #[cfg(unix)]
        assert_file_permissions_secure(&key_path);
    }

    #[test]
    fn private_key_file_auto_generates_when_remote_signing_and_custom_path_missing() {
        let dir = tempdir().unwrap();
        let key_path = dir.path().join("custom_key.json");

        let mut cmd = new_start_cmd();
        cmd.signing_remote = Some("http://remote-signer:10340".to_string());
        cmd.private_key = Some(key_path.clone());

        // Key doesn't exist yet
        assert!(!key_path.exists());

        // Should auto-generate the key
        let result = cmd.private_key_file(PathBuf::from("/unused/default"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), key_path);

        // Key should now exist
        assert!(key_path.exists());

        // Verify the file contains valid JSON
        let contents = std::fs::read_to_string(&key_path).unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&contents).is_ok());

        // Verify file permissions on Unix systems
        #[cfg(unix)]
        assert_file_permissions_secure(&key_path);
    }

    #[test]
    fn private_key_file_does_not_overwrite_existing_key_with_remote_signing() {
        let dir = tempdir().unwrap();
        let key_path = dir.path().join("priv_validator_key.json");

        // Create a key file with known content
        let original_content = r#"{"test": "original"}"#;
        std::fs::write(&key_path, original_content).unwrap();

        let mut cmd = new_start_cmd();
        cmd.signing_remote = Some("http://remote-signer:10340".to_string());

        // Should use existing key
        let result = cmd.private_key_file(key_path.clone());
        assert!(result.is_ok());

        // Verify content hasn't changed
        let contents = std::fs::read_to_string(&key_path).unwrap();
        assert_eq!(contents, original_content);
    }

    #[test]
    fn clap_parses_persistent_peers() {
        let peer1 = "/ip4/172.19.0.21/tcp/27000";
        let peer2 = "/ip4/172.19.0.22/tcp/27000";
        let peers_str = format!("{},{}", peer1, peer2);
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--p2p.persistent-peers",
            &peers_str,
        ];

        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.p2p_persistent_peers.len(), 2);
        assert_eq!(cmd.p2p_persistent_peers[0], peer1.parse().unwrap());
        assert_eq!(cmd.p2p_persistent_peers[1], peer2.parse().unwrap());
    }

    #[test]
    fn clap_uses_default_values() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.pprof_addr, "0.0.0.0:6060");
        assert!(!cmd.pprof_heap_prof);
        assert_eq!(cmd.prune_certificates_distance, 0);
        assert_eq!(cmd.prune_certificates_before, 0);
        assert_eq!(cmd.discovery_num_outbound_peers, 20);
        assert_eq!(cmd.discovery_num_inbound_peers, 20);
        assert!(cmd.value_sync);
        assert!(!cmd.discovery);
        assert!(!cmd.validator);
    }

    #[test]
    fn pprof_heap_prof_when_set() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--pprof.heap-prof",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.pprof_heap_prof);
    }

    #[test]
    fn p2p_listen_addr_returns_multiaddr() {
        let mut cmd = new_start_cmd();
        cmd.p2p_addr = "/ip4/172.19.0.5/tcp/27000".parse().unwrap();

        let multiaddr = cmd.p2p_listen_addr().unwrap();
        assert_eq!(multiaddr.to_string(), "/ip4/172.19.0.5/tcp/27000");
    }

    #[test]
    fn p2p_addr_has_default_value() {
        let cmd = StartCmd::default();
        assert_eq!(cmd.p2p_addr.to_string(), "/ip4/0.0.0.0/tcp/27000");
    }

    #[test]
    fn get_moniker_returns_provided_moniker() {
        let mut cmd = new_start_cmd();
        cmd.moniker = Some("my-validator".to_string());

        assert_eq!(cmd.get_moniker(), "my-validator");
    }

    #[test]
    fn get_moniker_generates_random_when_not_provided() {
        let mut cmd = new_start_cmd();
        cmd.moniker = None;

        let moniker = cmd.get_moniker();
        // Check format: {adjective}-{noun}-{number}
        let parts: Vec<&str> = moniker.split('-').collect();
        assert_eq!(parts.len(), 3, "Generated moniker should have 3 parts");
        // Verify the last part is a number
        assert!(
            parts[2].parse::<u32>().is_ok(),
            "Last part should be a number"
        );
    }

    #[test]
    fn p2p_addr_supports_different_protocols() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/udp/27000/quic-v1",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.p2p_addr.to_string(), "/ip4/127.0.0.1/udp/27000/quic-v1");
    }

    #[test]
    fn p2p_addr_uses_default_and_moniker_is_optional() {
        let args = vec!["arc-node-consensus"];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.p2p_addr.to_string(), "/ip4/0.0.0.0/tcp/27000");
        assert!(cmd.moniker.is_none());
    }

    // Remote signing tests
    #[test]
    fn signing_remote_alone_sets_remote_signing() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--validator",
            "--suggested-fee-recipient",
            TEST_FEE_RECIPIENT,
            "--signing.remote",
            "http://signer:10340",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.signing_remote, Some("http://signer:10340".to_string()));
        assert_eq!(cmd.signing_tls_cert_path, None);
    }

    #[test]
    fn signing_remote_with_tls_cert_path() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--validator",
            "--suggested-fee-recipient",
            TEST_FEE_RECIPIENT,
            "--signing.remote",
            "http://signer:10340",
            "--signing.tls-cert-path",
            "/path/to/cert.pem",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.signing_remote, Some("http://signer:10340".to_string()));
        assert_eq!(
            cmd.signing_tls_cert_path,
            Some("/path/to/cert.pem".to_string())
        );
    }

    #[test]
    fn signing_remote_without_validator_fails() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--signing.remote",
            "http://signer:10340",
        ];
        assert!(StartCmd::try_parse_from(args).is_err());
    }

    #[test]
    fn signing_tls_cert_path_without_remote_fails() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--signing.tls-cert-path",
            "/path/to/cert.pem",
        ];
        let result = StartCmd::try_parse_from(args);
        assert!(result.is_err());
    }

    #[test]
    fn default_is_local_signing() {
        let cmd = new_start_cmd();
        assert_eq!(cmd.signing_remote, None);
        assert_eq!(cmd.signing_tls_cert_path, None);
    }

    // Discovery tests
    #[test]
    fn discovery_flag_enables_discovery() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--discovery",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.discovery);
    }

    #[test]
    fn discovery_num_outbound_peers_parsing() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--discovery",
            "--discovery.num-outbound-peers",
            "30",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.discovery_num_outbound_peers, 30);
    }

    #[test]
    fn discovery_num_inbound_peers_parsing() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--discovery",
            "--discovery.num-inbound-peers",
            "40",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.discovery_num_inbound_peers, 40);
    }

    #[test]
    fn discovery_defaults_to_20_peers() {
        let cmd = new_start_cmd();
        assert_eq!(cmd.discovery_num_outbound_peers, 20);
        assert_eq!(cmd.discovery_num_inbound_peers, 20);
    }

    // Metrics and RPC tests
    #[test]
    fn metrics_flag_with_valid_socket_address() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--metrics",
            "0.0.0.0:29000",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.metrics, Some("0.0.0.0:29000".parse().unwrap()));
    }

    #[test]
    fn rpc_addr_flag_with_valid_socket_address() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--rpc.addr",
            "0.0.0.0:31000",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.rpc_addr, Some("0.0.0.0:31000".parse().unwrap()));
    }

    #[test]
    fn metrics_and_rpc_are_optional() {
        let cmd = new_start_cmd();
        assert_eq!(cmd.metrics, None);
        assert_eq!(cmd.rpc_addr, None);
    }

    // Pruning tests
    #[test]
    fn prune_certificates_distance_parsing() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--prune.certificates.distance",
            "1000",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.prune_certificates_distance, 1000);
    }

    #[test]
    fn prune_certificates_before_parsing() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--prune.certificates.before",
            "500",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.prune_certificates_before, 500);
    }

    #[test]
    fn pruning_defaults_to_zero() {
        let cmd = new_start_cmd();
        assert!(!cmd.full);
        assert!(!cmd.minimal);
        assert_eq!(cmd.prune_certificates_distance, 0);
        assert_eq!(cmd.prune_certificates_before, 0);
    }

    #[test]
    fn full_preset_parsing() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--full",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.full);
        assert!(!cmd.minimal);
    }

    #[test]
    fn minimal_preset_parsing() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--minimal",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.minimal);
        assert!(!cmd.full);
    }

    #[test]
    fn full_and_minimal_conflict() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--full",
            "--minimal",
        ];
        assert!(StartCmd::try_parse_from(args).is_err());
    }

    #[test]
    fn full_conflicts_with_prune_certificates_distance() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--full",
            "--prune.certificates.distance",
            "1000",
        ];
        assert!(StartCmd::try_parse_from(args).is_err());
    }

    // P2P tests
    #[test]
    fn validate_err_when_persistent_peers_only_without_peers() {
        let mut cmd = new_start_cmd();
        cmd.p2p_persistent_peers_only = true;
        // p2p_persistent_peers is empty by default

        let result = cmd.validate();
        assert!(result.is_err(), "Should fail without persistent peers");

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("--p2p.persistent-peers-only requires"));
    }

    #[test]
    fn validate_ok_when_persistent_peers_only_with_peers() {
        let mut cmd = new_start_cmd();
        cmd.p2p_persistent_peers_only = true;
        cmd.p2p_persistent_peers = vec!["/ip4/172.19.0.21/tcp/27000".parse().unwrap()];

        assert!(cmd.validate().is_ok());
    }

    #[test]
    fn p2p_persistent_peers_only_flag_enables_mode() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--p2p.persistent-peers-only",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.p2p_persistent_peers_only);
    }

    #[test]
    fn p2p_persistent_peers_only_defaults_to_false() {
        let cmd = StartCmd::default();
        assert!(!cmd.p2p_persistent_peers_only);
    }

    #[test]
    fn p2p_persistent_peers_with_empty_list() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.p2p_persistent_peers.is_empty());
    }

    #[test]
    fn p2p_persistent_peers_with_multiple_peers() {
        let peer1 = "/ip4/172.19.0.21/tcp/27000";
        let peer2 = "/ip4/172.19.0.22/tcp/27000";
        let peer3 = "/ip4/172.19.0.23/tcp/27000";
        let peers_str = format!("{},{},{}", peer1, peer2, peer3);
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--p2p.persistent-peers",
            &peers_str,
        ];

        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert_eq!(cmd.p2p_persistent_peers.len(), 3);
        assert_eq!(cmd.p2p_persistent_peers[0], peer1.parse().unwrap());
        assert_eq!(cmd.p2p_persistent_peers[1], peer2.parse().unwrap());
        assert_eq!(cmd.p2p_persistent_peers[2], peer3.parse().unwrap());
    }

    #[test]
    fn value_sync_default_is_true() {
        let cmd = new_start_cmd();
        assert!(cmd.value_sync);
    }

    // GossipSub tests
    #[test]
    fn gossipsub_explicit_peering_defaults_to_false() {
        let cmd = StartCmd::default();
        assert!(!cmd.gossipsub_explicit_peering);
    }

    #[test]
    fn gossipsub_mesh_prioritization_defaults_to_false() {
        let cmd = StartCmd::default();
        assert!(!cmd.gossipsub_mesh_prioritization);
    }

    #[test]
    fn gossipsub_load_defaults_to_none() {
        let cmd = StartCmd::default();
        assert!(cmd.gossipsub_load.is_none());
    }

    #[test]
    fn gossipsub_explicit_peering_flag() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--gossipsub.explicit-peering",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.gossipsub_explicit_peering);
    }

    #[test]
    fn gossipsub_mesh_prioritization_flag() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--gossipsub.mesh-prioritization",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.gossipsub_mesh_prioritization);
    }

    #[test]
    fn gossipsub_load_profile_parsing() {
        for profile in &["low", "average", "high"] {
            let args = vec![
                "arc-node-consensus",
                "--moniker",
                "test",
                "--p2p.addr",
                "/ip4/127.0.0.1/tcp/27000",
                "--gossipsub.load",
                profile,
            ];
            let cmd = StartCmd::try_parse_from(args).unwrap();
            assert_eq!(cmd.gossipsub_load.as_deref(), Some(*profile));
        }
    }

    #[test]
    fn gossipsub_load_rejects_invalid_profile() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--gossipsub.load",
            "ultra",
        ];
        assert!(StartCmd::try_parse_from(args).is_err());
    }

    #[test]
    fn gossipsub_all_flags_combined() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--gossipsub.explicit-peering",
            "--gossipsub.mesh-prioritization",
            "--gossipsub.load",
            "high",
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.gossipsub_explicit_peering);
        assert!(cmd.gossipsub_mesh_prioritization);
        assert_eq!(cmd.gossipsub_load.as_deref(), Some("high"));
    }

    // to_cli_flags tests
    #[test]
    fn to_cli_flags_empty_for_defaults() {
        assert!(StartCmd::default().to_cli_flags().is_empty());
    }

    #[test]
    fn to_cli_flags_round_trip_preserves_all_fields() {
        let original = StartCmd {
            moniker: Some("validator-01".to_string()),
            p2p_addr: "/ip4/10.0.0.1/tcp/27001".parse().unwrap(),
            p2p_persistent_peers: vec![
                "/ip4/10.0.0.2/tcp/27000".parse().unwrap(),
                "/ip4/10.0.0.3/tcp/27000".parse().unwrap(),
            ],
            p2p_persistent_peers_only: true,
            gossipsub_explicit_peering: true,
            gossipsub_mesh_prioritization: true,
            gossipsub_load: Some("high".to_string()),
            log_level: None,
            log_format: None,
            discovery: true,
            discovery_num_outbound_peers: 30,
            discovery_num_inbound_peers: 40,
            no_consensus: true,
            validator: false,
            value_sync: false,
            eth_socket: Some("/tmp/reth.ipc".to_string()),
            execution_socket: Some("/tmp/reth-auth.ipc".to_string()),
            eth_rpc_endpoint: None,
            execution_endpoint: None,
            execution_ws_endpoint: None,
            execution_persistence_backpressure: true,
            execution_persistence_backpressure_threshold: 32,
            execution_jwt: None,
            metrics: Some("127.0.0.1:9000".parse().unwrap()),
            rpc_addr: Some("127.0.0.1:31000".parse().unwrap()),
            runtime_flavor: RUNTIME_SINGLE_THREADED.to_string(),
            worker_threads: Some(8),
            full: false,
            minimal: false,
            prune_certificates_distance: 1000,
            prune_certificates_before: 0,
            private_key: Some(PathBuf::from("/etc/arc/key.json")),
            pprof_addr: "127.0.0.1:7070".to_string(),
            pprof_heap_prof: true,
            suggested_fee_recipient: Some(
                "0x0000000000000000000000000000000000000042"
                    .parse()
                    .unwrap(),
            ),
            skip_db_upgrade: true,
            signing_remote: Some("http://signer:10340".to_string()),
            signing_tls_cert_path: Some("/etc/arc/signer.pem".to_string()),
            follow: true,
            follow_endpoints: vec!["http://rpc-1:8545,ws=8546".parse().unwrap()],
        };

        let args = std::iter::once("arc-node-consensus".to_string())
            .chain(original.to_cli_flags())
            .collect::<Vec<_>>();
        let parsed = StartCmd::try_parse_from(args).expect("emitted flags should parse");

        assert_eq!(parsed, original);
    }

    // Validator flag tests
    #[test]
    fn validator_defaults_to_false() {
        let cmd = StartCmd::default();
        assert!(!cmd.validator);
    }

    #[test]
    fn validator_flag_parsing() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--validator",
            "--suggested-fee-recipient",
            TEST_FEE_RECIPIENT,
        ];
        let cmd = StartCmd::try_parse_from(args).unwrap();
        assert!(cmd.validator);
    }

    #[test]
    fn validator_requires_suggested_fee_recipient() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--validator",
        ];
        let err = StartCmd::try_parse_from(args).unwrap_err().to_string();
        assert!(
            err.contains("--suggested-fee-recipient"),
            "expected error to mention --suggested-fee-recipient, got: {err}"
        );
    }

    #[test]
    fn suggested_fee_recipient_rejects_zero_address() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--validator",
            "--suggested-fee-recipient",
            "0x0000000000000000000000000000000000000000",
        ];
        let err = StartCmd::try_parse_from(args).unwrap_err().to_string();
        assert!(
            err.contains("zero address"),
            "expected zero-address rejection, got: {err}"
        );
    }

    #[test]
    fn validator_conflicts_with_no_consensus() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--validator",
            "--suggested-fee-recipient",
            TEST_FEE_RECIPIENT,
            "--no-consensus",
        ];
        assert!(StartCmd::try_parse_from(args).is_err());
    }

    #[test]
    fn validator_conflicts_with_follow() {
        let args = vec![
            "arc-node-consensus",
            "--moniker",
            "test",
            "--p2p.addr",
            "/ip4/127.0.0.1/tcp/27000",
            "--validator",
            "--suggested-fee-recipient",
            TEST_FEE_RECIPIENT,
            "--follow",
            "--follow.endpoint",
            "http://localhost:8545",
        ];
        assert!(StartCmd::try_parse_from(args).is_err());
    }
}
