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

//! Node lifecycle management and identity configuration.
//!
//! This module defines the [`App`] struct which orchestrates the startup and shutdown
//! of a consensus node, including:
//!
//! - Loading and configuring node identity (P2P keys and consensus signing)
//! - Opening the persistent store
//! - Starting the consensus engine
//! - Connecting to the execution layer
//! - Spawning the RPC and metrics servers
//! - Handling graceful shutdown on SIGTERM
//!
//! The module also defines identity types ([`P2pIdentity`], [`ConsensusIdentity`],
//! [`NodeIdentity`]) that encapsulate the cryptographic keys and addresses used
//! for network communication and block signing.

use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use eyre::Context;
use rand::rngs::OsRng;
use tokio::signal::unix::SignalKind;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use malachitebft_app_channel::app::events::TxEvent;
use malachitebft_app_channel::app::metrics::SharedRegistry;
use malachitebft_app_channel::app::types::Keypair;
use malachitebft_app_channel::{
    Channels, ConsensusContext, EngineHandle, NetworkContext, NetworkIdentity, RequestContext,
    SyncContext, WalContext,
};

use malachitebft_app_channel::app::config::{GossipSubConfig, PubSubProtocol};

use arc_consensus_types::codec::{network::NetCodec, wal::WalCodec};
use arc_consensus_types::signing::PublicKey;
use arc_consensus_types::{Address, ArcContext, Config, SigningConfig};
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_node_consensus_cli::metrics;
use arc_signer::local::{LocalSigningProvider, PrivateKey};
use arc_signer::ArcSigningProvider;

use crate::env_config::EnvConfig;
use crate::hardcoded_config::{GossipLoad, GossipMeshParams};
use crate::metrics::{AppMetrics, DbMetrics, ProcessMetrics};
use crate::request::AppRequest;
use crate::spec::{ChainId, ConsensusSpec};
use crate::state::State;
use crate::store::Store;
use crate::utils::HaltAndWait;

pub use crate::config::StartConfig;
use crate::utils::pretty::Pretty;

const APP_REQUEST_CHANNEL_SIZE: usize = 64;
/// At most 5 different clients sending requests concurrently
/// Note that ConsensusRequest::dump_state blocks until the response is sent back
const CONSENSUS_REQUEST_CHANNEL_SIZE: usize = 5;

/// Main application struct implementing the consensus node functionality
pub struct App {
    /// The configuration for the node
    config: Config,
    /// The home directory for the node
    home_dir: PathBuf,
    /// The path to the private key file
    private_key_file: PathBuf,
    /// The configuration for the start
    start_config: StartConfig,
    /// Metrics registry
    registry: SharedRegistry,
}

/// Handle for the application.
pub struct Handle {
    pub app: JoinHandle<eyre::Result<()>>,
    pub rpc: Option<JoinHandle<()>>,
    pub engine: EngineHandle,
    pub store: Store,
    pub store_monitor: JoinHandle<()>,
    pub tx_event: TxEvent<ArcContext>,
    pub cancel_token: CancellationToken,
    /// Kept alive to prevent the app request channel from closing when RPC is disabled.
    _tx_app_req: mpsc::Sender<AppRequest>,
}

#[derive(Clone)]
pub struct P2pIdentity {
    pub keypair: Keypair,
}

impl P2pIdentity {
    pub fn new(private_key: &PrivateKey) -> Self {
        let keypair = Keypair::ed25519_from_bytes(private_key.inner().to_bytes())
            .expect("valid ed25519 key bytes");
        Self { keypair }
    }
}

#[derive(Clone)]
pub struct ConsensusIdentity {
    address: Address,
    public_key: PublicKey,
    signing_provider: ArcSigningProvider,
}

impl ConsensusIdentity {
    pub fn new(
        address: Address,
        public_key: PublicKey,
        signing_provider: ArcSigningProvider,
    ) -> Self {
        Self {
            address,
            public_key,
            signing_provider,
        }
    }

    pub fn address(&self) -> Address {
        self.address
    }

    pub fn public_key(&self) -> &PublicKey {
        &self.public_key
    }

    pub fn signing_provider(&self) -> &ArcSigningProvider {
        &self.signing_provider
    }
}

impl From<ConsensusIdentity> for ConsensusContext<ArcContext, ArcSigningProvider> {
    fn from(identity: ConsensusIdentity) -> Self {
        ConsensusContext {
            address: identity.address,
            signing_provider: identity.signing_provider,
        }
    }
}

#[derive(Clone)]
pub struct NodeIdentity {
    pub moniker: String,
    pub p2p: P2pIdentity,
    pub consensus: ConsensusIdentity,
}

impl NodeIdentity {
    pub fn new(moniker: String, p2p: P2pIdentity, consensus: ConsensusIdentity) -> Self {
        Self {
            moniker,
            p2p,
            consensus,
        }
    }
}

impl App {
    pub fn new(
        mut config: Config,
        home_dir: PathBuf,
        private_key_file: PathBuf,
        start_config: StartConfig,
    ) -> Self {
        Self::prepare_config(&mut config, &start_config);

        let registry = SharedRegistry::global().with_moniker(&config.moniker);

        Self {
            config,
            home_dir,
            private_key_file,
            start_config,
            registry,
        }
    }

    /// Apply CLI gossipsub overrides on top of the existing config.
    ///
    /// For the CLI path this re-applies overrides that `build_consensus_config` already set,
    /// which is harmless (idempotent). The method must exist because the config-file path
    /// loads a `Config` from disk and still needs CLI flags merged in.
    fn prepare_config(config: &mut Config, start_config: &StartConfig) {
        if !start_config.persistent_peers.is_empty() {
            config.consensus.p2p.persistent_peers = start_config.persistent_peers.clone();
        }
        if start_config.persistent_peers_only {
            config.consensus.p2p.persistent_peers_only = true;
        }

        let overrides = &start_config.gossipsub_overrides;
        if let PubSubProtocol::GossipSub(ref gs) = config.consensus.p2p.protocol {
            let needs_override = overrides.explicit_peering
                || overrides.mesh_prioritization
                || overrides.load != GossipLoad::Average;

            if needs_override {
                // Average preserves existing mesh sizes so only peering/prioritization flags
                // take effect; Low/High replace mesh sizes with canonical values.
                let p = if overrides.load != GossipLoad::Average {
                    overrides.load.mesh_params()
                } else {
                    GossipMeshParams {
                        mesh_n: gs.mesh_n(),
                        mesh_n_high: gs.mesh_n_high(),
                        mesh_n_low: gs.mesh_n_low(),
                        mesh_outbound_min: gs.mesh_outbound_min(),
                    }
                };

                config.consensus.p2p.protocol = PubSubProtocol::GossipSub(GossipSubConfig::new(
                    p.mesh_n,
                    p.mesh_n_high,
                    p.mesh_n_low,
                    p.mesh_outbound_min,
                    overrides.mesh_prioritization || gs.enable_peer_scoring(),
                    overrides.explicit_peering || gs.enable_explicit_peering(),
                    gs.enable_flood_publish(),
                ));
            }
        }
    }

    fn load_private_key(&self) -> eyre::Result<PrivateKey> {
        let private_key = std::fs::read_to_string(&self.private_key_file)?;
        serde_json::from_str(&private_key).map_err(|e| e.into())
    }

    async fn setup_node_identity(&self) -> eyre::Result<NodeIdentity> {
        // In RPC sync mode, use ephemeral keys since we don't sign anything
        // and don't participate in P2P networking
        if self.start_config.is_rpc_sync_mode() {
            return Ok(self.setup_ephemeral_identity());
        }

        let p2p_identity = self.p2p_identity()?;
        let consensus_identity = self.consensus_identity().await?;

        Ok(NodeIdentity::new(
            self.config.moniker.clone(),
            p2p_identity,
            consensus_identity,
        ))
    }

    /// Generate ephemeral identity for RPC sync mode
    ///
    /// In RPC sync mode, we don't need real keys because:
    /// - No P2P: We fetch blocks via HTTP RPC, not libp2p
    /// - No signing: We're not a validator, so we never sign proposals/votes
    /// - Verification only: The signing provider is only used to verify
    ///   signatures from validators (using their public keys, not ours)
    fn setup_ephemeral_identity(&self) -> NodeIdentity {
        // Generate random private key for P2P identity
        // (not actually used since we don't connect to P2P network)
        let p2p_key = PrivateKey::generate(OsRng);
        let p2p_identity = P2pIdentity::new(&p2p_key);

        // Generate random private key for consensus identity
        // (signing methods will never be called since we're not a validator)
        let consensus_key = PrivateKey::generate(OsRng);
        let local_provider = LocalSigningProvider::new(consensus_key);
        let public_key = local_provider.public_key();
        let address = Address::from_public_key(&public_key);

        info!(
            %address,
            "Using ephemeral identity for RPC sync mode (no signing will occur)"
        );

        let consensus_identity = ConsensusIdentity::new(
            address,
            public_key,
            ArcSigningProvider::Local(local_provider),
        );

        NodeIdentity::new(
            self.config.moniker.clone(),
            p2p_identity,
            consensus_identity,
        )
    }

    fn p2p_identity(&self) -> eyre::Result<P2pIdentity> {
        let private_key = self.load_private_key()?;
        Ok(P2pIdentity::new(&private_key))
    }

    async fn consensus_identity(&self) -> eyre::Result<ConsensusIdentity> {
        use arc_signer::local::LocalSigningProvider;
        use arc_signer::remote::{RemoteSigningConfig, RemoteSigningProvider};

        match &self.config.signing {
            SigningConfig::Local => {
                info!("Using local signing provider");

                let private_key = self.load_private_key()?;
                let local_provider = LocalSigningProvider::new(private_key);
                let public_key = local_provider.public_key();
                let address = Address::from_public_key(&public_key);

                info!(%address, public_key = %Pretty(&public_key), "Loaded local signer identity");

                Ok(ConsensusIdentity::new(
                    address,
                    public_key,
                    ArcSigningProvider::Local(local_provider),
                ))
            }
            SigningConfig::Remote(cfg) => {
                info!(endpoint = %cfg.endpoint, "Using remote signing provider");

                let config = RemoteSigningConfig::from(cfg.clone());
                let remote_provider = RemoteSigningProvider::new(config)
                    .await
                    .wrap_err("Failed to create remote signing provider")?;

                self.registry.with_prefix("arc_remote_signer", |registry| {
                    remote_provider.metrics().register(registry);
                });

                let public_key = remote_provider
                    .public_key()
                    .await
                    .wrap_err("Failed to get public key from remote signer")?;

                let address = Address::from_public_key(&public_key);

                info!(%address, public_key = %Pretty(&public_key), "Loaded remote signer identity");

                Ok(ConsensusIdentity::new(
                    address,
                    public_key,
                    ArcSigningProvider::Remote(remote_provider),
                ))
            }
        }
    }

    fn setup_metrics(&self) -> (AppMetrics, DbMetrics, ProcessMetrics) {
        let app_metrics = AppMetrics::register(&self.registry);
        let db_metrics = DbMetrics::register(&self.registry);
        let process_metrics = ProcessMetrics::register(&self.registry);

        (app_metrics, db_metrics, process_metrics)
    }

    fn spawn_metrics_server(&self, process_metrics: ProcessMetrics) {
        if self.config.metrics.enabled {
            tokio::spawn(metrics::serve(self.config.metrics.listen_addr));

            tokio::spawn(async move {
                loop {
                    process_metrics.update_all_metrics();
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            });
        }
    }

    async fn open_store(
        &self,
        db_metrics: DbMetrics,
        cache_size: ByteSize,
    ) -> eyre::Result<(Store, JoinHandle<()>)> {
        let db_path = self.home_dir.join("store.db");

        info!(path = %db_path.display(), "Opening database");

        let store = Store::open(
            &db_path,
            db_metrics,
            self.start_config.db_upgrade(),
            cache_size,
        )
        .await
        .wrap_err(format!("Failed to open database at {}", db_path.display()))?;

        info!("Database opened");

        let store_monitor = store.spawn_monitor(Duration::from_secs(30));

        Ok((store, store_monitor))
    }

    fn override_config_from_env(&mut self, env_config: &EnvConfig) {
        if let Some(interval) = env_config.status_update_interval {
            self.config.value_sync.status_update_interval = interval;
        }
    }

    fn apply_state_overrides(&self, state: &mut State) {
        if let Some(suggested_fee_recipient) = self.start_config.suggested_fee_recipient {
            state.set_suggested_fee_recipient(suggested_fee_recipient);
        }
    }

    async fn start_consensus_engine(
        &self,
        ctx: ArcContext,
        identity: NodeIdentity,
    ) -> eyre::Result<(Channels<ArcContext>, EngineHandle)> {
        let wal_path = self.home_dir.join("wal").join("consensus.wal");

        if self.start_config.is_rpc_sync_mode() {
            // Use EngineBuilder with custom Network and Sync actors for RPC sync mode
            // Streaming will start when Sync actor receives first StartedHeight from Consensus
            self.start_rpc_sync_engine(ctx, identity, wal_path).await
        } else {
            // Create validator proof (ADR-006) binding public key to peer ID
            //
            // TODO: Currently all non-RPC-sync nodes create a validator proof, but this should
            // only apply to actual validators. Non-validator full nodes should use
            // `NetworkIdentity::new(moniker, keypair, None)` instead of `new_validator`.
            // This requires a way to detect if the node is configured as a validator
            let network_identity = self
                .create_network_identity_with_proof(&identity)
                .await
                .wrap_err("Failed to create network identity with validator proof")?;

            // Use default engine for consensus mode
            let (channels, engine_handle) = malachitebft_app_channel::start_engine(
                ctx,
                self.config.clone(),
                WalContext::new(wal_path, WalCodec),
                NetworkContext::new(network_identity, NetCodec),
                ConsensusContext::from(identity.consensus),
                SyncContext::new(NetCodec),
                RequestContext::new(CONSENSUS_REQUEST_CHANNEL_SIZE),
            )
            .await
            .wrap_err("Failed to start consensus engine")?;

            Ok((channels, engine_handle))
        }
    }

    /// Create a NetworkIdentity with a signed validator proof.
    ///
    /// The validator proof binds the consensus public key to the libp2p peer ID,
    /// proving that the validator controls both keys (ADR-006).
    async fn create_network_identity_with_proof(
        &self,
        identity: &NodeIdentity,
    ) -> eyre::Result<NetworkIdentity> {
        let public_key_bytes = identity.consensus.public_key().as_bytes().to_vec();
        let peer_id_bytes = identity.p2p.keypair.public().to_peer_id().to_bytes();
        let address = identity.consensus.address().to_string();

        let proof_bytes = crate::validator_proof::create_validator_proof(
            identity.consensus.signing_provider(),
            public_key_bytes,
            peer_id_bytes,
            &address,
        )
        .await?;

        Ok(NetworkIdentity::new_validator(
            identity.moniker.clone(),
            identity.p2p.keypair.clone(),
            address,
            proof_bytes,
        ))
    }

    /// Start the consensus engine with RPC sync mode
    ///
    /// Uses custom Network and default Sync actors:
    /// - Network actor manages WebSocket subscriptions and handles fetch requests
    /// - Sync actor coordinates sync state
    async fn start_rpc_sync_engine(
        &self,
        ctx: ArcContext,
        identity: NodeIdentity,
        wal_path: std::path::PathBuf,
    ) -> eyre::Result<(Channels<ArcContext>, EngineHandle)> {
        use malachitebft_app_channel::EngineBuilder;

        info!("Starting consensus engine with RPC sync mode");

        // Spawn Network actor - manages WebSocket subscriptions and RPC fetching
        let (network_ref, network_tx) =
            crate::rpc_sync::spawn_rpc_network_actor(self.start_config.rpc_sync_endpoints.clone())
                .await?;

        // Build engine with custom network actor and default sync actor
        // The network actor handles WebSocket subscriptions and sends `NetworkEvent::Status`
        // It fetches blocks and certificates via RPC, sends `NetworkEvent::SyncResponse`
        let (channels, engine_handle) = EngineBuilder::new(ctx, self.config.clone())
            .with_default_wal(WalContext::new(wal_path, WalCodec))
            .with_custom_network(network_ref, network_tx)
            .with_default_sync(SyncContext::new(NetCodec))
            .with_default_consensus(ConsensusContext::from(identity.consensus))
            .with_default_request(RequestContext::new(CONSENSUS_REQUEST_CHANNEL_SIZE))
            .build()
            .await
            .wrap_err("Failed to start consensus engine with RPC sync")?;

        Ok((channels, engine_handle))
    }

    fn start_rpc_server(
        &self,
        channels: &Channels<ArcContext>,
    ) -> (
        mpsc::Sender<AppRequest>,
        mpsc::Receiver<AppRequest>,
        Option<JoinHandle<()>>,
    ) {
        let (tx_rpc_req, rx_app_req) = mpsc::channel(APP_REQUEST_CHANNEL_SIZE);

        let rpc_handle = if self.config.rpc.enabled {
            let join_handle = tokio::spawn({
                let listen_addr = self.config.rpc.listen_addr;
                let request_handle = channels.requests.clone();
                let net_request_handle = channels.net_requests.clone();
                crate::rpc::serve(
                    listen_addr,
                    request_handle,
                    tx_rpc_req.clone(),
                    net_request_handle,
                )
            });
            Some(join_handle)
        } else {
            None
        };

        (tx_rpc_req, rx_app_req, rpc_handle)
    }

    async fn connect_to_execution_engine(&self) -> eyre::Result<Engine> {
        match self.start_config.engine_config() {
            Some(engine_config) => engine_config
                .connect()
                .await
                .wrap_err("Failed to connect to execution engine"),
            None => Err(eyre::eyre!(
                "No engine configuration provided. Please specify either IPC sockets or RPC endpoints."
            )),
        }
    }

    /// Query the execution engine to retrieve the chain ID and genesis block.
    ///
    /// These are used during node startup to compute the initial network ID
    /// and to configure the consensus state with the genesis block's hash and timestamp.
    async fn resolve_chain_identity(
        &self,
        engine: &Engine,
    ) -> eyre::Result<(ChainId, ExecutionBlock)> {
        let eth_chain_id = engine
            .eth
            .get_chain_id()
            .await
            .wrap_err("Failed to get chain ID from execution engine")?;

        let chain_id: ChainId = eth_chain_id
            .parse()
            .wrap_err("Invalid chain ID from execution engine")?;

        let genesis_block = engine
            .eth
            .get_genesis_block()
            .await
            .wrap_err("Failed to get genesis block from execution engine")?;

        Ok((chain_id, genesis_block))
    }

    #[tracing::instrument(name = "node", skip_all, fields(moniker = %self.config.moniker))]
    async fn start(&mut self) -> eyre::Result<Handle> {
        let ctx = ArcContext::new();

        // Read environment-based configuration once at startup
        let env_config = EnvConfig::from_env();

        // Apply config overrides from environment variables
        self.override_config_from_env(&env_config);

        // Setup node identity, uses ephemeral keys in follow mode (RPC sync mode))
        let identity = self.setup_node_identity().await?;

        // Setup metrics
        let (app_metrics, db_metrics, process_metrics) = self.setup_metrics();

        // Open the store
        let (store, store_monitor) = self
            .open_store(db_metrics, env_config.db_cache_size)
            .await?;

        // Connect to the execution engine early so we can resolve consensus spec and genesis hash
        let engine = self.connect_to_execution_engine().await?;

        let (chain_id, genesis_block) = self
            .resolve_chain_identity(&engine)
            .await
            .wrap_err("Failed to resolve chain identity from execution engine")?;

        let consensus_spec = ConsensusSpec::from(chain_id);

        // Configure Engine API version selection (V4 vs V5) based on the chainspec.
        // When ARC_GENESIS_FILE_PATH is set, parse the same genesis.json the EL uses
        // so that patched hardfork timestamps (e.g. nightly-upgrade) are picked up
        // correctly. Otherwise fall back to the static chainspec for the chain ID.
        if let Some(ref genesis_path) = env_config.genesis_file_path {
            engine
                .set_osaka_from_genesis_file(genesis_path)
                .wrap_err("Failed to configure Osaka activation from genesis file")?;
        } else {
            engine.set_osaka_from_chain_id(chain_id.as_u64());
        }

        info!(
            %chain_id,
            genesis_hash = %genesis_block.block_hash,
            genesis_timestamp = genesis_block.timestamp,
            "Resolved chain identity from execution engine"
        );

        // Initialize the application state with the resolved spec and genesis block
        let mut state = State::builder(ctx)
            .identity(identity.consensus.clone())
            .store(store.clone())
            .config(self.config.clone())
            .env_config(env_config)
            .spec(consensus_spec)
            .genesis_block(genesis_block)
            .metrics(app_metrics)
            .build();

        // Apply any state overrides from the start configuration (e.g. suggested fee recipient)
        self.apply_state_overrides(&mut state);

        // Spawn the metrics server
        self.spawn_metrics_server(process_metrics);

        // Start the consensus engine
        let (channels, engine_handle) = self.start_consensus_engine(ctx, identity).await?;

        // Start the application RPC server
        let (tx_app_req, rx_app_req, rpc_handle) = self.start_rpc_server(&channels);

        let tx_event = channels.events.clone();
        let cancel_token = CancellationToken::new();

        // Start the application task
        let app_handle = tokio::spawn({
            let cancel_token = cancel_token.clone();
            crate::app::run(state, channels, engine, rx_app_req, cancel_token)
        });

        // Start the pprof server if enabled
        if let Some(pprof_bind_address) = self.start_config.pprof_bind_address {
            spawn_pprof_server(pprof_bind_address);
        }

        Ok(Handle {
            app: app_handle,
            rpc: rpc_handle,
            engine: engine_handle,
            store_monitor,
            tx_event,
            store,
            cancel_token,
            _tx_app_req: tx_app_req,
        })
    }

    pub async fn run(mut self) -> eyre::Result<()> {
        if self.start_config.is_rpc_sync_mode() {
            info!("Running in RPC sync mode");
        }

        // Start the application
        let handles = match self.start().await {
            Ok(handles) => handles,
            Err(e) => {
                let startup_error = e.wrap_err("Node failed to start");
                error!("{startup_error:?}");
                error!("Manual intervention required! Waiting for termination signal (SIGTERM)...");

                // Wait for SIGTERM to allow graceful shutdown
                wait_for_termination().await;

                return Err(startup_error);
            }
        };

        // Install SIGTERM handler for graceful shutdown
        install_sigterm_handler(&handles);

        // Wait for the application to finish
        let result = handles.app.await?;

        if let Err(e) = &result {
            // If the application halted due to reaching a configured height,
            // we stop the consensus engine and wait indefinitely for a termination signal.
            if e.downcast_ref::<HaltAndWait>().is_some() {
                warn!("Node halted, stopping consensus...");

                // Stop the consensus engine
                handles
                    .engine
                    .actor
                    .stop_and_wait(Some("Node halted at configured height".to_string()), None)
                    .await?;

                // Create a database savepoint ensuring no repair is needed on restart
                handles.store.savepoint();

                info!("Node stopped, waiting for termination signal...");
                tokio::time::sleep(Duration::MAX).await;
            }
        }

        result
    }
}

/// Install a SIGTERM handler to gracefully shutdown the node
///
/// ## Note
/// This is only available on Unix systems.
#[cfg(unix)]
fn install_sigterm_handler(handle: &Handle) {
    use tokio::signal::unix::signal;
    use tokio::time::sleep;

    let node = handle.engine.actor.clone();
    let store = handle.store.clone();
    let cancel_token = handle.cancel_token.clone();

    let mut sigterm = signal(SignalKind::terminate()).expect("inside Tokio runtime");

    tokio::spawn(async move {
        // Wait for the SIGTERM signal
        sigterm.recv().await;

        warn!("Received SIGTERM, shutting down...");

        // Trigger cancellation of the application
        cancel_token.cancel();

        // Give some time to the application to process the cancellation
        sleep(Duration::from_millis(500)).await;

        // Stop the consensus engine
        if let Err(e) = node
            .stop_and_wait(Some("Received SIGTERM signal".to_string()), None)
            .await
        {
            warn!(%e, "Failed to stop the node gracefully");
        }

        // Create a database savepoint ensuring no repair is needed on restart
        store.savepoint();

        info!("Waiting for all tasks to finish...");
        sleep(Duration::from_millis(500)).await;
        info!("Shutdown complete, exiting");

        // In Kubernetes signals, exit code 143 means that a container
        // was terminated by receiving a SIGTERM signal
        std::process::exit(143);
    });
}

#[cfg(not(unix))]
fn install_sigterm_handler(_handle: &Handle) {}

/// Wait for a termination signal (SIGTERM on Unix)
async fn wait_for_termination() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("inside Tokio runtime");
        sigterm.recv().await;
    }

    #[cfg(not(unix))]
    {
        // On non-Unix systems, we simply wait indefinitely
        futures::future::pending::<()>().await;
    }
}

#[cfg(feature = "pprof")]
fn spawn_pprof_server(bind_address: std::net::SocketAddr) {
    tokio::spawn(async move {
        if let Err(e) =
            pprof_hyper_server::serve(bind_address, pprof_hyper_server::Config::default()).await
        {
            tracing::error!(error = %e, "pprof server failed to start");
        }
    });
}

#[cfg(not(feature = "pprof"))]
fn spawn_pprof_server(_bind_address: std::net::SocketAddr) {}
