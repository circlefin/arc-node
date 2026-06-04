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

//! Integration test runner for real in-process Arc nodes.
//!
//! Unlike the mock runner (see `arc_test_framework::runner::mock` for the
//! shared handle; runners are defined in test files) which tests the
//! framework harness with synthetic events, `ArcNodeRunner` exercises the
//! **full Arc stack**: real EVM execution, BFT consensus, Engine API wiring,
//! IPC socket timing, the `TxEvent<ArcContext>` → `ArcEvent` event bridge,
//! and libp2p peer discovery.
//!
//! Each node consists of:
//! - An execution layer ([`ArcNode`] via Reth) providing the EVM and block storage
//! - A consensus layer (Malachite [`App`]) providing BFT consensus
//! - An Engine API connection between them (IPC sockets)
//!
//! Consensus nodes communicate via real libp2p P2P networking.
//!
//! ## Engine API connection
//!
//! The consensus layer connects to the execution layer via IPC (Unix domain
//! sockets: `reth.ipc` and `auth.ipc`). No JWT authentication is needed for
//! IPC mode.
//!
//! ## Spawn sequence (per node)
//!
//! 1. Create temp directory for node data (execution DB, consensus DB, keys)
//! 2. Generate ed25519 keys (pre-computed at runner creation)
//! 3. Start Reth `ArcNode` with IPC sockets in the temp dir
//! 4. Start Malachite `App` with `StartConfig` pointing to the IPC sockets
//! 5. Bridge consensus events (`TxEvent<ArcContext>`) into the unified `ArcEvent` stream
//! 6. Return `ArcNodeHandle` wrapping both layer handles
//!
//! ## Port allocation
//!
//! Test IDs are derived from `(pid + seq) % 65536` where `pid` is the OS
//! process ID and `seq` is a per-process atomic counter. This gives
//! cross-process isolation for `cargo nextest` (different PIDs) and
//! intra-process isolation for `cargo test` (different `seq` values).
//!
//! Each node N in test T gets:
//! - Consensus P2P: `26000 + T * 100 + N * 10`
//! - Consensus RPC: `31000 + T * 100 + N * 10`
//! - Reth ports: default + `T * 100 + N * 10` offset
//! - Execution IPC: `<temp_dir>/reth.ipc` and `<temp_dir>/auth.ipc`

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{address, Address};
use async_trait::async_trait;
use eyre::WrapErr;
use tempfile::TempDir;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use arc_consensus_types::{ArcContext, Config as ConsensusConfig};
use arc_evm_node::node::{ArcNode, ArcRpcConfig};
use arc_evm_node::ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT;
use arc_execution_config::addresses_denylist::AddressesDenylistConfig;
use arc_execution_config::chainspec::{localdev_with_block_gas_limit, ArcChainSpec, LOCAL_DEV};
use arc_execution_txpool::InvalidTxListConfig;
use arc_node_consensus::hardcoded_config::{
    build_consensus_config, build_value_sync_config, GossipSubOverrides,
};
use arc_node_consensus::node::{App, Handle as ConsensusHandle, StartConfig};
use arc_node_consensus::store::Store as ConsensusStore;
use arc_node_consensus_cli::file::save_priv_validator_key;
use arc_node_consensus_cli::new::generate_private_keys;
use arc_signer::local::PrivateKey;
use arc_test_framework::events::ArcEvent;
use arc_test_framework::node::{Layer, TestNodeConfig};
use arc_test_framework::params::TestParams;
use arc_test_framework::{NodeHandle, NodeId, NodeRunner};
use malachitebft_app_channel::app::consensus::Multiaddr;
use malachitebft_app_channel::EngineHandle;
use reth_node_builder::{NodeBuilder, NodeConfig};
use reth_tasks::TaskExecutor;

use crate::bridge::spawn_event_bridge;

const P2P_BASE_PORT: usize = 26000;
const RPC_BASE_PORT: usize = 31000;
// IANA dynamic/private ports start at 49152, so 49151 is the last non-ephemeral port.
const MAX_NON_EPHEMERAL_PORT: usize = 49151;

/// Maximum `test_id` that keeps all computed ports below the OS ephemeral range.
///
/// Dynamic/private ports (49152+) are used for outbound client sockets; binding
/// test listeners there can race with those ephemeral allocations and cause
/// intermittent `EADDRINUSE` on startup.
///
/// Tightest bound comes from consensus RPC:
/// `RPC_BASE_PORT + id * 100 + 9 * 10 <= MAX_NON_EPHEMERAL_PORT`.
const MAX_TEST_ID: usize = (MAX_NON_EPHEMERAL_PORT - RPC_BASE_PORT - 90) / 100;

/// Per-node key material and addresses, computed at runner creation.
#[derive(Clone)]
struct NodeKeyMaterial {
    private_key: PrivateKey,
    listen_addr: Multiaddr,
}

/// IPC socket paths for Engine API communication between EL and CL.
#[derive(Clone)]
struct IpcPaths {
    reth: PathBuf,
    auth: PathBuf,
}

impl IpcPaths {
    fn new(base_dir: &std::path::Path) -> Self {
        Self {
            reth: base_dir.join("reth.ipc"),
            auth: base_dir.join("auth.ipc"),
        }
    }
}
/// Consensus-layer state, taken as a unit during crash or shutdown.
struct ClState {
    engine: EngineHandle,
    store: ConsensusStore,
    app_task: JoinHandle<eyre::Result<()>>,
    rpc_task: Option<JoinHandle<()>>,
    store_monitor_task: JoinHandle<()>,
    bridge_task: JoinHandle<()>,
}

/// Execution-layer state, taken as a unit during crash or shutdown.
struct ElState {
    executor: TaskExecutor,
    task: JoinHandle<()>,
}

/// Handle to a running Arc node (both execution and consensus layers).
///
/// Each layer's mutable state is grouped into a single struct behind one
/// `Mutex<Option<_>>` so that crash/shutdown can take ownership in a single
/// lock acquisition.
///
/// Implements both crash and graceful shutdown from [`NodeHandle`]:
/// - **Crash** (`kill_cl`/`kill_el`): hard abort — tasks are cancelled
///   immediately, modelling an abrupt process death. Resources (ports, file
///   locks) may not be released instantly.
/// - **Graceful** (`shutdown_cl`/`shutdown_el`): cooperative shutdown that
///   waits for the engine, store, and task executor to release OS resources
///   before returning. Used by `restart` to ensure ports and file
///   locks are free before respawning.
pub struct ArcNodeHandle {
    node_id: NodeId,
    temp_dir: TempDir,
    ipc: IpcPaths,

    tx: broadcast::Sender<ArcEvent>,
    cancel_token: CancellationToken,
    cl: Mutex<Option<ClState>>,
    el: Mutex<Option<ElState>>,
}

impl Drop for ArcNodeHandle {
    fn drop(&mut self) {
        self.cancel_token.cancel();

        // Best-effort abort: try_lock succeeds when no async shutdown is in
        // progress (the common case).  If the lock is held, the ongoing
        // shutdown already owns cleanup.
        if let Ok(mut guard) = self.cl.try_lock() {
            if let Some(cl) = guard.take() {
                cl.app_task.abort();
                if let Some(rpc) = cl.rpc_task {
                    rpc.abort();
                }
                cl.store_monitor_task.abort();
                cl.bridge_task.abort();
            }
        }
        if let Ok(mut guard) = self.el.try_lock() {
            if let Some(el) = guard.take() {
                el.task.abort();
            }
        }
    }
}

#[async_trait]
impl NodeHandle for ArcNodeHandle {
    fn subscribe(&self) -> broadcast::Receiver<ArcEvent> {
        self.tx.subscribe()
    }

    async fn kill_cl(&self) -> eyre::Result<()> {
        self.cancel_token.cancel();

        let Some(cl) = self.cl.lock().await.take() else {
            return Ok(());
        };

        stop_cl_engine(&cl.engine, "kill_cl").await;

        cl.app_task.abort();
        if let Some(rpc) = cl.rpc_task {
            rpc.abort();
        }
        cl.store_monitor_task.abort();
        cl.bridge_task.abort();

        // Drop the store on a blocking thread to avoid blocking the async
        // runtime (database closing can be slow). Await the handle so the WAL
        // file lock is released before any subsequent `spawn_cl` call.
        tokio::task::spawn_blocking(move || drop(cl.store))
            .await
            .map_err(|e| eyre::eyre!("kill_cl: store drop task failed: {e}"))?;

        Ok(())
    }

    async fn kill_el(&self) -> eyre::Result<()> {
        let Some(el) = self.el.lock().await.take() else {
            return Ok(());
        };

        // Reth spawns internal tasks (listeners, discovery, etc.) via
        // TaskExecutor. Aborting the top-level JoinHandle does not cancel those
        // children. Only stopping the executor does. This is not a graceful
        // application shutdown; it is the mechanism for tearing down Reth's
        // internal task tree so OS resources (ports) are released.
        el.executor
            .graceful_shutdown_with_timeout(Duration::from_secs(5));
        el.task.abort();
        match el.task.await {
            Ok(()) => Ok(()),
            Err(e) if e.is_cancelled() => Ok(()),
            Err(e) => Err(eyre::eyre!("kill_el: execution task failed to join: {e}")),
        }
    }

    async fn shutdown_cl(&self) -> eyre::Result<()> {
        self.cancel_token.cancel();

        let Some(cl) = self.cl.lock().await.take() else {
            return Ok(());
        };

        stop_cl_engine(&cl.engine, "shutdown_cl").await;

        // Drop the store on a blocking thread to avoid blocking the async
        // runtime (database closing can be slow), same as kill_cl.
        let store = cl.store;
        tokio::task::spawn_blocking(move || {
            store.savepoint();
            drop(store);
        })
        .await
        .map_err(|e| eyre::eyre!("shutdown_cl: store drop task failed: {e}"))?;

        // Always abort auxiliary tasks first so they release OS resources
        // (ports, channels) regardless of whether the app task exited
        // cleanly. Without this, a failed app_task (e.g. broken IPC pipe
        // after an EL crash) would cause an early return, leaving the RPC
        // listener and bridge task orphaned and holding resources that the
        // next spawn needs.
        abort_and_await(cl.rpc_task, "consensus rpc task").await?;
        abort_and_await(Some(cl.store_monitor_task), "consensus store monitor task").await?;
        abort_and_await(Some(cl.bridge_task), "consensus event bridge task").await?;

        await_or_abort(cl.app_task, Duration::from_secs(5), "consensus app task").await?;
        Ok(())
    }

    async fn shutdown_el(&self) -> eyre::Result<()> {
        let Some(el) = self.el.lock().await.take() else {
            return Ok(());
        };

        el.executor
            .graceful_shutdown_with_timeout(Duration::from_secs(5));
        abort_and_await(Some(el.task), "execution task").await?;
        Ok(())
    }
}

async fn abort_and_await<T>(handle: Option<JoinHandle<T>>, task_name: &str) -> eyre::Result<()> {
    if let Some(h) = handle {
        h.abort();
        match h.await {
            Ok(_) => {}
            Err(e) if e.is_cancelled() => {}
            Err(e) => {
                return Err(eyre::eyre!("{task_name} failed to join after abort: {e}"));
            }
        }
    }
    Ok(())
}

/// Wait for a task to finish within a timeout, then force-abort if it hasn't.
async fn await_or_abort(
    handle: JoinHandle<eyre::Result<()>>,
    timeout: Duration,
    task_name: &str,
) -> eyre::Result<()> {
    tokio::pin!(handle);
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(joined) => {
            let result =
                joined.map_err(|e| eyre::eyre!("{task_name} failed to join cleanly: {e}"))?;
            result.wrap_err_with(|| format!("{task_name} exited with error"))?;
            Ok(())
        }
        Err(_) => {
            handle.abort();
            match handle.await {
                Ok(result) => {
                    result.wrap_err_with(|| {
                        format!("{task_name} exited with error while forcing abort")
                    })?;
                    Ok(())
                }
                Err(e) if e.is_cancelled() => Ok(()),
                Err(e) => Err(eyre::eyre!(
                    "{task_name} failed to join after timeout/abort: {e}"
                )),
            }
        }
    }
}

/// Stop the consensus engine actor, releasing the WAL file lock.
///
/// The engine actor owns the WAL thread (a real OS thread, not a tokio task).
/// `stop_and_wait` releases its exclusive advisory lock on the WAL file;
/// without it a subsequent `spawn_cl` fails with "already locked".
/// In production SIGKILL releases all file locks at process exit; in-process
/// we don't have that luxury.
///
/// Errors and timeouts are logged but not propagated — the engine may have
/// already exited (e.g. broken IPC pipe after an EL crash).
async fn stop_cl_engine(engine: &EngineHandle, caller: &str) {
    match tokio::time::timeout(
        Duration::from_secs(2),
        engine
            .actor
            .stop_and_wait(Some(format!("test {caller}")), None),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => error!(caller, error = %e, "stop_and_wait failed (non-fatal)"),
        Err(_) => error!(caller, "stop_and_wait timed out (non-fatal)"),
    }
}

/// Integration runner that spawns real in-process Arc nodes.
///
/// Implements [`NodeRunner`] by creating both execution and consensus layers
/// for each node, wiring them via Engine API (IPC).
#[derive(Clone)]
pub struct ArcNodeRunner {
    test_id: usize,
    test_configs: Arc<Vec<TestNodeConfig>>,
    params: TestParams,
    keys: Arc<Vec<NodeKeyMaterial>>,
    peer_addrs: Arc<Vec<Multiaddr>>,
}

#[async_trait]
impl NodeRunner for ArcNodeRunner {
    type Handle = ArcNodeHandle;

    fn new(test_id: usize, nodes: &[TestNodeConfig], params: TestParams) -> Self {
        let test_id = test_id % (MAX_TEST_ID + 1);
        let num_nodes = nodes.len();
        assert!(
            num_nodes <= 10,
            "ArcNodeRunner supports at most 10 nodes per test (got {num_nodes})"
        );
        let keys: Vec<NodeKeyMaterial> = generate_private_keys(num_nodes, true)
            .expect("failed to generate private keys for test nodes")
            .into_iter()
            .enumerate()
            .map(|(id, private_key)| {
                let port = P2P_BASE_PORT + test_id * 100 + id * 10;
                let listen_addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/{port}")
                    .parse()
                    .expect("invalid multiaddr");
                NodeKeyMaterial {
                    private_key,
                    listen_addr,
                }
            })
            .collect();

        let peer_addrs: Vec<Multiaddr> = keys.iter().map(|k| k.listen_addr.clone()).collect();

        Self {
            test_id,
            test_configs: Arc::new(nodes.to_vec()),
            params,
            keys: Arc::new(keys),
            peer_addrs: Arc::new(peer_addrs),
        }
    }

    async fn spawn(&self, id: NodeId) -> eyre::Result<Self::Handle> {
        info!(%self.test_id, node_id = %id, num_nodes = self.test_configs.len(), "Spawning Arc node");

        let temp_dir = TempDir::new().wrap_err("failed to create temp dir")?;
        let ipc = IpcPaths::new(temp_dir.path());
        let (tx, _) = broadcast::channel::<ArcEvent>(256);

        let mut handle = ArcNodeHandle {
            node_id: id,
            temp_dir,
            ipc,
            tx,
            cancel_token: CancellationToken::new(),
            cl: Mutex::new(None),
            el: Mutex::new(None),
        };

        self.spawn_el(&mut handle).await?;
        self.spawn_cl(&mut handle).await?;

        info!(%self.test_id, node_id = %id, "Arc node spawned");
        Ok(handle)
    }

    async fn restart(
        &self,
        id: NodeId,
        mut handle: Self::Handle,
        layer: Layer,
    ) -> eyre::Result<Self::Handle> {
        // Use graceful shutdown to ensure OS resources (ports, file locks)
        // are released before respawning. If a prior Crash step already
        // hard-aborted the layer, the state is None and these are no-ops.
        match layer {
            Layer::Both => {
                handle.shutdown_cl().await?;
                handle.shutdown_el().await?;
                drop(handle);
                self.spawn(id).await
            }
            Layer::Consensus => {
                handle.shutdown_cl().await?;
                self.spawn_cl(&mut handle).await?;
                Ok(handle)
            }
            Layer::Execution => {
                // Reth's test infrastructure uses an ephemeral in-memory
                // database, so the EL cannot be restarted in isolation: the new
                // EL starts from genesis while the CL expects the previously
                // committed blocks. Additionally, the CL's IPC connection
                // breaks when the EL dies, leaving the CL in an error state.
                //
                // To work around both issues we do a full respawn (same as
                // Layer::Both). The CL shutdown may fail because the app task
                // already exited with an IPC error; that is expected.
                if let Err(e) = handle.shutdown_cl().await {
                    info!(%id, error = %e, "CL shutdown error during EL restart (expected after EL crash)");
                }
                handle.shutdown_el().await?;
                drop(handle);
                self.spawn(id).await
            }
        }
    }
}

impl ArcNodeRunner {
    /// Spawn the consensus layer.
    async fn spawn_cl(&self, handle: &mut ArcNodeHandle) -> eyre::Result<()> {
        let node_id = handle.node_id;
        let idx = node_id.as_usize();
        let key_material = &self.keys[idx];
        let test_cfg = &self.test_configs[idx];
        let validator = test_cfg.voting_power > 0;

        // Build config, save key, spawn consensus layer
        let consensus_config = build_node_consensus_config(
            key_material,
            self.test_id,
            node_id,
            &self.peer_addrs,
            true,
        )?;
        let key_file = save_validator_key(handle.temp_dir.path(), &key_material.private_key)?;
        let consensus_handle = spawn_consensus_layer(
            handle.temp_dir.path(),
            &handle.ipc,
            consensus_config,
            key_file,
            validator,
        )
        .await?;

        // Keep one event bus for the handle lifetime so EL failures and
        // consensus events stay visible to the same subscribers.
        let bridge_task = spawn_consensus_event_bridge(handle, consensus_handle.tx_event);

        // Update handle with new consensus layer
        handle.cancel_token = consensus_handle.cancel_token;
        *handle.cl.lock().await = Some(ClState {
            engine: consensus_handle.engine,
            store: consensus_handle.store,
            app_task: consensus_handle.app,
            rpc_task: consensus_handle.rpc,
            store_monitor_task: consensus_handle.store_monitor,
            bridge_task,
        });

        info!(%self.test_id, %node_id, "Consensus layer started");
        Ok(())
    }

    /// Spawn the execution layer.
    async fn spawn_el(&self, handle: &mut ArcNodeHandle) -> eyre::Result<()> {
        // Remove stale IPC sockets from a previous run, or Reth will fail to bind on restart.
        let _ = std::fs::remove_file(&handle.ipc.reth);
        let _ = std::fs::remove_file(&handle.ipc.auth);

        let node_id = handle.node_id;
        let chain_spec = match self.params.block_gas_limit {
            Some(limit) => localdev_with_block_gas_limit(limit),
            None => LOCAL_DEV.clone(),
        };
        let node_config =
            build_node_execution_config(self.test_id, node_id, &handle.ipc, chain_spec)?;
        let (reth_executor, reth_task) =
            spawn_execution_layer(node_config, handle.tx.clone()).await?;

        *handle.el.lock().await = Some(ElState {
            executor: reth_executor,
            task: reth_task,
        });

        info!(%self.test_id, %node_id, "Execution layer started");
        Ok(())
    }
}

/// Save the validator private key to `<base_dir>/config/priv_validator_key.json`.
fn save_validator_key(
    base_dir: &std::path::Path,
    private_key: &PrivateKey,
) -> eyre::Result<PathBuf> {
    let config_dir = base_dir.join("config");
    std::fs::create_dir_all(&config_dir).wrap_err("failed to create config dir")?;
    let key_file = config_dir.join("priv_validator_key.json");
    save_priv_validator_key(&key_file, private_key)
        .map_err(|e| eyre::eyre!("failed to save private key: {e}"))?;
    Ok(key_file)
}

/// Build the [`ConsensusConfig`] for a single test node.
fn build_node_consensus_config(
    key_material: &NodeKeyMaterial,
    test_id: usize,
    node_id: NodeId,
    peer_addrs: &[Multiaddr],
    consensus_enabled: bool,
) -> eyre::Result<ConsensusConfig> {
    let rpc_port = RPC_BASE_PORT + test_id * 100 + node_id.as_usize() * 10;
    let rpc_listen_addr = format!("127.0.0.1:{rpc_port}")
        .parse()
        .wrap_err("invalid consensus RPC listen address")?;

    let persistent_peers: Vec<Multiaddr> = peer_addrs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != node_id.as_usize())
        .map(|(_, addr)| addr.clone())
        .collect();

    let mut config = ConsensusConfig {
        moniker: format!("node-{node_id}"),
        consensus: build_consensus_config(
            key_material.listen_addr.clone(),
            persistent_peers,
            false,
            false,
            20,
            20,
            consensus_enabled,
            GossipSubOverrides::default(),
        ),
        // Restart scenarios can bring a node back behind the network tip.
        // Keep value sync enabled so the restarted node can catch up.
        value_sync: build_value_sync_config(true),
        rpc: arc_consensus_types::RpcConfig {
            enabled: true,
            listen_addr: rpc_listen_addr,
        },
        ..Default::default()
    };
    // A restarted node can miss the one-shot peer status sent at reconnect and
    // otherwise resume consensus from a stale height. Periodic status rebroadcasts
    // let value sync discover the current peer tip and continue catching up.
    config.value_sync.status_update_interval = Duration::from_millis(250);
    Ok(config)
}

fn spawn_consensus_event_bridge(
    handle: &ArcNodeHandle,
    tx_event: malachitebft_app_channel::app::engine::util::events::TxEvent<ArcContext>,
) -> JoinHandle<()> {
    spawn_event_bridge(tx_event, handle.tx.clone())
}

/// Matches `QUAKE_DEFAULT_FEE_RECIPIENT` in `crates/quake/src/setup.rs` and
/// `LOCALDEV_FEE_RECIPIENT` in `tests/helpers/networks/localdev.ts`. Validators
/// must supply a non-zero fee recipient or Reth builds invalid blocks.
const LOCALDEV_FEE_RECIPIENT: Address = address!("0x65E0a200006D4FF91bD59F9694220dafc49dbBC1");

/// Start the Malachite consensus layer, returning its [`ConsensusHandle`].
async fn spawn_consensus_layer(
    base_dir: &std::path::Path,
    ipc: &IpcPaths,
    consensus_config: ConsensusConfig,
    key_file: PathBuf,
    validator: bool,
) -> eyre::Result<ConsensusHandle> {
    let start_config = StartConfig {
        eth_socket: Some(ipc.reth.to_string_lossy().to_string()),
        execution_socket: Some(ipc.auth.to_string_lossy().to_string()),
        persistent_peers: Vec::new(),
        persistent_peers_only: false,
        eth_rpc_endpoint: None,
        execution_endpoint: None,
        execution_jwt: None,
        pprof_bind_address: None,
        pprof_heap_prof: false,
        suggested_fee_recipient: validator.then(|| LOCALDEV_FEE_RECIPIENT.into()),
        skip_db_upgrade: false,
        validator,
        rpc_sync_enabled: false,
        rpc_sync_endpoints: Vec::new(),
        gossipsub_overrides: GossipSubOverrides::default(),
        execution_ws_endpoint: None,
    };

    let home_dir: PathBuf = base_dir.to_path_buf();
    let mut app = App::new(consensus_config, home_dir, key_file, start_config);

    app.start().await.wrap_err("failed to start consensus app")
}

/// Build the Reth [`NodeConfig`] for a single test node with offset ports and IPC paths.
fn build_node_execution_config(
    test_id: usize,
    node_id: NodeId,
    ipc: &IpcPaths,
    chain_spec: Arc<ArcChainSpec>,
) -> eyre::Result<NodeConfig<ArcChainSpec>> {
    let mut config: NodeConfig<ArcChainSpec> = NodeConfig::new(chain_spec).set_dev(true);
    // The in-process test harness runs several Reth nodes in one runtime.
    // Use the synchronous state-root path to avoid debug-only panics from
    // Reth proof worker pools that are unrelated to lifecycle behavior.
    config.engine.legacy_state_root_task_enabled = true;

    let reth_port_offset = u16::try_from(test_id * 100 + node_id.as_usize() * 10)
        .wrap_err("Reth port offset overflow")?;
    let add_port = |base: u16, name: &str| -> eyre::Result<u16> {
        base.checked_add(reth_port_offset)
            .ok_or_else(|| eyre::eyre!("{name} port overflow"))
    };

    config.network.port = add_port(config.network.port, "reth p2p")?;
    config.network.discovery.port = add_port(config.network.discovery.port, "reth discovery v4")?;
    config.network.discovery.discv5_port =
        add_port(config.network.discovery.discv5_port, "reth discovery v5")?;
    config.network.discovery.discv5_port_ipv6 = add_port(
        config.network.discovery.discv5_port_ipv6,
        "reth discovery v5 ipv6",
    )?;
    config.rpc.auth_port = add_port(config.rpc.auth_port, "reth auth rpc")?;
    config.rpc.ipcpath = ipc.reth.to_string_lossy().to_string();
    config.rpc.auth_ipc = true;
    config.rpc.auth_ipc_path = ipc.auth.to_string_lossy().to_string();
    config.rpc.http = false;
    config.rpc.ws = false;

    Ok(config)
}

/// Launch the Reth execution layer with IPC sockets and offset ports.
///
/// If the node exits with an error, the broadcast `tx` is dropped so the
/// step sequencer observes a channel-closed error instead of hanging.
async fn spawn_execution_layer(
    node_config: NodeConfig<ArcChainSpec>,
    tx: broadcast::Sender<ArcEvent>,
) -> eyre::Result<(TaskExecutor, JoinHandle<()>)> {
    let executor = TaskExecutor::test();
    let arc_node = ArcNode::new(
        ArcRpcConfig::default(),
        InvalidTxListConfig::default(),
        AddressesDenylistConfig::default(),
        None,
        true,
        false,
        false,
        160 * 1024 * 1024,
        ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT,
        std::time::Duration::from_secs(0),
    );

    let reth_handle = NodeBuilder::new(node_config)
        .testing_node(executor.clone())
        .node(arc_node)
        .launch()
        .await
        .wrap_err("failed to launch reth node")?;

    let task = tokio::spawn(async move {
        if let Err(e) = reth_handle.wait_for_node_exit().await {
            error!(error = %e, "Reth node exited with error");
            // Close the broadcast channel so the step sequencer sees a
            // channel-closed error instead of hanging on block waits.
            drop(tx);
        }
    });

    Ok((executor, task))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use arc_consensus_types::{ArcContext, Height};
    use malachitebft_app_channel::app::engine::util::events::{Event, TxEvent};

    fn test_handle() -> ArcNodeHandle {
        let temp_dir = TempDir::new().expect("temp dir");
        let ipc = IpcPaths::new(temp_dir.path());
        let (tx, _) = broadcast::channel::<ArcEvent>(16);

        ArcNodeHandle {
            node_id: NodeId::new(0),
            temp_dir,
            ipc,
            tx,
            cancel_token: CancellationToken::new(),
            cl: Mutex::new(None),
            el: Mutex::new(None),
        }
    }

    #[tokio::test]
    async fn consensus_bridge_uses_existing_handle_event_bus() {
        let handle = test_handle();
        let mut rx = handle.subscribe();
        let tx_event = TxEvent::<ArcContext>::new();

        let bridge_task = spawn_consensus_event_bridge(&handle, tx_event.clone());
        tokio::task::yield_now().await;
        tx_event.send(|| Event::StartedHeight(Height::new(7), false));

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("event timed out")
            .expect("event channel closed");

        assert!(matches!(
            event,
            ArcEvent::ConsensusStartedHeight { height } if height == Height::new(7)
        ));

        bridge_task.abort();
    }
}
