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

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::unwrap_used
)]

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use clap::{Args, Parser, Subcommand};
use clap_verbosity_flag::{InfoLevel, Verbosity};
use color_eyre::eyre::{self, bail, Context, Result};
use spammer::SpammerArgs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use crate::infra::export;
use crate::infra::{BuildProfile, INFRA_DATA_FILENAME};
use crate::manifest::{generate_manifests, EngineApiConnection};
use crate::perturb::{PERTURB_MAX_TIME_OFF, PERTURB_MIN_TIME_OFF};
use crate::valset::ValidatorPowerUpdate;

use perturb::Perturbation;
use testnet::{Testnet, TestnetError};

mod build;
mod clean;
mod cli_version;
mod genesis;
mod info;
mod infra;
mod latency;
mod load;
mod manifest;
mod mcp;
mod mesh;
mod metrics;
mod monitor;
mod node;
mod nodekey;
mod nodes;
mod perturb;
mod report;
mod rpc;
mod rpc_cmd;
mod setup;
mod shell;
mod testnet;
mod tests;
mod util;
mod valset;
mod wait;
mod web;

const DEFAULT_NUM_EXTRA_PREFUNDED_ACCOUNTS: usize = 100;

#[derive(Parser)]
#[command(
    name = "quake",
    version = arc_version::SHORT_VERSION,
    long_version = arc_version::LONG_VERSION,
    about = "Testnet management and end-to-end testing tool"
)]
struct Cli {
    /// Path to the manifest TOML file
    #[arg(short = 'f', long = "file", value_name = "MANIFEST_TOML")]
    manifest_file: Option<PathBuf>,

    #[command(flatten)]
    verbosity: Verbosity<InfoLevel>,

    /// Seed for deterministic execution
    #[arg(long)]
    seed: Option<u64>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate from manifest all required files to run the testnet
    Setup {
        #[command(flatten)]
        args: SetupArgs,
    },
    /// Build the testnet Docker images
    Build {
        #[command(flatten)]
        args: BuildArgs,
    },
    /// Start the testnet or a subset of CL and/or EL containers.
    ///
    /// If no list of node or container names is provided, start the testnet following the starting heights in the manifest.
    /// Otherwise, start only the given nodes and containers immediately.
    ///
    /// A node name will expand to both the CL and EL containers of the node.
    /// Wildcard '*' is supported; e.g. 'val*_cl' will match all consensus layer containers of all validators.
    #[command(verbatim_doc_comment)]
    Start {
        #[command(flatten)]
        start_args: StartArgs,
    },
    /// Stop the testnet or a subset of CL and/or EL containers.
    ///
    /// If no list of node or container names is provided, stop all CL and EL containers.
    /// Otherwise, stop only the given CL and EL containers.
    /// Note that monitoring services are not stopped by this command.
    ///
    /// A node name will expand to both the CL and EL containers of the node.
    /// Wildcard '*' is supported; e.g. 'val*_cl' will match all consensus layer containers of all validators.
    #[command(verbatim_doc_comment)]
    Stop {
        /// Names of the nodes or containers to stop (all nodes if not specified)
        nodes_or_containers: Vec<String>,
    },
    /// Stop all nodes and remove testnet-related files (including databases).
    ///
    /// Monitoring data is not removed by default.
    #[command(verbatim_doc_comment)]
    Clean {
        #[command(flatten)]
        clean_args: CleanArgs,
    },
    /// Clean and start the testnet.
    Restart {
        #[command(flatten)]
        clean_args: CleanArgs,
        #[command(flatten)]
        start_args: StartArgs,
    },
    /// Apply a perturbation (disconnect, kill, pause, or restart) to nodes and/or containers.
    ///
    /// A node is composed of two containers: '<node_name>_cl' and '<node_name>_el'.
    ///
    /// Wildcard '*' is supported; e.g. 'val*_cl' will match all consensus layer containers of all validators.
    #[command(verbatim_doc_comment)]
    Perturb {
        #[command(subcommand)]
        action: Perturbation,
        /// Minimum time the targets will be offline before recovering from the last perturbation
        #[arg(short = 't', long, value_parser = parse_duration, default_value = PERTURB_MIN_TIME_OFF)]
        min_time_off: Duration,
        /// Maximum time the targets will be offline before recovering from the last perturbation
        #[arg(short = 'T', long, value_parser = parse_duration, default_value = PERTURB_MAX_TIME_OFF)]
        max_time_off: Duration,
    },
    /// Output logs of all containers or a specific container
    Logs {
        /// Names of the nodes or containers to show logs for (all containers if not specified)
        names: Vec<String>,
        /// Follow the logs output
        #[clap(short = 'f', long, default_value = "false")]
        follow: bool,
    },
    /// Show the state of the testnet and metadata
    Info {
        #[command(subcommand)]
        command: Option<InfoSubcommand>,
    },
    /// Deploy and manage the testnet in remote infrastructure
    Remote {
        #[command(subcommand)]
        command: RemoteSubcommand,
    },
    /// Manage monitoring services (Prometheus, Grafana, Blockscout)
    Monitoring {
        #[command(subcommand)]
        command: MonitoringSubcommand,
    },
    /// Download monitoring data (metrics and/or node databases).
    ///
    /// Without a subcommand, downloads both metrics and a single-node database
    /// snapshot using defaults. Works in both local and remote mode.
    Download {
        #[command(subcommand)]
        command: Option<DownloadKindSubcommand>,
    },
    /// Send transaction load to the testnet (backpressure mode: waits for each
    /// response and only advances the nonce on success).
    /// Use --mix to blend transaction types (e.g., --mix transfer=70,erc20=30).
    ///
    /// If `--targets` is omitted, all manifest nodes are used. Each target may
    /// be an exact node name or a manifest node group such as `ALL_VALIDATORS`.
    #[command(
        verbatim_doc_comment,
        after_long_help = "Examples:\n  quake load --rate 200 --time 60\n  quake load --targets validator1,ALL_VALIDATORS --rate 200 --time 60\n"
    )]
    Load {
        #[arg(long, value_delimiter = ',')]
        targets: Option<Vec<String>>,
        #[command(flatten)]
        args: SpammerArgs,
    },
    /// Send transaction load to the testnet (fire-and-forget mode: pushes
    /// transactions into a buffer and sends without waiting for responses).
    /// Use --mix to blend transaction types (e.g., --mix transfer=70,erc20=30).
    ///
    /// If `--targets` is omitted, all manifest nodes are used. Each target may
    /// be an exact node name or a manifest node group such as `ALL_VALIDATORS`.
    #[command(
        verbatim_doc_comment,
        after_long_help = "Examples:\n  quake spam --rate 200 --time 60\n  quake spam --targets validator1,ALL_VALIDATORS --rate 200 --time 60\n"
    )]
    Spam {
        #[arg(long, value_delimiter = ',')]
        targets: Option<Vec<String>>,
        #[command(flatten)]
        args: SpammerArgs,
    },
    /// Modify the voting power of the testnet's validators.
    #[clap(name = "valset")]
    ValSet {
        /// List of VALIDATOR:VOTING_POWER pairs, e.g. `validator1:123
        /// validator2:456`
        #[arg(
            value_name = "VALIDATOR:VOTING_POWER",
            num_args = 1..,
        )]
        updates: Vec<ValidatorPowerUpdate>,
    },
    /// Wait for nodes to reach a height or finish syncing
    Wait {
        #[clap(subcommand)]
        command: WaitSubcommand,
    },
    /// Send a JSON-RPC call (EL) or REST call (CL) to one or more nodes.
    ///
    /// `el` targets Reth's JSON-RPC; `cl` targets Malachite's REST API.
    /// `list` shows the live CL endpoint catalog plus a link to the Reth docs.
    ///
    /// Examples:
    ///   quake rpc list
    ///   quake rpc el admin_clearTxpool
    ///   quake rpc el eth_blockNumber ALL_VALIDATORS
    ///   quake rpc el eth_getBalance validator1 0xabc... latest
    ///   quake rpc cl /status
    ///   quake rpc cl /commit ALL_VALIDATORS
    #[command(verbatim_doc_comment)]
    Rpc {
        #[command(subcommand)]
        command: RpcSubcommand,
    },
    /// Run tests against the testnet (or list with --dry-run)
    ///
    /// Supports glob patterns (* and ?) for matching groups and tests.
    /// IMPORTANT: Quote patterns to prevent shell expansion, e.g., 'n*:*peer*'
    ///
    /// Examples:
    ///   quake test                       - Run all tests (except excluded groups: validation, health, validator_set, perf)
    ///   quake test probe                 - Run all tests in probe group
    ///   quake test 'n*'                  - Run tests in groups starting with n
    ///   quake test 'n*:*peer*'           - Run tests containing 'peer' in groups starting with n
    ///   quake test probe:connectivity    - Run specific test
    ///   quake test --dry-run             - List all tests without running
    ///   quake test probe --dry-run       - List tests in probe group
    #[command(verbatim_doc_comment)]
    Test {
        /// Test specification: empty for all, 'group' for group tests, 'group:test1,test2' for specific tests.
        /// Supports glob patterns (quote to prevent shell expansion): 'n*:*peer*'
        #[clap(default_value = "")]
        spec: String,
        /// List tests that would run without executing them
        #[clap(long, default_value = "false")]
        dry_run: bool,
        /// RPC timeout for test requests
        #[clap(long, default_value = "1s", value_parser = parse_duration)]
        rpc_timeout: Duration,
        /// Pass parameters to tests as key=value pairs (e.g. --set arc_node=full1)
        #[clap(long = "set", value_parser = parse_key_value)]
        params: Vec<(String, String)>,
    },
    /// Generate random manifests
    ///
    /// This command generates multiple random manifests with different seeds.
    /// The manifests are saved to the specified output directory.
    ///
    /// By default, generates `count` manifests for 1 single node scenario and EACH combination of:
    ///   - Network topology: 5 nodes | complex topology
    ///   - Height start: all nodes at 0 | some nodes start at 100
    ///   - Region assignment: no regions | uniform random | clustered
    ///
    /// Note: Complex topology with region strategy other than single region is not supported on local infrastructure.
    ///
    /// Note: The complex topology is constructed as follows:
    ///   - Two sentry groups (1–4 validators each, fully meshed behind sentry-1/sentry-2),
    ///   - A relayer connected to both sentries and to 1–2 full nodes (themselves fully meshed).
    ///   - All nodes use persistent peer connections.
    ///
    /// Example:
    ///   quake generate --output-dir manifests --count 10
    ///
    /// This will generate 10 manifests per each supported combination.
    ///
    /// If --seed is provided, it will be used as the base seed, with subsequent files using incremental seeds.
    #[clap(visible_alias = "gen")]
    #[command(verbatim_doc_comment)]
    Generate {
        /// Output directory for generated manifests
        #[arg(short = 'o', long, default_value = ".quake/generated")]
        output_dir: PathBuf,
        /// Number of manifest files to generate per combination
        #[arg(short = 'c', long, default_value_t = 1)]
        count: usize,
    },
    /// Generate a network testing report (mesh, health, perf, sanity, sync).
    ///
    /// Collects metrics from a running testnet, optionally runs sanity phases
    /// and sync-speed tests, then writes a structured markdown report.
    ///
    /// Parameters (via `--set key=value`):
    ///
    ///   Key                Default              Description
    ///   ─────────────────  ───────────────────  ─────────────────────────────────────
    ///   warmup_s           30                   Seconds before first Prometheus scrape
    ///   duration_s         60                   Observation window / load duration
    ///   load_rate          50                   TPS during observation (0 = no load)
    ///   load_targets       RPC_NODES            Node names and/or [node_groups] selectors (default group)
    ///   load_mix           transfer=100         Tx type mix
    ///   block_time_p50_ms  550                  Max p50 block time threshold for validators
    ///   block_time_p99_ms  1000                 Max p99 block time threshold for validators
    ///   sanity             true                 Run sanity phases
    ///   sync_speed         true                 Run sync speed test (destructive)
    ///   arc_nodes          ARC_NODES group      Sanity target nodes (names and/or [node_groups])
    ///   snapshot_provider  full-circle-5        Snapshot source node
    ///   reference          validator-blue       Reference node for tip height
    ///   sync_nodes         full-quicknode-1     Nodes to sync-test
    ///   sync_min_bps       7.0                  Min avg blocks/sec to pass
    ///   sync_timeout_s     180                  Max sync measurement duration
    ///   sync_downtime_s    120                  Seconds to keep node down
    ///   store_nodes        (pruned nodes)       Nodes for storage size lookup
    #[command(verbatim_doc_comment)]
    Report {
        /// Output file path for the markdown report
        #[arg(short = 'o', long, default_value = "/tmp/quake-report.md")]
        output: PathBuf,
        /// Pass parameters as key=value pairs (e.g. --set sanity=false)
        #[clap(long = "set", value_parser = parse_key_value)]
        params: Vec<(String, String)>,
    },
    /// Start a web server to visualize the testnet network topology (local mode only)
    Web {
        /// Host/IP address to bind the web server to
        #[clap(long, default_value = "127.0.0.1")]
        host: String,
        /// Port for the web server
        #[clap(long, default_value = "7777")]
        port: u16,
        /// Frontend topology poll interval in milliseconds
        ///
        /// A WebSocket subscriber polls new block headers to update block
        /// heights in real-time as blocks arrive. When the chain stalls (no new
        /// blocks being produced), the WS stream goes silent, and no data
        /// flows. For that we have the EL peer refresh poller.
        ///
        /// CL data (/network-state and /status) is fetched inline during each
        /// topology refresh request.
        #[clap(long, default_value = "1000")]
        refresh_ms: u64,
        /// EL peer refresh poller interval in milliseconds
        ///
        /// Polls admin_peers to update EL peer connection data (who is
        /// connected, trusted, inbound, static). This runs independently of
        /// block production, so peer data stays fresh even during chain stalls.
        #[clap(long, default_value = "1000")]
        el_refresh_ms: u64,
        /// Docker container status poller interval in milliseconds
        #[clap(long, default_value = "1000")]
        container_refresh_ms: u64,
    },
    /// Start an MCP (Model Context Protocol) server for AI-assisted testnet management.
    ///
    /// By default uses stdio transport (for Claude Code, Cursor, etc.).
    /// Use --http to start an HTTP+SSE server for remote clients.
    Mcp {
        /// Use HTTP+SSE transport instead of stdio
        #[clap(long, default_value = "false")]
        http: bool,
        /// Port for HTTP+SSE server (only used with --http)
        #[clap(long, default_value = "8080")]
        port: u16,
    },
}

#[derive(Args)]
struct SetupArgs {
    /// Force the creation of the testnet files even if they already exist
    #[clap(long, default_value = "false")]
    force: bool,
    /// Use auth RPC connection between Consensus Layer (CL) and Execution Layer (EL) instead of IPC
    #[clap(long, default_value = "false")]
    rpc: bool,
    /// Number of extra pre-funded accounts to generate in the genesis file (for sending transaction load)
    #[clap(short = 'e', long, default_value_t = DEFAULT_NUM_EXTRA_PREFUNDED_ACCOUNTS)]
    num_extra_accounts: usize,
    /// Initial balance for each prefunded account in whole token units (overrides manifest value, default 1_000_000)
    #[clap(long, value_parser = clap::value_parser!(u64).range(1..))]
    extra_account_balance: Option<u64>,
    /// ProtocolConfig blockGasLimit and genesis header gas limit (overrides manifest value, default 30_000_000)
    #[clap(long, value_parser = clap::value_parser!(u64).range(1..))]
    block_gas_limit: Option<u64>,
}

#[derive(Args)]
struct BuildArgs {
    /// Build artifacts with the specified profile
    #[clap(short = 'p', long, default_value_t = BuildProfile::default())]
    profile: BuildProfile,
}

#[derive(Args)]
struct StartArgs {
    /// Names of the nodes or containers to start (all nodes if not specified)
    nodes_or_containers: Vec<String>,
    /// Create the testnet in remote infrastructure and start it immediately (no confirmation asked)
    #[clap(long, default_value = "false")]
    remote: bool,
    /// Whether to start monitoring services (Prometheus, Grafana, cAdvisor, Blockscout)
    #[clap(short = 'm', long, num_args = 0..=1, default_value_t = true, default_missing_value = "true")]
    monitoring: bool,
    #[command(flatten)]
    setup_args: SetupArgs,
    #[command(flatten)]
    build_args: BuildArgs,
    #[command(flatten)]
    infra_args: InfraArgs,
}

/// EC2 instance type and optional root EBS size for remote infrastructure.
///
/// See README "Instance sizing" for details. Disk flags apply only when using
/// `quake remote create` or `quake start --remote` (they are ignored for local testnets).
#[derive(Args, Debug, Clone)]
pub(crate) struct InfraArgs {
    /// EC2 instance type for nodes [default: t3.medium].
    ///
    /// The default t3.medium (2 vCPU, 4 GiB RAM, 20 GiB disk) supports testnets
    /// running for up to ~20 hours. Debug-level logs grow at ~200 MiB/hr and
    /// the execution layer uses ~2.5 GiB RAM, leaving little headroom.
    ///
    /// Recommended sizes:
    ///   t3.medium  — short tests (< 12h), no load or light load
    ///   t3.large   — day-long runs (8 GiB RAM, fits EL + CL with headroom)
    ///   t3.xlarge  — multi-day or heavy-load testnets (16 GiB RAM)
    #[clap(long, verbatim_doc_comment)]
    node_size: Option<String>,
    /// EC2 instance type for the Control Center [default: t3.xlarge].
    ///
    /// CC runs Prometheus, Grafana, Blockscout, RPC proxy, and spammer containers.
    /// t3.large (8 GiB) is insufficient; t3.xlarge (16 GiB) is the minimum.
    ///
    /// Recommended sizes:
    ///   t3.xlarge  — standard monitoring stack (16 GiB RAM)
    ///   t3.2xlarge — heavy Blockscout indexing or many nodes (32 GiB RAM)
    #[clap(long, verbatim_doc_comment)]
    cc_size: Option<String>,
    /// Root EBS volume size for nodes (GiB). Omit to use the AMI default.
    ///
    /// Long runs need more than the default root volume when debug logs fill the disk;
    /// pair with `--node-size` for RAM headroom. When set, must be at least **8** (typical
    /// lower bound vs AMI snapshot size; AWS may still require a larger minimum for a given AMI).
    #[clap(long, value_name = "GIB", value_parser = clap::value_parser!(u32).range(crate::manifest::MIN_DISK_GB as i64..))]
    node_disk_gb: Option<u32>,
    /// Root EBS volume size for the Control Center (GiB). Omit to use the AMI default.
    ///
    /// When set, must be at least **8** (see `--node-disk-gb`).
    #[clap(long, value_name = "GIB", value_parser = clap::value_parser!(u32).range(crate::manifest::MIN_DISK_GB as i64..))]
    cc_disk_gb: Option<u32>,
    /// Root EBS volume type for nodes (e.g. `gp3`, `io2`). Omit to use the Terraform default (`gp3`).
    ///
    /// Pair with `--node-volume-iops` for `gp3`/`io1`/`io2`. Validated against AWS-supported types.
    #[clap(long, value_name = "TYPE")]
    node_volume_type: Option<String>,
    /// Provisioned IOPS for the node root EBS volume.
    ///
    /// Only meaningful for `gp3`, `io1`, and `io2` volumes. AWS will reject finer-grained
    /// violations (per-type maxima, volume-size ratio) at apply time.
    #[clap(long, value_name = "IOPS", value_parser = clap::value_parser!(u32).range(100..=256_000))]
    node_volume_iops: Option<u32>,
    /// Place the node data directory on the local instance-store NVMe instead of the root EBS volume.
    ///
    /// Requires an instance type that ships local NVMe instance storage (e.g. `i4i.*`, `i3.*`,
    /// `m6id.*`); pair with `--node-size`. On instance types without instance store this is a
    /// no-op and the data directory stays on the root EBS volume. Independent of
    /// `--node-volume-type`/`--node-volume-iops`, which keep configuring the root EBS volume.
    #[clap(long)]
    node_data_on_instance_store: bool,
}

#[derive(Args)]
struct CleanArgs {
    /// Remove all data, including the testnet directory and monitoring services
    #[clap(short = 'a', long, default_value = "false")]
    #[clap(conflicts_with_all = ["data", "execution_data", "consensus_data"])]
    all: bool,
    /// Remove only execution and consensus layer data, preserving configuration
    #[clap(short = 'd', long, default_value = "false")]
    #[clap(conflicts_with_all = ["execution_data", "consensus_data"])]
    data: bool,
    /// Remove only execution layer data, preserving configuration
    #[clap(short = 'x', long, default_value = "false")]
    execution_data: bool,
    /// Remove only consensus layer data, preserving configuration
    #[clap(short = 'c', long, default_value = "false")]
    consensus_data: bool,
}

impl CleanArgs {
    fn scope(&self) -> clean::Scope {
        clean::Scope::from_cli_flags(
            self.all,
            self.data,
            self.execution_data,
            self.consensus_data,
        )
    }
}

#[derive(Subcommand)]
pub(crate) enum RpcSubcommand {
    /// List available endpoints: CL live catalog + EL docs link.
    List,
    /// Send a JSON-RPC request to the Execution Layer (Reth).
    ///
    /// Examples:
    ///   quake rpc el admin_clearTxpool
    ///   quake rpc el eth_blockNumber ALL_VALIDATORS
    ///   quake rpc el eth_getBalance validator1 0xabc... latest
    ///   quake rpc el eth_call validator1 --raw '[{"to":"0x..."},"latest"]'
    #[command(verbatim_doc_comment)]
    El(rpc_cmd::ElArgs),
    /// Send a REST request to the Consensus Layer (Malachite).
    ///
    /// The leading `/` on the path is optional.
    ///
    /// Examples:
    ///   quake rpc cl /status
    ///   quake rpc cl consensus-state
    ///   quake rpc cl /commit ALL_VALIDATORS
    ///   quake rpc cl '/commit?height=42' validator1
    ///   quake rpc cl /persistent-peers validator1 --method POST --body '{"addr":"/ip4/..."}'
    #[command(verbatim_doc_comment)]
    Cl(rpc_cmd::ClArgs),
}

#[derive(Debug, Subcommand, PartialEq)]
pub(crate) enum WaitSubcommand {
    /// Wait for nodes to reach a specific block height
    Height {
        /// Height to wait for
        height: u64,
        /// Names of the nodes to wait for (all nodes if not specified)
        nodes: Vec<String>,
        /// Timeout in seconds
        #[clap(short, long, default_value = "30")]
        timeout: u64,
    },
    /// Wait for nodes to finish syncing (eth_syncing returns false)
    Sync {
        /// Names of the nodes to wait for (all nodes if not specified)
        nodes: Vec<String>,
        /// Timeout in seconds
        #[clap(short, long, default_value = "180")]
        timeout: u64,
        /// Maximum number of retries for failed RPC calls (for node restarts)
        #[clap(long, default_value = "3")]
        max_retries: u32,
    },
    /// Wait for consensus rounds to settle at 0
    Rounds {
        /// Number of consecutive round-0 blocks required
        #[clap(long, default_value = "10")]
        consecutive: u64,
        /// Timeout in seconds
        #[clap(short, long, default_value = "120")]
        timeout: u64,
    },
}

#[derive(Debug, Subcommand, PartialEq)]
pub(crate) enum InfoSubcommand {
    /// Show the latest block height of a single node
    Height {
        /// Name of the node to query
        node: String,
    },
    /// Show the latest heights of each node
    Heights {
        /// Number of rounds to print before exiting (0 for infinite)
        #[clap(short = 'n', long, default_value = "0")]
        number: u32,
    },
    /// Show number of pending and queued transactions in the mempool of each node
    #[clap(alias = "pools")]
    Mempool,
    /// Show detailed information about the peers of each node
    Peers {
        /// Show all information about the peers
        #[clap(short = 'a', long, default_value = "false")]
        all: bool,
    },
    /// Show gossipsub mesh status, connectivity, and partition analysis
    Mesh {
        /// Show only mesh topology analysis (no status table)
        #[clap(long, default_value = "false")]
        mesh_only: bool,
        /// Show detailed peer information for each node
        #[clap(long, default_value = "false")]
        peers: bool,
        /// Show full peer detail including peer types and scores
        #[clap(long, default_value = "false")]
        peers_full: bool,
        /// Show duplicate message rates
        #[clap(long, default_value = "false")]
        duplicates: bool,
    },
    /// Show performance metrics: block latency and throughput
    Perf {
        /// Show only latency metrics (block time, finalize, build, consensus)
        #[clap(long, default_value = "false")]
        latency_only: bool,
        /// Show only throughput metrics (txs/block, block size, gas/block)
        #[clap(long, default_value = "false")]
        throughput_only: bool,
        /// Use two scrapes and show histogram deltas for the observation window only
        #[clap(long, default_value = "false")]
        interval: bool,
        /// Seconds to wait before the first scrape (interval mode only)
        #[clap(long, default_value = "30")]
        warmup_seconds: u64,
        /// Seconds between first and second scrape (interval mode only)
        #[clap(long, default_value = "60")]
        observation_seconds: u64,
    },
    /// Show Malachite CL store.db table statistics (record counts, height ranges)
    Store {
        /// Names of the nodes to inspect (all nodes if not specified)
        nodes: Vec<String>,
    },
    /// Show consensus health: round breakdown, height restarts, sync-fell-behind counts
    Health,
    /// Measure sync speed: wait for a node to start, then track blocks/s until it
    /// catches up with validator1
    SyncSpeed {
        /// Name of the node to measure
        node: String,
        /// Reference node to sync against (default: validator1)
        #[clap(long, default_value = "validator1")]
        reference: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RemoteSubcommand {
    /// Initialize Terraform state (run only once)
    Preinit,
    /// Create nodes and a Control Server in the remote infrastructure
    Create {
        /// Dry run the command
        #[clap(short, long, default_value = "false")]
        dry_run: bool,
        /// Set to _not_ ask for confirmation
        #[clap(long, default_value = "false")]
        yes: bool,
        #[command(flatten)]
        infra_args: InfraArgs,
    },
    /// Show the status of the infrastructure
    Status,
    /// Monitor health of all nodes and the Control Center
    Monitor {
        /// Node name, 'cc' for Control Center only, or 'all' for everything
        #[clap(default_value = "all")]
        node_or_cc: String,
        /// Continuously refresh data
        #[clap(short, long)]
        follow: bool,
        /// Refresh interval in seconds (default: 5 for single host, 30 for all)
        #[clap(short, long)]
        interval: Option<u64>,
    },
    /// Upload testnet files to the Control Center server
    ///
    /// Nodes will access their configuration files via NFS.
    #[command(verbatim_doc_comment)]
    Provision,
    /// Manage SSM sessions, required for RPC and SSH access to nodes and monitoring services
    Ssm {
        #[command(subcommand)]
        command: SSMSubcommand,
    },
    /// Destroy the created infrastructure
    Destroy {
        /// Set to ask for confirmation
        #[clap(long, default_value = "true")]
        yes: bool,
    },
    /// SSH to a remote node or the Control Center (CC) server
    Ssh {
        /// Node name or 'cc' for the Control Center server
        node_or_cc: String,
        /// Command to run on the node or CC server; if not provided, will open an interactive shell
        command: Vec<String>,
    },
    /// [Deprecated: use `quake load` directly — it now works for both local and remote testnets]
    ///
    /// Send transaction load to the nodes by running `quake load` from the Control
    /// Center (backpressure mode).
    #[command(verbatim_doc_comment)]
    Load { args: Vec<String> },
    /// [Deprecated: use `quake spam` directly — it now works for both local and remote testnets]
    ///
    /// Send transaction load to the nodes by running `quake spam` from the Control Center
    /// (fire-and-forget mode).
    #[command(verbatim_doc_comment)]
    Spam { args: Vec<String> },
    /// Export a JSON file with everything needed for another user to access this remote testnet
    Export {
        /// Path to the output file
        path: Option<PathBuf>,
        /// Exclude Terraform state from the bundle (recipients won't be able to run terraform destroy)
        #[clap(long, default_value = "false")]
        exclude_terraform: bool,
    },
    /// Import an exported JSON file to set up local quake state for an existing remote testnet
    Import {
        /// Path to the JSON file created by `quake remote export`
        path: PathBuf,
    },
    /// [DEPRECATED] Download metrics or database data from remote infrastructure.
    ///
    /// Use `quake download {metrics,db}` instead — it works in both
    /// local and remote mode.
    Download {
        #[clap(subcommand)]
        command: DownloadSubcommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum MonitoringSubcommand {
    /// Start monitoring services
    Start,
    /// Stop monitoring services
    Stop,
    /// Stop monitoring services and clean monitoring data
    Clean,
}

#[derive(Debug, Subcommand)]
pub(crate) enum DownloadKindSubcommand {
    /// Download a Prometheus metrics snapshot for the testnet.
    ///
    /// Queries the `query_range` API directly over HTTP (local Docker port for
    /// local testnets, SSM-tunnelled port for remote) and bundles the JSON
    /// responses into a single `.tar.gz` under `.quake/metrics/<testnet>/`.
    Metrics {
        /// Start of the time range (default: headStats.minTime from Prometheus; e.g. 2024-01-15T10:30:00Z or 2024-01-15)
        #[clap(long)]
        from: Option<CliTimestamp>,
        /// End of the time range (default: now; e.g. 2024-01-15T10:30:00Z or 2024-01-15)
        #[clap(long)]
        to: Option<CliTimestamp>,
        /// Query resolution — interval between data points (e.g. 30s, 1m, 5m).
        /// Defaults to ceil((end-start)/10000) to stay within Prometheus' 11 000-point limit.
        #[clap(long)]
        step: Option<String>,
        /// Metric names to download (pass after `--`; downloads every metric Prometheus knows about by default)
        #[clap(last = true)]
        metric_names: Vec<String>,
        /// Output file path (default: .quake/metrics/<testnet>/quake-metrics-<timestamp>.tar.gz)
        #[clap(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// Download node database(s).
    ///
    /// In remote mode: archives DB data from the selected node(s) via the CC and
    /// downloads the resulting `.tar.gz` to `.quake/db/<testnet>/`.
    ///
    /// In local mode: prints the path to the selected node's on-disk DB files;
    /// no archiving is performed since they are already on the local filesystem.
    ///
    /// Defaults to the first node in the manifest (DBs across nodes are typically
    /// identical; downloading from all of them is wasteful). Pass node names
    /// after `--` to override.
    Db {
        /// Node names to download from (default: first node in manifest)
        #[clap(last = true)]
        nodes: Vec<String>,
        /// Download only execution layer (Reth) data
        #[clap(long, conflicts_with = "consensus_only")]
        execution_only: bool,
        /// Download only consensus layer (Malachite) data
        #[clap(long)]
        consensus_only: bool,
        /// Output file path (remote only; default: .quake/db/<testnet>/quake-db-<timestamp>.tar.gz)
        #[clap(short = 'o', long)]
        output: Option<PathBuf>,
    },
}

/// A datetime accepted by `--from`/`--to` flags, converted to a Unix timestamp.
///
/// Accepted formats (timezone-naive values are treated as UTC):
///   `2024-01-15T10:30:00Z`, `2024-01-15T10:30:00+05:00` (RFC 3339)
///   `2024-01-15T10:30:00`, `2024-01-15 10:30:00` (naive datetime, UTC assumed)
///   `2024-01-15` (date only, start of day UTC)
#[derive(Debug, Clone, Copy)]
pub(crate) struct CliTimestamp(i64);

impl CliTimestamp {
    pub(crate) fn unix_secs(self) -> i64 {
        self.0
    }
}

impl std::str::FromStr for CliTimestamp {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return Ok(CliTimestamp(dt.timestamp()));
        }
        for fmt in &["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
            if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
                return Ok(CliTimestamp(Utc.from_utc_datetime(&ndt).timestamp()));
            }
        }
        if let Ok(nd) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            let ndt = nd.and_hms_opt(0, 0, 0).expect("valid HMS");
            return Ok(CliTimestamp(Utc.from_utc_datetime(&ndt).timestamp()));
        }
        Err(format!(
            "invalid datetime '{s}'; expected RFC 3339 or one of: \
             YYYY-MM-DDTHH:MM:SS, YYYY-MM-DD HH:MM:SS, YYYY-MM-DD"
        ))
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum DownloadSubcommand {
    /// Download Prometheus metrics from the Control Center.
    ///
    /// Alias of `quake metrics download` kept for backward compatibility.
    /// Requires the SSM tunnel to be open (start with `quake remote ssm start`).
    /// Downloads all metrics by default; pass metric names after -- to filter.
    /// Without --from/--to, start defaults to headStats.minTime (Prometheus head block only, ~2h max).
    Metrics {
        /// Start of the time range (default: headStats.minTime from Prometheus, covering the current head block; e.g. 2024-01-15T10:30:00Z or 2024-01-15)
        #[clap(long)]
        from: Option<CliTimestamp>,
        /// End of the time range (default: now; e.g. 2024-01-15T10:30:00Z or 2024-01-15)
        #[clap(long)]
        to: Option<CliTimestamp>,
        /// Query resolution — interval between data points (e.g. 30s, 1m, 5m).
        /// Defaults to ceil((end-start)/10000) to stay within Prometheus' 11,000-point limit.
        #[clap(long)]
        step: Option<String>,
        /// Metric names to download (all metrics if not specified)
        #[clap(last = true)]
        metric_names: Vec<String>,
        /// Output file path (default: ./quake-metrics-<timestamp>.tar.gz)
        #[clap(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// Download node databases from one or more remote validators.
    ///
    /// Defaults to all nodes in the manifest. Pass node names after -- to restrict.
    Db {
        /// Node names to download from (default: all nodes in manifest)
        #[clap(last = true)]
        nodes: Vec<String>,
        /// Download only execution layer (Reth) data
        #[clap(long, conflicts_with = "consensus_only")]
        execution_only: bool,
        /// Download only consensus layer (Malachite) data
        #[clap(long)]
        consensus_only: bool,
        /// Output file path (default: ./quake-db-<timestamp>.tar.gz)
        #[clap(short = 'o', long)]
        output: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SSMSubcommand {
    /// Start SSM tunnels to the Control Center server
    Start,
    /// Stop all SSM tunnels
    Stop,
    /// List all active SSM tunnels
    List,
    /// Keep all SSM tunnels alive for the given duration
    ///
    /// AWS closes SSM sessions after 20 minutes of inactivity. This command
    /// periodically opens a TCP connection through each tunnel to reset the
    /// inactivity timer. Run it in a separate terminal alongside long
    /// experiments. Cancel with Ctrl-C when no longer needed.
    #[command(verbatim_doc_comment)]
    KeepAlive {
        /// How long to keep tunnels alive (e.g. "30m", "2h", "1h30m")
        #[clap(value_parser = parse_duration)]
        duration: Duration,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    reset_sigpipe_to_default();

    dotenvy::dotenv().ok();

    let cli = Cli::parse();

    let level = cli.verbosity.tracing_level_filter();
    let filter = EnvFilter::builder()
        .with_default_directive(level.into())
        .from_env()?
        .add_directive("hyper_util::client=info".parse()?)
        .add_directive("arc_node_consensus_cli::new=info".parse()?);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    tracing::info!(
        version = arc_version::GIT_VERSION,
        commit = arc_version::GIT_COMMIT_HASH,
        "Quake starting"
    );

    if let Commands::Generate { output_dir, count } = cli.command {
        return generate_manifests(count, &output_dir, cli.seed);
    }

    if let Commands::Remote {
        command: RemoteSubcommand::Import { path },
    } = &cli.command
    {
        #[cfg(not(unix))]
        {
            bail!(
                "`quake remote import` is only supported on Unix-like platforms because SSH private key permissions (0600) cannot be enforced"
            );
        }
        export::import_shared_testnet(path)?;
        return Ok(());
    }

    // Force the use of remote mode on certain sub-commands
    let force_remote = matches!(
        cli.command,
        Commands::Remote {
            command: RemoteSubcommand::Preinit,
        } | Commands::Remote {
            command: RemoteSubcommand::Create { .. },
        } | Commands::Remote {
            command: RemoteSubcommand::Destroy { .. },
        } | Commands::Remote {
            command: RemoteSubcommand::Status
        } | Commands::Remote {
            command: RemoteSubcommand::Monitor { .. }
        } | Commands::Start {
            start_args: StartArgs { remote: true, .. },
            ..
        } | Commands::Restart {
            start_args: StartArgs { remote: true, .. },
            ..
        }
    );

    // Build testnet from manifest
    let testnet_result = Testnet::from(&cli.manifest_file, force_remote).await;

    // Handle the case where clean is called but no testnet exists
    if let Err(ref err) = testnet_result {
        if let Some(TestnetError::NoManifestFound(_)) = err.downcast_ref::<TestnetError>() {
            if matches!(cli.command, Commands::Clean { .. }) {
                info!("No existing testnet to clean, skipping.");
                debug!("Details: {err}");
                return Ok(());
            }
        }
    }

    let mut testnet = testnet_result?;

    // Use the manifest to determine if we should use RPC for the Engine API
    // connection, unless overridden by the command line
    let rpc_manifest = matches!(
        testnet.manifest.engine_api_connection,
        Some(EngineApiConnection::Rpc)
    );

    match cli.command {
        Commands::Setup { args } => {
            let rpc = args.rpc || rpc_manifest;
            testnet
                .with_seed(cli.seed)
                .setup(
                    args.force,
                    rpc,
                    args.num_extra_accounts,
                    args.extra_account_balance,
                    args.block_gas_limit,
                )
                .await?;
        }
        Commands::Build { args } => {
            if let Err(err) = testnet.infra.is_setup(&[]) {
                bail!("Infra is not set up: {err}: run `quake setup` first to create the testnet infrastructure");
            }
            testnet.build(args.profile).await?
        }
        Commands::Start { start_args } => {
            pre_start(
                &mut testnet,
                &start_args,
                &cli.manifest_file,
                cli.seed,
                rpc_manifest,
            )
            .await?;
            testnet
                .start(start_args.nodes_or_containers, start_args.monitoring)
                .await?
        }
        Commands::Stop {
            nodes_or_containers,
        } => testnet.stop(nodes_or_containers).await?,
        Commands::Clean { clean_args } => testnet.clean(clean_args.scope()).await?,
        Commands::Restart {
            clean_args,
            start_args,
        } => {
            testnet.clean(clean_args.scope()).await?;
            pre_start(
                &mut testnet,
                &start_args,
                &cli.manifest_file,
                cli.seed,
                rpc_manifest,
            )
            .await?;
            testnet
                .start(start_args.nodes_or_containers, start_args.monitoring)
                .await?;
        }
        Commands::Perturb {
            action,
            min_time_off,
            max_time_off,
        } => {
            testnet
                .with_seed(cli.seed)
                .perturb(action, min_time_off, max_time_off)
                .await?
        }
        Commands::Logs { names, follow } => testnet.logs(names, follow).await?,
        Commands::Info { command } => testnet.info(command).await?,
        Commands::Remote { command } => testnet.remote(command).await?,
        Commands::Monitoring { command } => testnet.monitoring(command).await?,
        Commands::Download { command } => testnet.download(command).await?,
        Commands::Load { targets, args } => {
            let target_nodes = targets.unwrap_or_default();
            load::run(
                &testnet,
                target_nodes,
                &args,
                false,
                cli.verbosity.is_silent(),
            )
            .await?;
        }
        Commands::Spam { targets, args } => {
            let target_nodes = targets.unwrap_or_default();
            load::run(
                &testnet,
                target_nodes,
                &args,
                true,
                cli.verbosity.is_silent(),
            )
            .await?;
        }
        Commands::ValSet { updates } => testnet.valset_update(updates).await?,
        Commands::Test {
            spec,
            dry_run,
            rpc_timeout,
            params,
        } => {
            let params = crate::tests::TestParams::from(params);
            testnet
                .with_seed(cli.seed)
                .run_tests(&spec, dry_run, rpc_timeout, &params)
                .await?
        }
        Commands::Report { output, params } => {
            let params = crate::tests::TestParams::from(params);
            report::run_report(&testnet, &params, &output).await?
        }
        Commands::Wait { command } => match command {
            WaitSubcommand::Height {
                height,
                nodes,
                timeout,
            } => {
                testnet
                    .wait(height, &nodes, Duration::from_secs(timeout))
                    .await?
            }
            WaitSubcommand::Sync {
                nodes,
                timeout,
                max_retries,
            } => {
                testnet
                    .wait_sync(nodes, Duration::from_secs(timeout), max_retries)
                    .await?
            }
            WaitSubcommand::Rounds {
                consecutive,
                timeout,
            } => {
                testnet
                    .wait_rounds(consecutive, Duration::from_secs(timeout))
                    .await?
            }
        },
        Commands::Rpc { command } => match command {
            RpcSubcommand::List => {
                let (node, base_url) = testnet.nodes_metadata.first_consensus_rpc_url();
                rpc_cmd::run_list(node, base_url).await?
            }
            RpcSubcommand::El(args) => {
                let (target, _params) = args.parse_positionals()?;
                let selectors = parse_comma_separated_string(target.as_deref())?;
                let node_urls = testnet
                    .nodes_metadata
                    .resolve_el_targets(&testnet.manifest, selectors.as_deref())?;
                rpc_cmd::run_el(node_urls, args).await?
            }
            RpcSubcommand::Cl(args) => {
                let selectors = parse_comma_separated_string(args.target.as_deref())?;
                let node_urls = testnet
                    .nodes_metadata
                    .resolve_cl_targets(&testnet.manifest, selectors.as_deref())?;
                rpc_cmd::run_cl(node_urls, args).await?
            }
        },
        Commands::Web {
            host,
            port,
            refresh_ms,
            el_refresh_ms,
            container_refresh_ms,
        } => {
            if !testnet.is_local() {
                bail!("Web server is currently only supported in local mode.");
            }
            crate::web::run_server(
                testnet,
                host,
                port,
                refresh_ms,
                el_refresh_ms,
                container_refresh_ms,
            )
            .await?;
        }
        Commands::Mcp { http, port } => {
            crate::mcp::run_server(testnet, http, port).await?;
        }
        Commands::Generate { .. } => {} // handled above
    }

    Ok(())
}

/// Restore the POSIX default disposition for SIGPIPE so the process exits
/// quietly on broken pipes (e.g. `quake rpc ... | head -1`) instead of
/// panicking from a failed `println!`. Rust installs `SIG_IGN` for SIGPIPE at
/// startup; every well-behaved CLI tool undoes that.
fn reset_sigpipe_to_default() {
    #[cfg(unix)]
    {
        // SIGPIPE = 13 and SIG_DFL = null sighandler on every POSIX target.
        const SIGPIPE: i32 = 13;
        unsafe extern "C" {
            fn signal(
                signum: i32,
                handler: Option<extern "C" fn(i32)>,
            ) -> Option<extern "C" fn(i32)>;
        }
        // SAFETY: `signal` is an atomic process-global operation. No other
        // code in this process modifies SIGPIPE disposition, so there is no
        // race.
        unsafe {
            signal(SIGPIPE, None);
        }
    }
}

/// Parse a time duration from a string formatted as a human-readable duration.
pub(crate) fn parse_duration(s: &str) -> Result<Duration> {
    humantime::parse_duration(s).wrap_err_with(|| format!("invalid duration: {s}"))
}

fn parse_key_value(s: &str) -> Result<(String, String)> {
    let (key, value) = s
        .split_once('=')
        .ok_or_else(|| eyre::eyre!("expected key=value, got: {s}"))?;
    Ok((key.to_string(), value.to_string()))
}

/// Split a comma-separated string into a list of strings.
fn parse_comma_separated_string(target: Option<&str>) -> Result<Option<Vec<String>>> {
    target.map(split_comma_names).transpose()
}

fn split_comma_names(str: &str) -> Result<Vec<String>> {
    let names: Vec<String> = str
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        bail!("Empty target node list");
    }
    Ok(names)
}

/// Prepare the testnet before starting it
///
/// If the remote flag is set, it will create the remote infrastructure and reload the testnet.
/// If the testnet is not set up, it will run `quake setup` to set it up.
/// If the Docker images do not exist, it will run `quake build` to build them.
async fn pre_start(
    testnet: &mut Testnet,
    args: &StartArgs,
    manifest_file: &Option<PathBuf>,
    seed: Option<u64>,
    rpc_manifest: bool,
) -> Result<()> {
    // Warn if sizing fields are set in the manifest but we're running locally — they are ignored.
    if !args.remote {
        let m = &testnet.manifest;
        if m.node_size.is_some()
            || m.cc_size.is_some()
            || m.node_disk_gb.is_some()
            || m.cc_disk_gb.is_some()
            || m.node_volume_type.is_some()
            || m.node_volume_iops.is_some()
            || m.node_data_on_instance_store
        {
            warn!(
                "Manifest sets remote-only infrastructure fields (node_size/cc_size/\
                 node_disk_gb/cc_disk_gb/node_volume_type/node_volume_iops/\
                 node_data_on_instance_store), but these are only applied when creating \
                 remote infrastructure (--remote). They are ignored in local mode."
            );
        }
    }

    // Create remote infrastructure, if requested and not already created
    if args.remote && !testnet.dir.join(INFRA_DATA_FILENAME).exists() {
        info!("Creating remote infrastructure...");
        let node_size = args
            .infra_args
            .node_size
            .as_deref()
            .or(testnet.manifest.node_size.as_deref());
        let cc_size = args
            .infra_args
            .cc_size
            .as_deref()
            .or(testnet.manifest.cc_size.as_deref());
        let node_disk_gb = args
            .infra_args
            .node_disk_gb
            .or(testnet.manifest.node_disk_gb);
        let cc_disk_gb = args.infra_args.cc_disk_gb.or(testnet.manifest.cc_disk_gb);
        let node_volume_type = args
            .infra_args
            .node_volume_type
            .as_deref()
            .or(testnet.manifest.node_volume_type.as_deref());
        let node_volume_iops = args
            .infra_args
            .node_volume_iops
            .or(testnet.manifest.node_volume_iops);
        // Enabled by either the CLI flag or the manifest field.
        let node_data_on_instance_store = args.infra_args.node_data_on_instance_store
            || testnet.manifest.node_data_on_instance_store;
        // CLI overrides can mix freely with manifest fields, so re-validate
        // the merged pair before reaching Terraform.
        crate::manifest::validate_node_volume(node_volume_type, node_volume_iops)?;
        testnet.remote_infra()?.terraform.create(
            false,
            true,
            node_size,
            cc_size,
            node_disk_gb,
            cc_disk_gb,
            node_volume_type,
            node_volume_iops,
            node_data_on_instance_store,
        )?;

        // Reload testnet with the recently created infra files
        *testnet = Testnet::from(manifest_file, true).await?;
    }

    // Check if the testnet is set up
    let setup_args = &args.setup_args;
    let nodes = testnet
        .nodes_metadata
        .expand_to_nodes_list(&args.nodes_or_containers)?;
    if let Err(err) = testnet.infra.is_setup(&nodes) {
        let rpc = setup_args.rpc || rpc_manifest;
        warn!("Testnet not set up: {err}; Running setup...");
        testnet
            .with_seed(seed)
            .setup(
                setup_args.force,
                rpc,
                setup_args.num_extra_accounts,
                setup_args.extra_account_balance,
                setup_args.block_gas_limit,
            )
            .await?;
    }

    // Build Docker images if they do not exist, for local infrastructure only
    if testnet.is_local() {
        if let Err(err) = infra::docker::images_exist(&testnet.images) {
            warn!("Docker images do not exist: {err}; running `quake build`...");
            testnet.build(args.build_args.profile).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::CommandFactory;

    #[test]
    fn cli_command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn cli_parses_load_targets() {
        let cli = Cli::try_parse_from([
            "quake",
            "load",
            "--rate",
            "42",
            "--targets",
            "validator1,ALL_VALIDATORS",
        ])
        .expect("parsing load with --targets");

        match cli.command {
            Commands::Load { targets, args } => {
                assert_eq!(
                    targets,
                    Some(vec!["validator1".to_string(), "ALL_VALIDATORS".to_string(),])
                );
                assert_eq!(args.rate, 42);
            }
            _ => panic!("expected load command"),
        }
    }

    #[test]
    fn cli_rejects_positional_load_targets() {
        let err = match Cli::try_parse_from(["quake", "load", "validator1"]) {
            Ok(_) => panic!("positional load targets must be rejected"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
        assert!(err.to_string().contains("validator1"));
    }

    #[test]
    fn split_comma_names_parses_basic_list() {
        let v = split_comma_names("validator1,validator2,ALL_VALIDATORS").unwrap();
        assert_eq!(
            v,
            vec![
                "validator1".to_string(),
                "validator2".to_string(),
                "ALL_VALIDATORS".to_string(),
            ]
        );
    }

    #[test]
    fn split_comma_names_trims_whitespace_and_skips_empties() {
        let v = split_comma_names(" validator1 , , ALL_VALIDATORS  ").unwrap();
        assert_eq!(
            v,
            vec!["validator1".to_string(), "ALL_VALIDATORS".to_string(),]
        );
    }

    #[test]
    fn split_comma_names_rejects_empty_input() {
        assert!(split_comma_names("").is_err());
        assert!(split_comma_names(",,,").is_err());
    }
}
