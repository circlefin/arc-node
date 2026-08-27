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

//! Arc Network - A custom Reth node implementation
//!
//! This example demonstrates how to create a custom blockchain node using Reth
//! with custom EVM configuration, precompiles, and transaction pool.

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Profiling configuration for jemalloc.
#[cfg(feature = "pprof")]
#[allow(non_upper_case_globals)]
#[unsafe(export_name = "malloc_conf")]
pub static malloc_conf: &[u8] = b"prof:true,prof_active:false,lg_prof_sample:19\0";

use arc_evm_node::node::{ArcNode, ArcRpcConfig};
use arc_evm_node::ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT;
use arc_execution_config::addresses_denylist::{
    AddressesDenylistConfig, DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
};
use arc_execution_config::chainspec::{ArcChainSpec, ArcChainSpecParser};
use arc_execution_config::defaults;
use arc_execution_config::follow;
use arc_node_execution::patch_node_command_defaults;
use clap::{Args, CommandFactory, FromArgMatches, Parser};
use directories::BaseDirs;
use reth_chainspec::EthChainSpec;
use reth_ethereum::cli::interface::{Cli as RethCli, Commands};
use reth_ethereum::cli::parse_duration_from_secs_or_ms;
use reth_node_core::version::default_extra_data_bytes;
use reth_rpc_builder::config::RethRpcServerConfig;
use reth_rpc_server_types::{RethRpcModule, RpcModuleSelection};
use tracing::info;

use std::collections::HashSet;
use std::sync::Arc;

use reth_node_core::args::DefaultPruningValues;
use reth_prune_types::{PruneMode, PruneModes};

use arc_execution_txpool::{InvalidTxListConfig, ARC_INVALID_TX_LIST_DEFAULT_CAP};
use arc_node_execution::ArcConsensus;
use arc_node_execution::ArcEvmConfig;
use arc_node_execution::ArcEvmFactory;
use reth_db::DatabaseEnv;
use reth_node_builder::NodeBuilder;
use reth_node_builder::WithLaunchContext;
use reth_node_ethereum::EthEvmConfig;

/// Arc Network node CLI with custom version handling
#[derive(Debug, Parser)]
#[command(
    name = "arc-node-execution",
    version = arc_version::SHORT_VERSION,
    long_version = arc_version::LONG_VERSION,
    about = "Arc execution layer",
    disable_help_subcommand = true
)]
struct ArcCli {
    #[command(flatten)]
    inner: RethCli<ArcChainSpecParser, ArcExtraCli>,
}

impl ArcCli {
    /// Validate Arc-specific CLI constraints.
    fn validate(&self) -> Result<(), &'static str> {
        if let Commands::Node(ref node_cmd) = self.inner.command {
            // Reject --builder.extradata if user explicitly set it.
            // Arc uses the extra_data field to store the next block's base fee.
            if node_cmd.builder.extra_data != default_extra_data_bytes() {
                return Err("--builder.extradata is not supported");
            }

            // The middleware intercepts pending-block and pool-based queries in both
            // single and batch paths. Enforce `--rpc.pending-block=none` so reth
            // replaces pending data with finalized data for all other queries.
            if compute_filter_pending_txs(&node_cmd.ext)
                && node_cmd.rpc.rpc_pending_block
                    != reth_rpc_eth_types::builder::config::PendingBlockKind::None
            {
                return Err(
                    "--rpc.pending-block must be 'none' when the pending-tx filter is active; \
                     pass --arc.expose-pending-txs to opt out of hiding or set --rpc.pending-block=none",
                );
            }
        }
        Ok(())
    }
}

fn arc_components(spec: Arc<ArcChainSpec>) -> (ArcEvmConfig, Arc<ArcConsensus<ArcChainSpec>>) {
    let eth_evm =
        EthEvmConfig::new_with_evm_factory(spec.clone(), ArcEvmFactory::new(spec.clone()));
    let evm = ArcEvmConfig::new(eth_evm);
    let consensus = Arc::new(ArcConsensus::new(spec.clone()));

    (evm, consensus)
}

/// Configure the node builder to follow a trusted node for consensus.
fn follow_url_for_consensus(
    builder: &mut WithLaunchContext<NodeBuilder<DatabaseEnv, ArcChainSpec>>,
    follow_url: &str,
) -> eyre::Result<()> {
    let chain_id = builder.config().chain.chain().id();

    let url = if follow_url.is_empty() || follow_url == "auto" {
        follow::ws_url_for_chain_id(chain_id)?
    } else {
        follow_url.to_string()
    };

    info!("🔗 Following trusted node: {}", url);

    // Configure the builder to use the follow URL for consensus (get the latest block and subscribe for new blocks)
    //
    // "Runs a fake consensus client using blocks fetched from an RPC endpoint.
    // Supports both HTTP and WebSocket endpoints - WebSocket endpoints will use
    // subscriptions, while HTTP endpoints will poll for new blocks"
    builder.config_mut().debug.rpc_consensus_url = Some(url);

    // Configure trusted peers (needed to backfill the missing blocks via devp2p)
    if let Ok(trusted_peers) = follow::trusted_peers_for_chain_id(chain_id) {
        if !trusted_peers.is_empty() {
            info!(
                "🤝 Configuring {} trusted peers for chain {}",
                trusted_peers.len(),
                chain_id
            );
            builder.config_mut().network.trusted_peers = trusted_peers;
        }
    }

    Ok(())
}

#[derive(Debug, Args)]
struct ArcExtraCli {
    /// Enable custom ARC RPC namespace (certificates etc.).
    #[arg(long = "enable-arc-rpc", default_value_t = false)]
    enable_arc_rpc: bool,
    /// Upstream malachite-app base URL used by ARC RPC (e.g. http://127.0.0.1:31000).
    #[arg(
        long = "arc-rpc-upstream-url",
        value_name = "URL",
        env = "ARC_RPC_UPSTREAM_URL"
    )]
    arc_rpc_upstream_url: Option<String>,

    /// Run an RPC node (unsafe - no verification).
    ///
    /// Use without a value (--unsafe-follow) to automatically use the preconfigured trusted node or
    /// provide the WebSocket URL of the trusted node (e.g., ws://trusted-node:8546).
    #[arg(
        long = "unsafe-follow",
        value_name = "URL",
        env = "ARC_UNSAFE_FOLLOW_URL",
        default_missing_value = "auto",
        num_args = 0..=1
    )]
    unsafe_follow_url: Option<String>,

    /// Enable the invalid transaction list.
    ///
    /// When enabled, problematic transactions that cause builder panics or errors
    /// are cached and rejected on subsequent submissions. A builder panic flushes
    /// all currently-pending transactions into the list; resubmit them after
    /// investigating the panic.
    #[arg(
        long = "invalid-tx-list-enable",
        default_value_t = true,
        // Flag is true by default; `Set` action lets `--invalid-tx-list-enable=false` opt out.
        action = clap::ArgAction::Set,
        help_heading = "Invalid tx list"
    )]
    invalid_tx_list_enable: bool,

    /// Maximum capacity of the invalid tx list LRU cache.
    ///
    /// Only relevant when --invalid-tx-list-enable is true.
    /// A value of 0 disables storage (all inserts are ignored, but counted in metrics).
    #[arg(
        long = "invalid-tx-list-cap",
        default_value_t = ARC_INVALID_TX_LIST_DEFAULT_CAP,
        value_name = "CAPACITY",
        help_heading = "Invalid tx list"
    )]
    invalid_tx_list_cap: u32,

    /// Maximum number of entries permitted in a JSON-RPC batch request.
    ///
    /// Batches with more entries are rejected before any per-entry handler runs.
    /// Must be >= 1.
    #[arg(
        long = "arc.rpc.max-batch-entries",
        default_value_t = ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT,
        value_parser = parse_max_batch_entries,
        value_name = "COUNT",
        help_heading = "Arc RPC limits"
    )]
    arc_rpc_max_batch_entries: usize,

    /// Maximum duration for the custom payload builder's transaction selection loop, in milliseconds.
    ///
    /// When unset, Reth's `builder.deadline` (seconds) is adopted as the maximum loop duration.
    #[arg(
        long = "arc.builder.deadline",
        value_name = "MS",
        env = "ARC_BUILDER_DEADLINE_MS",
        help_heading = "Payload builder deadline"
    )]
    payload_builder_deadline_ms: Option<u64>,

    /// Wait for the in-flight payload build instead of racing an
    /// empty block when `engine_getPayload` arrives early.
    #[arg(
        long = "arc.builder.wait-for-payload",
        default_value_t = true,
        // because the flag is true by default, we need `Set` action so that we can
        // do `--arc.builder.wait-for-payload=false` in the CLI.
        action = clap::ArgAction::Set,
        env = "ARC_BUILDER_WAIT_FOR_PAYLOAD",
        help_heading = "Payload builder"
    )]
    wait_for_payload: bool,

    /// Comma-separated addresses to exclude from denylist checks (e.g. for ops recovery).
    #[arg(
        long = "arc.denylist.addresses-exclusions",
        value_name = "ADDRESSES",
        value_delimiter = ',',
        help_heading = "Arc denylist"
    )]
    arc_denylist_addresses_exclusions: Vec<String>,

    /// Expose pending-tx RPCs on externally-reachable sockets.
    ///
    /// Off by default: the middleware blocks `eth_subscribe("newPendingTransactions")`,
    /// `eth_newPendingTransactionFilter`, and returns null for
    /// `eth_getBlockByNumber("pending")` and `eth_getTransactionBySenderAndNonce`.
    /// Set this flag on trusted / internal nodes where exposing pending-tx state
    /// is desired (e.g. debugging).
    #[arg(
        long = "arc.expose-pending-txs",
        default_value_t = false,
        help_heading = "Arc RPC"
    )]
    arc_expose_pending_txs: bool,

    /// Convenience flag for externally-exposed RPC nodes.
    ///
    /// Forces hiding of pending-tx RPCs. Conflicts with
    /// `--arc.expose-pending-txs`, and warns at startup if `--http.api` or
    /// `--ws.api` exposes namespaces outside `{eth, net, web3, rpc}`.
    #[arg(
        long = "public-api",
        default_value_t = false,
        conflicts_with = "arc_expose_pending_txs",
        help_heading = "Arc RPC"
    )]
    public_api: bool,

    /// Accept pre-EIP-155 (replay-unprotected) transactions over JSON-RPC.
    ///
    /// Defaults to false, matching Geth: raw transaction submission RPCs reject
    /// transactions whose signature does not encode a chain ID, returning the
    /// standard error "only replay-protected (EIP-155) transactions allowed over RPC".
    /// Affects the RPC submission path only — transactions received from
    /// peers or included in blocks by other validators are still accepted
    /// by the txpool and execution layers.
    ///
    /// Enable on nodes that need to relay legacy deployer transactions
    /// (Nick's-method singletons such as CreateX, ERC-2470, ERC-1820).
    #[arg(
        long = "arc.rpc.allow-unprotected-txs",
        default_value_t = false,
        help_heading = "Arc RPC"
    )]
    arc_rpc_allow_unprotected_txs: bool,

    /// Comma-separated upstream RPC URLs for transaction relay, in priority order.
    ///
    /// Follow nodes relay raw-transaction submission (`eth_sendRawTransaction`
    /// and `eth_sendRawTransactionSync`) to these upstreams with failover.
    /// Conflicts with Reth's `--rpc.forwarder`.
    #[arg(
        long = "arc.tx.relays",
        env = "ARC_TX_RELAYS",
        value_delimiter = ',',
        value_name = "URLS",
        conflicts_with = "rpc_forwarder",
        help_heading = "Arc RPC"
    )]
    arc_tx_relays: Vec<String>,

    /// Transaction relay timeout: `10s`, `500ms`, or a bare number of seconds.
    ///
    /// Bounds each relay attempt; when it elapses the relay advances to the next
    /// upstream.
    #[arg(
        long = "arc.tx.relays.timeout",
        env = "ARC_TX_RELAYS_TIMEOUT",
        value_parser = parse_duration_from_secs_or_ms,
        value_name = "DURATION",
        default_value = "10s",
        help_heading = "Arc RPC"
    )]
    arc_tx_relays_timeout: std::time::Duration,

    /// Interval in seconds between transaction rebroadcast rounds.
    ///
    /// Pending transactions are periodically re-announced to all peers to recover
    /// from missed gossip. Set to 0 to disable.
    #[arg(
        long = "txpool.rebroadcast-interval",
        value_name = "SECONDS",
        default_value_t = 60,
        help_heading = "Transaction pool"
    )]
    txpool_rebroadcast_interval: u64,

    /// Profiling server bind address.
    #[arg(
        long = "pprof.addr",
        value_name = "ADDR",
        default_value = "0.0.0.0:6061",
        help_heading = "Profiling"
    )]
    pprof_addr: String,

    /// Activate jemalloc heap profiling at startup.
    ///
    /// When built with the `pprof` feature, heap profiling infrastructure is
    /// always available but inactive by default. This flag activates it so
    /// that the `/debug/pprof/allocs` endpoint returns meaningful data.
    #[arg(
        long = "pprof.heap-prof",
        default_value_t = false,
        help_heading = "Profiling"
    )]
    pprof_heap_prof: bool,
}

/// Build [`AddressesDenylistConfig`] for the chain being run.
///
/// The contract address and storage slot come from the chain spec, not from CLI flags: the
/// denylist is a protocol requirement, so it is not operator-configurable. Only the
/// ops-recovery exclusions remain operator-supplied.
fn build_addresses_denylist_config(
    chain_spec: &ArcChainSpec,
    ext: &ArcExtraCli,
) -> eyre::Result<AddressesDenylistConfig> {
    use alloy_primitives::Address;

    let addresses_exclusions: Vec<Address> = ext
        .arc_denylist_addresses_exclusions
        .iter()
        .map(|s| s.trim().parse::<Address>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| eyre::eyre!("invalid --arc.denylist.addresses-exclusions: {}", e))?;

    // No denylist-free node: an unrecognised chain spec is refused rather than run unchecked.
    let contract_address = chain_spec.denylist_address().ok_or_else(|| {
        eyre::eyre!(
            "no Arc denylist is defined for chain id {}; refusing to start without denylist checks",
            chain_spec.chain().id()
        )
    })?;

    Ok(AddressesDenylistConfig::new(
        contract_address,
        DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
        addresses_exclusions,
    ))
}

/// Validates `--arc.tx.relays` and returns the upstream URLs in priority order.
///
/// An empty list disables relaying. A non-empty list must contain only valid
/// URLs; any unparseable entry is a startup error.
fn build_tx_relays(ext: &ArcExtraCli) -> eyre::Result<Vec<String>> {
    ext.arc_tx_relays
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            url::Url::parse(s)
                .map(|_| s.to_string())
                .map_err(|e| eyre::eyre!("invalid --arc.tx.relays entry {s:?}: {e}"))
        })
        .collect()
}

/// Namespaces considered safe on a `--public-api` node.
///
/// Excludes anything that exposes pending / mempool state, admin controls,
/// tracing, MEV endpoints, or implementation-specific internals.
const PUBLIC_API_SAFE_MODULES: [RethRpcModule; 4] = [
    RethRpcModule::Eth,
    RethRpcModule::Net,
    RethRpcModule::Web3,
    RethRpcModule::Rpc,
];

/// Returns the modules in `selection` that are not in `PUBLIC_API_SAFE_MODULES`.
/// `RethRpcModule::Other(_)` is always considered unsafe.
pub(crate) fn unsafe_public_api_modules(selection: &RpcModuleSelection) -> Vec<RethRpcModule> {
    let safe: HashSet<RethRpcModule> = PUBLIC_API_SAFE_MODULES.into_iter().collect();
    selection
        .to_selection()
        .into_iter()
        .filter(|m| !safe.contains(m))
        .collect()
}

/// Emits a `warn!` if `selection` contains modules outside the safe set.
/// `None` is safe: Reth's default is `Standard` = {eth, net, web3}, a subset of our safe set.
fn warn_if_public_api_unsafe(selection: Option<&RpcModuleSelection>, socket_flag: &str) {
    let Some(sel) = selection else { return };
    let unsafe_modules = unsafe_public_api_modules(sel);
    if !unsafe_modules.is_empty() {
        let names: Vec<String> = unsafe_modules.iter().map(|m| m.to_string()).collect();
        tracing::warn!(
            "--public-api set but {socket_flag} exposes sensitive namespaces: {names:?}. \
             Consider dropping them or removing --public-api to acknowledge the risk."
        );
    }
}

/// Computes whether the pending-tx RPC filter should be active for this run.
/// `--public-api` wins; clap enforces it can't coexist with `--arc.expose-pending-txs`.
fn compute_filter_pending_txs(ext: &ArcExtraCli) -> bool {
    ext.public_api || !ext.arc_expose_pending_txs
}

/// Parses `--arc.rpc.max-batch-entries`, rejecting `0` so the cap is never silently disabled.
fn parse_max_batch_entries(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|_| format!("invalid number: {s}"))?;
    if n == 0 {
        return Err("must be >= 1".to_string());
    }
    Ok(n)
}

/// Number of bodies, receipts, etc. to retain after pruning.
/// See init_arc_pruning for more details.
const PRESETS_PRUNE_DISTANCE: u64 = 237_600;
const FLAG_FULL: &str = "--full";
const FLAG_MINIMAL: &str = "--minimal";
const FLAG_BLOCK_INTERVAL: &str = "--prune.block-interval=128";
const FLAG_DATADIR: &str = "--datadir";

/// Registers Arc-specific `DefaultPruningValues` with Reth's global static, then injects
/// Arc defaults into argv:
/// - `--prune.block-interval=128` whenever `--full` or `--minimal` is present
/// - `--datadir=~/.arc/execution` unless the user already supplied `--datadir`
fn init_arc_pruning<I, S>(argv: I) -> Vec<std::ffi::OsString>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString>,
{
    // Register Arc-specific pruning defaults. This must happen before clap parses --full /
    // --minimal, so that DefaultPruningValues::get_global() returns our values.
    let _ = DefaultPruningValues::default()
        .with_full_prune_modes(PruneModes {
            sender_recovery: Some(PruneMode::Full),
            transaction_lookup: Some(PruneMode::Distance(PRESETS_PRUNE_DISTANCE)),
            receipts: Some(PruneMode::Distance(PRESETS_PRUNE_DISTANCE)),
            account_history: Some(PruneMode::Distance(PRESETS_PRUNE_DISTANCE)),
            storage_history: Some(PruneMode::Distance(PRESETS_PRUNE_DISTANCE)),
            bodies_history: Some(PruneMode::Distance(PRESETS_PRUNE_DISTANCE)),
            receipts_log_filter: Default::default(),
        })
        .with_full_bodies_history_use_pre_merge(false)
        .with_minimal_prune_modes(PruneModes {
            sender_recovery: Some(PruneMode::Full),
            transaction_lookup: Some(PruneMode::Distance(64)), // Can be `Full`, but we use 64 here because our smoke tests rely on tx lookup
            receipts: Some(PruneMode::Distance(64)),           // Min enforced by Reth
            account_history: Some(PruneMode::Distance(10064)), // Min enforced by Reth
            storage_history: Some(PruneMode::Distance(10064)), // Min enforced by Reth
            bodies_history: Some(PruneMode::Distance(PRESETS_PRUNE_DISTANCE)),
            receipts_log_filter: Default::default(),
        })
        .try_init();

    // Collect argv so we can inspect it before rewriting.
    let mut args: Vec<std::ffi::OsString> = argv.into_iter().map(Into::into).collect();

    // Inject --prune.block-interval=128 for `node` when --full or --minimal is
    // present, unless already supplied. Other subcommands (e.g. `download`) reuse
    // --full/--minimal with a different meaning and reject --prune.block-interval.
    let has_node_subcommand = args.get(1).and_then(|a| a.to_str()) == Some("node");
    let has_preset = args
        .iter()
        .any(|a| matches!(a.to_str(), Some(FLAG_FULL) | Some(FLAG_MINIMAL)));
    let has_explicit_block_interval = args.iter().any(|a| {
        a.to_str()
            .is_some_and(|s| s.starts_with("--prune.block-interval"))
    });
    if has_node_subcommand && has_preset && !has_explicit_block_interval {
        args.push(std::ffi::OsString::from(FLAG_BLOCK_INTERVAL));
    }

    // Inject --datadir=~/.arc/execution unless the user already supplied --datadir.
    // Only inject for subcommands that accept --datadir; skip the ones that don't.
    const SUBCOMMANDS_WITH_DATADIR: &[&str] = &[
        // Keep in sync with Reth subcommands that accept --datadir (as of Reth v1.11.3).
        // When upgrading Reth, check for new subcommands and update this list.
        "node",
        "init",
        "init-state",
        "import",
        "import-era",
        "export-era",
        "db",
        "download",
        "stage",
        "prune",
        "re-execute",
    ];
    let has_datadir_subcommand = args.iter().any(|a| {
        a.to_str()
            .is_some_and(|s| SUBCOMMANDS_WITH_DATADIR.contains(&s))
    });
    let has_explicit_datadir = args.iter().any(|a| {
        a.to_str()
            .is_some_and(|s| s == FLAG_DATADIR || s.starts_with("--datadir="))
    });
    if has_datadir_subcommand && !has_explicit_datadir {
        if let Some(home) = BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
            let datadir = home.join(".arc").join("execution");
            args.push(std::ffi::OsString::from(format!(
                "--datadir={}",
                datadir.display()
            )));
        }
    }

    args
}

fn main() {
    // Initialize Arc Network defaults (download URLs, etc.) before parsing CLI
    defaults::init_defaults();

    let argv = init_arc_pruning(std::env::args_os());
    let patched_cmd = patch_node_command_defaults(ArcCli::command());
    let cli =
        ArcCli::from_arg_matches(&patched_cmd.get_matches_from(argv)).unwrap_or_else(|e| e.exit());
    if let Err(err) = cli.validate() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }

    if let Err(err) = cli.inner.run_with_components::<ArcNode>(
        arc_components,
        |mut builder: WithLaunchContext<NodeBuilder<DatabaseEnv, ArcChainSpec>>,
         ext: ArcExtraCli| async move {
            // Resolved from the chain spec, so this must happen after the spec is parsed.
            let addresses_denylist_config =
                build_addresses_denylist_config(builder.config().chain.as_ref(), &ext)?;
            let arc_rpc_cfg =
                ArcRpcConfig::new(ext.enable_arc_rpc, ext.arc_rpc_upstream_url.clone());
            let invalid_tx_list_cfg =
                InvalidTxListConfig::new(ext.invalid_tx_list_enable, ext.invalid_tx_list_cap);
            let payload_builder_deadline_ms = ext.payload_builder_deadline_ms;

            if ext.public_api {
                let rpc = &builder.config().rpc;
                warn_if_public_api_unsafe(rpc.http_api.as_ref(), "--http.api");
                warn_if_public_api_unsafe(rpc.ws_api.as_ref(), "--ws.api");
            }

            // Run an RPC node if enabled (unsafe - no verification)
            if let Some(ref unsafe_follow_url) = ext.unsafe_follow_url {
                follow_url_for_consensus(&mut builder, unsafe_follow_url)?;
            }

            // Log version information when node is actually starting
            info!(
                version = arc_version::GIT_VERSION,
                commit = arc_version::GIT_COMMIT_HASH,
                "Arc Execution EL starting"
            );

            // Register version information in metrics
            arc_node_execution::metrics::register_version_info();

            let wait_for_payload = ext.wait_for_payload;
            let filter_pending_txs = compute_filter_pending_txs(&ext);
            let allow_unprotected_txs = ext.arc_rpc_allow_unprotected_txs;
            let max_response_body_size = builder.config().rpc.rpc_max_response_size_bytes();
            let max_batch_entries = ext.arc_rpc_max_batch_entries;
            let tx_relays = build_tx_relays(&ext)?;
            let tx_relay_timeout = ext.arc_tx_relays_timeout;
            let rebroadcast_interval =
                std::time::Duration::from_secs(ext.txpool_rebroadcast_interval);
            let handle = builder
                .node(ArcNode::new(
                    arc_rpc_cfg,
                    invalid_tx_list_cfg,
                    addresses_denylist_config,
                    payload_builder_deadline_ms,
                    wait_for_payload,
                    filter_pending_txs,
                    allow_unprotected_txs,
                    max_response_body_size,
                    max_batch_entries,
                    rebroadcast_interval,
                    tx_relays,
                    tx_relay_timeout,
                ))
                .launch_with_debug_capabilities()
                .await?;

            spawn_pprof_server(ext.pprof_addr.parse()?, ext.pprof_heap_prof);

            #[cfg(unix)]
            install_sigterm_handler(handle.node.add_ons_handle.engine_shutdown.clone());

            handle.node_exit_future.await
        },
    ) {
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}

/// Install a SIGTERM handler to gracefully shutdown the engine.
///
/// When SIGTERM is received, triggers engine shutdown so in-memory blocks are persisted
/// before the process exits. The main `node_exit_future` will complete when the engine
/// shuts down.
///
/// # Note
/// This is only available on Unix systems.
#[cfg(unix)]
fn install_sigterm_handler(engine_shutdown: reth_node_builder::rpc::EngineShutdown) {
    use tokio::signal::unix::{signal, SignalKind};
    use tokio::time::{timeout, Duration};

    match signal(SignalKind::terminate()) {
        Ok(mut sigterm) => {
            tokio::spawn(async move {
                if sigterm.recv().await.is_some() {
                    tracing::info!(target: "arc::node", "Received SIGTERM, shutting down engine...");

                    // A second SIGTERM during shutdown forces an immediate exit.
                    tokio::spawn(async move {
                        if sigterm.recv().await.is_some() {
                            tracing::warn!(target: "arc::node", "Received second SIGTERM, forcing exit");
                            std::process::exit(143);
                        }
                    });

                    if let Some(done_rx) = engine_shutdown.shutdown() {
                        match timeout(Duration::from_secs(30), done_rx).await {
                            Ok(Ok(_)) => {
                                tracing::info!(target: "arc::node", "Engine shutdown complete");
                            }
                            Ok(Err(err)) => {
                                tracing::error!(target: "arc::node", ?err, "Engine shutdown failed");
                            }
                            Err(_) => {
                                tracing::error!(
                                    target: "arc::node",
                                    "Engine shutdown timed out after 30s"
                                );
                            }
                        }
                    } else {
                        tracing::warn!(target: "arc::node", "Engine shutdown channel already closed");
                    }

                    // Exit with the conventional SIGTERM code (128 + 15).
                    std::process::exit(143);
                }
            });
        }
        Err(err) => {
            tracing::warn!(
                target: "arc::node",
                %err,
                "Failed to register SIGTERM handler; graceful engine shutdown on SIGTERM will not be available"
            );
        }
    }
}

#[cfg(not(unix))]
fn install_sigterm_handler(_engine_shutdown: reth_node_builder::rpc::EngineShutdown) {}

#[cfg(feature = "pprof")]
fn spawn_pprof_server(bind_address: std::net::SocketAddr, heap_prof: bool) {
    if heap_prof {
        // SAFETY: writing a bool to a well-known jemalloc mallctl key.
        if let Err(e) = unsafe { tikv_jemalloc_ctl::raw::write(b"prof.active\0", true) } {
            tracing::error!(error = %e, "failed to activate jemalloc heap profiling; /debug/pprof/allocs will return empty profiles");
        } else {
            tracing::info!("jemalloc heap profiling activated");
        }
    }

    tokio::spawn(async move {
        if let Err(e) =
            pprof_hyper_server::serve(bind_address, pprof_hyper_server::Config::default()).await
        {
            tracing::error!(
                error = %e,
                "pprof server failed to start"
            );
        }
    });
}

#[cfg(not(feature = "pprof"))]
fn spawn_pprof_server(_bind_address: std::net::SocketAddr, _heap_prof: bool) {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    /// Parse CLI args with `patch_node_command_defaults` applied (mirrors production).
    fn parse_with_arc_defaults<I>(argv: I) -> ArcCli
    where
        I: IntoIterator<Item = &'static str>,
    {
        let patched = patch_node_command_defaults(ArcCli::command());
        ArcCli::from_arg_matches(&patched.get_matches_from(argv)).unwrap()
    }

    #[test]
    fn test_extradata_default_is_allowed() {
        let cli = parse_with_arc_defaults(["arc-node-execution", "node"]);
        assert!(cli.validate().is_ok());
    }

    #[test]
    fn test_extradata_custom_is_rejected() {
        let cli = parse_with_arc_defaults([
            "arc-node-execution",
            "node",
            "--builder.extradata",
            "custom",
        ]);
        assert_eq!(cli.validate(), Err("--builder.extradata is not supported"));
    }

    #[test]
    fn test_validate_rejects_filter_with_pending_block_full() {
        let cli =
            parse_with_arc_defaults(["arc-node-execution", "node", "--rpc.pending-block=full"]);
        assert!(
            cli.validate()
                .unwrap_err()
                .contains("--rpc.pending-block must be 'none'"),
            "default filter + --rpc.pending-block=full must be rejected"
        );
    }

    #[test]
    fn test_validate_allows_expose_with_pending_block_full() {
        let cli = parse_with_arc_defaults([
            "arc-node-execution",
            "node",
            "--arc.expose-pending-txs",
            "--rpc.pending-block=full",
        ]);
        assert!(
            cli.validate().is_ok(),
            "--arc.expose-pending-txs + --rpc.pending-block=full must be allowed"
        );
    }

    #[test]
    fn test_validate_rejects_public_api_with_pending_block_full() {
        let cli = parse_with_arc_defaults([
            "arc-node-execution",
            "node",
            "--public-api",
            "--rpc.pending-block=full",
        ]);
        assert!(
            cli.validate()
                .unwrap_err()
                .contains("--rpc.pending-block must be 'none'"),
            "--public-api + --rpc.pending-block=full must be rejected"
        );
    }

    #[test]
    fn test_pending_block_default_is_none() {
        let patched = patch_node_command_defaults(ArcCli::command());
        let cli =
            ArcCli::from_arg_matches(&patched.get_matches_from(["arc-node-execution", "node"]))
                .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert_eq!(
                node_cmd.rpc.rpc_pending_block,
                reth_rpc_eth_types::builder::config::PendingBlockKind::None,
                "Arc default for --rpc.pending-block should be none"
            );
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_invalid_tx_list_flags_default_values() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(node_cmd.ext.invalid_tx_list_enable);
            assert_eq!(node_cmd.ext.invalid_tx_list_cap, 100_000);
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_invalid_tx_list_flag_explicit_disable() {
        let cli = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--invalid-tx-list-enable=false",
        ])
        .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(!node_cmd.ext.invalid_tx_list_enable);
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_invalid_tx_list_flags_custom_values() {
        let cli = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--invalid-tx-list-enable=false",
            "--invalid-tx-list-cap",
            "50000",
        ])
        .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(!node_cmd.ext.invalid_tx_list_enable);
            assert_eq!(node_cmd.ext.invalid_tx_list_cap, 50000);
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_invalid_tx_list_cap_invalid_value_rejected() {
        let result = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--invalid-tx-list-cap",
            "notanumber",
        ]);
        assert!(result.is_err_and(|err| err.to_string().contains("invalid value")));
    }

    #[test]
    fn test_invalid_tx_list_cap_overflow_rejected() {
        let result = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--invalid-tx-list-cap",
            &u128::MAX.to_string(),
        ]);
        assert!(result.is_err_and(|err| err.to_string().contains("invalid value")));
    }

    #[test]
    fn test_arc_rpc_max_batch_entries_default() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert_eq!(
                node_cmd.ext.arc_rpc_max_batch_entries,
                ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT
            );
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_arc_rpc_max_batch_entries_custom() {
        let cli = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--arc.rpc.max-batch-entries",
            "250",
        ])
        .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert_eq!(node_cmd.ext.arc_rpc_max_batch_entries, 250);
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_arc_rpc_max_batch_entries_zero_rejected() {
        let result = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--arc.rpc.max-batch-entries",
            "0",
        ]);
        assert!(
            result.is_err_and(|err| err.to_string().contains("must be >= 1")),
            "0 should be rejected with the must-be->=1 message"
        );
    }

    #[test]
    fn test_arc_builder_deadline_default_unset() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(node_cmd.ext.payload_builder_deadline_ms.is_none());
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_arc_builder_deadline_custom_value() {
        let cli = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--arc.builder.deadline",
            "900",
        ])
        .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert_eq!(node_cmd.ext.payload_builder_deadline_ms, Some(900));
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_wait_for_payload_default_is_true() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(node_cmd.ext.wait_for_payload);
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_wait_for_payload_disabled() {
        let cli = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--arc.builder.wait-for-payload=false",
        ])
        .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(!node_cmd.ext.wait_for_payload);
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_arc_rpc_allow_unprotected_txs_default_is_false() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(
                !node_cmd.ext.arc_rpc_allow_unprotected_txs,
                "default must reject pre-EIP-155 txs over RPC"
            );
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_arc_rpc_allow_unprotected_txs_explicit() {
        let cli = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--arc.rpc.allow-unprotected-txs",
        ])
        .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(node_cmd.ext.arc_rpc_allow_unprotected_txs);
        } else {
            panic!("Expected Node command");
        }
    }

    fn tx_relays_from_args(args: &[&str]) -> eyre::Result<Vec<String>> {
        let mut argv = vec!["arc-node-execution", "node"];
        argv.extend_from_slice(args);
        let cli = ArcCli::try_parse_from(argv).unwrap();
        match &cli.inner.command {
            Commands::Node(cmd) => build_tx_relays(&cmd.ext),
            _ => panic!("Expected Node command"),
        }
    }

    #[test]
    fn test_build_tx_relays_default_empty() {
        assert!(tx_relays_from_args(&[]).unwrap().is_empty());
    }

    #[test]
    fn test_build_tx_relays_parses_csv_in_order() {
        let relays =
            tx_relays_from_args(&["--arc.tx.relays", "http://a:8545,http://b:8545"]).unwrap();
        assert_eq!(
            relays,
            vec!["http://a:8545".to_string(), "http://b:8545".to_string()]
        );
    }

    #[test]
    fn test_build_tx_relays_trims_and_skips_blank_entries() {
        let relays = tx_relays_from_args(&["--arc.tx.relays", " http://a:8545 , "]).unwrap();
        assert_eq!(relays, vec!["http://a:8545".to_string()]);
    }

    #[test]
    fn test_build_tx_relays_rejects_invalid_url() {
        let err = tx_relays_from_args(&["--arc.tx.relays", "not-a-url"]).unwrap_err();
        assert!(err.to_string().contains("invalid --arc.tx.relays entry"));
    }

    #[test]
    fn test_tx_relays_conflicts_with_forwarder() {
        let result = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--arc.tx.relays",
            "http://a:8545",
            "--rpc.forwarder",
            "http://b:8545",
        ]);
        assert!(
            result.is_err(),
            "--arc.tx.relays must conflict with --rpc.forwarder"
        );
    }

    fn tx_relay_timeout_from_args(args: &[&str]) -> std::time::Duration {
        let mut argv = vec!["arc-node-execution", "node"];
        argv.extend_from_slice(args);
        let cli = ArcCli::try_parse_from(argv).unwrap();
        match &cli.inner.command {
            Commands::Node(cmd) => cmd.ext.arc_tx_relays_timeout,
            _ => panic!("Expected Node command"),
        }
    }

    #[test]
    fn test_tx_relay_timeout_default_matches_const() {
        assert_eq!(
            tx_relay_timeout_from_args(&[]),
            arc_evm_node::DEFAULT_TX_RELAY_TIMEOUT
        );
    }

    #[test]
    fn test_tx_relay_timeout_parses_suffixed_durations() {
        assert_eq!(
            tx_relay_timeout_from_args(&["--arc.tx.relays.timeout", "500ms"]),
            std::time::Duration::from_millis(500)
        );
        assert_eq!(
            tx_relay_timeout_from_args(&["--arc.tx.relays.timeout", "15s"]),
            std::time::Duration::from_secs(15)
        );
    }

    #[test]
    fn test_tx_relay_timeout_bare_number_is_seconds() {
        assert_eq!(
            tx_relay_timeout_from_args(&["--arc.tx.relays.timeout", "15"]),
            std::time::Duration::from_secs(15)
        );
    }

    fn ext_from_args<'a>(args: impl IntoIterator<Item = &'a str>) -> ArcExtraCli {
        let cli = ArcCli::try_parse_from(
            ["arc-node-execution", "node"]
                .into_iter()
                .chain(args)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        match cli.inner.command {
            Commands::Node(cmd) => cmd.ext,
            _ => panic!("Expected Node command"),
        }
    }

    /// The denylist must not be operator-configurable: disabling it or repointing it at a
    /// different list has to require a source change and rebuild, not a CLI flag.
    #[test]
    fn test_arc_denylist_configuration_flags_are_rejected() {
        for flag in [
            "--arc.denylist.enabled",
            "--arc.denylist.address",
            "--arc.denylist.storage-slot",
        ] {
            assert!(
                ArcCli::try_parse_from(["arc-node-execution", "node", flag]).is_err(),
                "{flag} must not be accepted"
            );
        }
    }

    #[test]
    fn test_arc_denylist_exclusions_flag_defaults_empty() {
        assert!(ext_from_args([])
            .arc_denylist_addresses_exclusions
            .is_empty());
    }

    #[test]
    fn test_build_addresses_denylist_config_uses_chain_spec_address() {
        use arc_execution_config::chainspec::{DEVNET, LOCAL_DEV, MAINNET, TESTNET};

        for spec in [&*LOCAL_DEV, &*DEVNET, &*TESTNET, &*MAINNET] {
            let cfg = build_addresses_denylist_config(spec, &ext_from_args([])).unwrap();
            assert_eq!(cfg.contract_address(), spec.denylist_address().unwrap());
            assert_eq!(cfg.storage_slot(), DEFAULT_DENYLIST_ERC7201_BASE_SLOT);
            assert!(cfg.addresses_exclusions().is_empty());
        }
    }

    #[test]
    fn test_build_addresses_denylist_config_applies_exclusions() {
        use arc_execution_config::chainspec::MAINNET;

        let ext = ext_from_args([
            "--arc.denylist.addresses-exclusions",
            "0x3600000000000000000000000000000000000001,0x3600000000000000000000000000000000000002",
        ]);
        let cfg = build_addresses_denylist_config(&MAINNET, &ext).unwrap();

        // Exclusions are ops recovery only; they must not move the contract address.
        assert_eq!(cfg.contract_address(), MAINNET.denylist_address().unwrap());
        assert_eq!(
            cfg.addresses_exclusions(),
            &[
                address!("0x3600000000000000000000000000000000000001"),
                address!("0x3600000000000000000000000000000000000002"),
            ]
        );
    }

    #[test]
    fn test_build_addresses_denylist_config_rejects_unknown_chain() {
        use alloy_genesis::Genesis;
        use arc_execution_config::chainspec::ArcChainSpec;
        use reth_chainspec::ChainSpec;

        let mut genesis: Genesis =
            serde_json::from_str(include_str!("../../../assets/localdev/genesis.json")).unwrap();
        genesis.config.chain_id = 999999;
        let spec = ArcChainSpec::new(ChainSpec::from_genesis(genesis));

        let err = build_addresses_denylist_config(&spec, &ext_from_args([])).unwrap_err();
        assert!(
            err.to_string()
                .contains("no Arc denylist is defined for chain id 999999"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_build_addresses_denylist_config_invalid_exclusion_rejected() {
        use arc_execution_config::chainspec::MAINNET;

        let ext = ext_from_args(["--arc.denylist.addresses-exclusions", "not-an-address"]);
        let err = build_addresses_denylist_config(&MAINNET, &ext).unwrap_err();
        assert!(err
            .to_string()
            .contains("invalid --arc.denylist.addresses-exclusions"));
    }

    #[test]
    fn test_arc_expose_pending_txs_default_is_false() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(
                !node_cmd.ext.arc_expose_pending_txs,
                "Default: --arc.expose-pending-txs should be false (pending txs hidden by default)"
            );
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_arc_expose_pending_txs_when_set() {
        let cli =
            ArcCli::try_parse_from(["arc-node-execution", "node", "--arc.expose-pending-txs"])
                .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(
                node_cmd.ext.arc_expose_pending_txs,
                "--arc.expose-pending-txs should flip the flag"
            );
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_public_api_default_is_false() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(
                !node_cmd.ext.public_api,
                "Default: --public-api should be false"
            );
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_public_api_when_set() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node", "--public-api"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(node_cmd.ext.public_api, "--public-api should flip the flag");
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_public_api_conflicts_with_expose_pending_txs() {
        use clap::error::ErrorKind;

        let err = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--public-api",
            "--arc.expose-pending-txs",
        ])
        .unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::ArgumentConflict,
            "clap should reject --public-api + --arc.expose-pending-txs as a conflict"
        );
    }

    #[test]
    fn test_public_api_enables_filter() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node", "--public-api"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(
                compute_filter_pending_txs(&node_cmd.ext),
                "--public-api alone must enable the filter"
            );
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_compute_filter_pending_txs_default_hides() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(
                compute_filter_pending_txs(&node_cmd.ext),
                "default config must keep the filter on"
            );
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_compute_filter_pending_txs_expose_disables() {
        let cli =
            ArcCli::try_parse_from(["arc-node-execution", "node", "--arc.expose-pending-txs"])
                .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(
                !compute_filter_pending_txs(&node_cmd.ext),
                "--arc.expose-pending-txs must disable the filter"
            );
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_pprof_heap_prof_default_is_false() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(!node_cmd.ext.pprof_heap_prof);
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_pprof_heap_prof_when_set() {
        let cli =
            ArcCli::try_parse_from(["arc-node-execution", "node", "--pprof.heap-prof"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert!(node_cmd.ext.pprof_heap_prof);
        } else {
            panic!("Expected Node command");
        }
    }

    /// --full gets --prune.block-interval=128 injected.
    #[test]
    fn test_full_preset_argv_translation() {
        let argv = init_arc_pruning(["arc-node", "node", "--full"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(
            translated.contains(&"--full".to_owned()),
            "must retain --full"
        );
        assert!(
            translated.iter().any(|s| s == FLAG_BLOCK_INTERVAL),
            "must inject --prune.block-interval"
        );
    }

    /// --minimal gets --prune.block-interval=128 injected.
    #[test]
    fn test_minimal_preset_argv_translation() {
        let argv = init_arc_pruning(["arc-node", "node", "--minimal"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(
            translated.contains(&"--minimal".to_owned()),
            "must retain --minimal"
        );
        assert!(
            translated.iter().any(|s| s == FLAG_BLOCK_INTERVAL),
            "must inject --prune.block-interval"
        );
    }

    /// Explicit --prune.block-interval overrides the injected default.
    #[test]
    fn test_full_preset_explicit_block_interval_overrides() {
        let argv = init_arc_pruning(["arc-node", "node", "--full", "--prune.block-interval=1000"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(
            translated.contains(&"--full".to_owned()),
            "must retain --full"
        );
        assert!(
            translated.contains(&"--prune.block-interval=1000".to_owned()),
            "must keep user-supplied block interval"
        );
        assert!(
            !translated.contains(&FLAG_BLOCK_INTERVAL.to_owned()),
            "must not inject default block interval when user supplied one"
        );
    }

    /// `download --full` must not get --prune.block-interval injected; the flag
    /// is a restore profile there, not a node pruning preset.
    #[test]
    fn test_download_full_does_not_inject_block_interval() {
        let argv = init_arc_pruning(["arc-node", "download", "--full"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(
            translated.contains(&"--full".to_owned()),
            "must retain --full"
        );
        assert!(
            !translated.iter().any(|s| s == FLAG_BLOCK_INTERVAL),
            "must not inject --prune.block-interval on download"
        );
    }

    /// `download --minimal` must not get --prune.block-interval injected either.
    #[test]
    fn test_download_minimal_does_not_inject_block_interval() {
        let argv = init_arc_pruning(["arc-node", "download", "--minimal"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(
            translated.contains(&"--minimal".to_owned()),
            "must retain --minimal"
        );
        assert!(
            !translated.iter().any(|s| s == FLAG_BLOCK_INTERVAL),
            "must not inject --prune.block-interval on download"
        );
    }

    /// Unrelated args are passed through and --datadir is injected.
    #[test]
    fn test_arc_pruning_init_injects_datadir() {
        let argv = init_arc_pruning(["arc-node", "node", "--http"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(translated.contains(&"arc-node".to_owned()));
        assert!(translated.contains(&"--http".to_owned()));
        assert!(
            translated.iter().any(|s| s.starts_with("--datadir=")),
            "must inject --datadir"
        );
        assert!(
            translated.iter().any(|s| s.contains(".arc/execution")),
            "--datadir must point to ~/.arc/execution"
        );
    }

    /// Explicit --datadir is not overridden.
    #[test]
    fn test_arc_pruning_explicit_datadir_not_overridden() {
        let argv = init_arc_pruning(["arc-node", "node", "--datadir=/custom/path"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(translated.contains(&"--datadir=/custom/path".to_owned()));
        assert_eq!(
            translated
                .iter()
                .filter(|s| s.starts_with("--datadir"))
                .count(),
            1,
            "must not inject a second --datadir"
        );
    }

    /// Subcommands that don't accept --datadir must not receive the injected flag.
    #[test]
    fn test_arc_pruning_no_datadir_for_p2p() {
        let argv = init_arc_pruning(["arc-node", "p2p"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(
            !translated.iter().any(|s| s.starts_with("--datadir")),
            "p2p must not receive --datadir"
        );
    }

    #[test]
    fn test_arc_pruning_no_datadir_for_config() {
        let argv = init_arc_pruning(["arc-node", "config"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(
            !translated.iter().any(|s| s.starts_with("--datadir")),
            "config must not receive --datadir"
        );
    }

    #[test]
    fn test_arc_pruning_no_datadir_for_dump_genesis() {
        let argv = init_arc_pruning(["arc-node", "dump-genesis"]);
        let translated: Vec<_> = argv
            .iter()
            .map(|s| s.to_str().unwrap().to_owned())
            .collect();
        assert!(
            !translated.iter().any(|s| s.starts_with("--datadir")),
            "dump-genesis must not receive --datadir"
        );
    }

    #[test]
    fn test_txpool_rebroadcast_interval_default() {
        let cli = ArcCli::try_parse_from(["arc-node-execution", "node"]).unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert_eq!(node_cmd.ext.txpool_rebroadcast_interval, 60);
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_txpool_rebroadcast_interval_custom() {
        let cli = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--txpool.rebroadcast-interval",
            "120",
        ])
        .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert_eq!(node_cmd.ext.txpool_rebroadcast_interval, 120);
        } else {
            panic!("Expected Node command");
        }
    }

    #[test]
    fn test_txpool_rebroadcast_interval_zero_disables() {
        let cli = ArcCli::try_parse_from([
            "arc-node-execution",
            "node",
            "--txpool.rebroadcast-interval",
            "0",
        ])
        .unwrap();
        if let Commands::Node(node_cmd) = cli.inner.command {
            assert_eq!(node_cmd.ext.txpool_rebroadcast_interval, 0);
        } else {
            panic!("Expected Node command");
        }
    }
}

#[cfg(test)]
mod public_api_tests {
    use super::*;

    #[test]
    fn unsafe_modules_empty_when_selection_is_subset_of_safe() {
        let sel = RpcModuleSelection::try_from_selection(["eth", "net", "web3", "rpc"]).unwrap();
        let unsafe_ = unsafe_public_api_modules(&sel);
        assert!(unsafe_.is_empty(), "eth/net/web3/rpc are all safe");
    }

    #[test]
    fn unsafe_modules_lists_sensitive_namespaces() {
        let sel = RpcModuleSelection::try_from_selection([
            "eth", "net", "web3", "txpool", "debug", "trace", "admin",
        ])
        .unwrap();
        let unsafe_: HashSet<_> = unsafe_public_api_modules(&sel).into_iter().collect();
        assert_eq!(
            unsafe_,
            HashSet::from([
                RethRpcModule::Txpool,
                RethRpcModule::Debug,
                RethRpcModule::Trace,
                RethRpcModule::Admin,
            ])
        );
    }

    #[test]
    fn unsafe_modules_treats_other_as_unsafe() {
        let sel = RpcModuleSelection::try_from_selection(["eth", "custom"]).unwrap();
        let unsafe_ = unsafe_public_api_modules(&sel);
        assert_eq!(unsafe_.len(), 1);
        assert!(matches!(unsafe_[0], RethRpcModule::Other(_)));
    }

    #[test]
    fn unsafe_modules_handles_all_selection() {
        let sel = RpcModuleSelection::All;
        let unsafe_ = unsafe_public_api_modules(&sel);
        assert!(!unsafe_.is_empty());
        assert!(!unsafe_.contains(&RethRpcModule::Eth));
        assert!(!unsafe_.contains(&RethRpcModule::Rpc));
    }

    #[tracing_test::traced_test]
    #[test]
    fn warn_if_public_api_unsafe_none_is_silent() {
        warn_if_public_api_unsafe(None, "--http.api");
        assert!(!logs_contain("sensitive namespaces"));
    }

    #[tracing_test::traced_test]
    #[test]
    fn warn_if_public_api_unsafe_safe_selection_is_silent() {
        let sel = RpcModuleSelection::try_from_selection(["eth", "net", "web3"]).unwrap();
        warn_if_public_api_unsafe(Some(&sel), "--http.api");
        assert!(!logs_contain("sensitive namespaces"));
    }

    #[tracing_test::traced_test]
    #[test]
    fn warn_if_public_api_unsafe_unsafe_selection_warns() {
        let sel = RpcModuleSelection::try_from_selection(["eth", "txpool", "debug"]).unwrap();
        warn_if_public_api_unsafe(Some(&sel), "--ws.api");
        assert!(logs_contain("sensitive namespaces"));
        assert!(logs_contain("--ws.api"));
        assert!(logs_contain("txpool"));
        assert!(logs_contain("debug"));
    }
}
