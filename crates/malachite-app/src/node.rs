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
use tokio::sync::{mpsc, oneshot};
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
use arc_consensus_types::{Address, ArcContext, ChainId, Config, ConsensusSpec, SigningConfig};
use arc_eth_engine::engine::Engine;
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_node_consensus_cli::metrics;
use arc_signer::local::{LocalSigningProvider, PrivateKey};
use arc_signer::ArcSigningProvider;

use crate::env_config::EnvConfig;
use crate::hardcoded_config::{GossipLoad, GossipMeshParams};
use crate::metrics::{AppMetrics, DbMetrics, ProcessMetrics, ValidatorSetMetrics};
use crate::request::AppRequest;
use crate::state::State;
use crate::store::Store;
use crate::utils::HaltAndWait;

pub use crate::config::StartConfig;
use crate::utils::pretty::Pretty;

const APP_REQUEST_CHANNEL_SIZE: usize = 64;
/// At most 5 different clients sending requests concurrently
/// Note that ConsensusRequest::dump_state blocks until the response is sent back
const CONSENSUS_REQUEST_CHANNEL_SIZE: usize = 5;

/// Overall budget for connecting to the execution engine and resolving chain
/// identity at startup. Exceeding it means the EL is unreachable.
const STARTUP_ENGINE_RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);

/// Delay between chain-identity resolution attempts. Each attempt rebuilds the
/// engine connection.
const STARTUP_ENGINE_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);

/// Marker attached to a startup failure when the execution engine stays unreachable
/// past the resolution deadline. It distinguishes a transient, restart-recoverable
/// condition from an unrecoverable one (corrupt store, bad genesis/keys): the
/// former exits non-zero so the orchestrator restarts the node, the latter parks
/// for manual intervention.
#[derive(Debug, thiserror::Error)]
#[error("execution engine unreachable during startup")]
struct ExecutionEngineUnreachable;

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
    /// Set before the Node actor is stopped so the application loop treats the resulting
    /// consensus-channel close as a clean shutdown rather than an error.
    graceful_shutdown: CancellationToken,
    /// Fires when the EL IPC watchdog triggered shutdown (as opposed to SIGTERM or normal halt).
    el_watchdog_triggered: oneshot::Receiver<()>,
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

impl From<ConsensusIdentity> for ConsensusContext<ArcContext> {
    fn from(identity: ConsensusIdentity) -> Self {
        ConsensusContext::new_validator(
            identity.address,
            Box::new(identity.signing_provider.clone()),
            Box::new(identity.signing_provider),
        )
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

/// Validate that `parallel_requests * batch_size` does not overflow `usize`.
///
/// This product bounds the consensus input buffer (`max_pending_proposals`),
/// which computes it with `checked_mul().expect(..)` and would panic on
/// overflow. Env overrides can push these counts arbitrarily high, so validate
/// the effective product here and fail startup with a clear, actionable error.
fn check_pending_proposals_bound(parallel_requests: usize, batch_size: usize) -> eyre::Result<()> {
    parallel_requests.checked_mul(batch_size).ok_or_else(|| {
        eyre::eyre!(
            "value_sync parallel_requests ({parallel_requests}) * batch_size ({batch_size}) \
             overflows usize; reduce ARC_SYNC_PARALLEL_REQUESTS and/or ARC_SYNC_BATCH_SIZE"
        )
    })?;
    Ok(())
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

        let consensus_identity = if self.start_config.validator {
            self.consensus_identity().await?
        } else {
            self.ephemeral_consensus_identity()
        };

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

    /// Generate an ephemeral consensus identity for full nodes.
    ///
    /// Full nodes participate in gossip but do not sign votes or proposals,
    /// so they do not need a persistent consensus key.
    fn ephemeral_consensus_identity(&self) -> ConsensusIdentity {
        let consensus_key = PrivateKey::generate(OsRng);
        let local_provider = LocalSigningProvider::new(consensus_key);
        let public_key = local_provider.public_key();
        let address = Address::from_public_key(&public_key);

        info!(
            %address,
            "Using ephemeral consensus identity for full node (no signing will occur)"
        );

        ConsensusIdentity::new(
            address,
            public_key,
            ArcSigningProvider::Local(local_provider),
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

        // Wire the global recorder for `arc_validator_set_skipped_total` so the counter
        // emitted from `abi_decode_validator_set` lands on the consensus `/metrics` endpoint.
        ValidatorSetMetrics::register(&self.registry).install_global();

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

    fn override_config_from_env(&mut self, env_config: &EnvConfig) -> eyre::Result<()> {
        // Record every override that actually takes effect, keyed by the env var
        // an operator sets, so a stale or forgotten variable is visible at startup.
        let mut applied: Vec<String> = Vec::new();

        // Apply an optional override to a config field and record it under its env
        // var name. `$fmt` is the value formatter: `{:?}` for durations, `{}` for
        // byte sizes and counts.
        macro_rules! apply {
            ($src:expr => $dst:expr, $var:literal, $fmt:literal) => {
                if let Some(v) = $src {
                    $dst = v;
                    applied.push(format!(concat!($var, "=", $fmt), v));
                }
            };
        }

        let value_sync = &mut self.config.value_sync;
        apply!(env_config.status_update_interval => value_sync.status_update_interval, "ARC_SYNC_STATUS_UPDATE_INTERVAL", "{:?}");
        apply!(env_config.value_sync_request_timeout => value_sync.request_timeout, "ARC_SYNC_REQUEST_TIMEOUT", "{:?}");
        apply!(env_config.value_sync_max_request_size => value_sync.max_request_size, "ARC_SYNC_MAX_REQUEST_SIZE", "{}");
        apply!(env_config.value_sync_max_response_size => value_sync.max_response_size, "ARC_SYNC_MAX_RESPONSE_SIZE", "{}");
        apply!(env_config.value_sync_parallel_requests => value_sync.parallel_requests, "ARC_SYNC_PARALLEL_REQUESTS", "{}");
        apply!(env_config.value_sync_inactive_threshold => value_sync.inactive_threshold, "ARC_SYNC_INACTIVE_THRESHOLD", "{:?}");
        apply!(env_config.value_sync_batch_size => value_sync.batch_size, "ARC_SYNC_BATCH_SIZE", "{}");

        check_pending_proposals_bound(value_sync.parallel_requests, value_sync.batch_size)?;

        let consensus = &mut self.config.consensus;
        apply!(env_config.wal_replay_delay => consensus.wal_replay_delay, "ARC_CONSENSUS_WAL_REPLAY_DELAY", "{:?}");
        apply!(env_config.queue_capacity => consensus.queue_capacity, "ARC_CONSENSUS_QUEUE_CAPACITY", "{}");
        apply!(env_config.queue_per_height_capacity => consensus.queue_per_height_capacity, "ARC_CONSENSUS_QUEUE_PER_HEIGHT_CAPACITY", "{}");
        apply!(env_config.ephemeral_connection_timeout => consensus.p2p.discovery.ephemeral_connection_timeout, "ARC_DISCOVERY_EPHEMERAL_CONNECTION_TIMEOUT", "{:?}");

        // Only applies under remote signing; inside the branch `apply!` handles it.
        if let SigningConfig::Remote(ref mut cfg) = self.config.signing {
            apply!(env_config.remote_signing_timeout => cfg.timeout, "ARC_REMOTE_SIGNING_TIMEOUT", "{:?}");
        }

        if !applied.is_empty() {
            info!(overrides = ?applied, "Applied environment variable overrides to consensus config");
        }

        Ok(())
    }

    /// Must run after `resolve_chain_identity` and before the engine is started,
    /// so the overrides are observed when `self.config` is cloned into the engine.
    fn apply_chain_specific_config(&mut self, chain_id: ChainId) {
        let protocol_names = crate::hardcoded_config::consensus::p2p::protocol_names(chain_id);
        info!(
            ?chain_id,
            consensus = %protocol_names.consensus,
            sync = %protocol_names.sync,
            "Applying chain-specific libp2p protocol names",
        );
        self.config.consensus.p2p.protocol_names = protocol_names;
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
            let network_identity = if self.start_config.validator {
                self.create_network_identity_with_proof(&identity)
                    .await
                    .wrap_err("Failed to create network identity with validator proof")?
            } else {
                NetworkIdentity::new(identity.moniker.clone(), identity.p2p.keypair.clone(), None)
            };

            #[cfg(feature = "byzantine")]
            if let Some(byz_cfg) = self.config.byzantine.clone().filter(|c| c.is_active()) {
                return self
                    .start_byzantine_engine(ctx, identity, network_identity, wal_path, byz_cfg)
                    .await;
            }

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

    /// Start the consensus engine with Byzantine behavior enabled.
    ///
    /// Uses the `with_byzantine_network` builder method on `app-channel` to
    /// spawn the real network actor and wrap it with a `ByzantineNetworkProxy`
    /// that intercepts votes and proposals per [`ByzantineConfig`]. State-
    /// machine-layer attacks (`ignore_locks`, `force_precommit_nil`) are
    /// handled inside `ArcContext::new_prevote` / `new_precommit` when
    /// `ArcContext.byzantine` is set.
    #[cfg(feature = "byzantine")]
    async fn start_byzantine_engine(
        &self,
        ctx: ArcContext,
        identity: NodeIdentity,
        network_identity: NetworkIdentity,
        wal_path: std::path::PathBuf,
        byz_cfg: arc_consensus_types::config::ByzantineConfig,
    ) -> eyre::Result<(Channels<ArcContext>, EngineHandle)> {
        use arc_consensus_types::{Value, ValueId};
        use malachitebft_app_channel::{ByzantineContext, EngineBuilder};

        warn!(
            ?byz_cfg,
            "BYZANTINE: Starting node with Byzantine behavior enabled"
        );

        let address = identity.consensus.address();
        let signing_provider = identity.consensus.signing_provider().clone();

        // Conflicting value factory: flip the last byte of the block hash.
        let conflicting_value_fn: malachitebft_engine_byzantine::ConflictingValueFn<ArcContext> =
            Box::new(|original: &Value| {
                let mut hash = original.id().block_hash();
                let bytes = hash.as_mut_slice();
                bytes[31] ^= 0xFF;
                Value::new(hash)
            });

        // Conflicting vote value factory: flip the last byte of the id bytes;
        // nil votes are replaced with a zero id that still differs after flip.
        let conflicting_vote_value_fn: malachitebft_engine_byzantine::ConflictingVoteValueFn<
            ArcContext,
        > = Box::new(|original: Option<&ValueId>| match original {
            Some(id) => {
                let mut hash = id.block_hash();
                let bytes = hash.as_mut_slice();
                bytes[31] ^= 0xFF;
                ValueId::new(hash)
            }
            None => ValueId::new(Default::default()),
        });

        let (channels, engine_handle) = EngineBuilder::new(ctx, self.config.clone())
            .with_default_wal(WalContext::new(wal_path, WalCodec))
            .with_byzantine_network(ByzantineContext {
                identity: network_identity,
                codec: NetCodec,
                config: byz_cfg,
                signer: Box::new(signing_provider),
                address,
                conflicting_value_fn: Some(conflicting_value_fn),
                conflicting_vote_value_fn: Some(conflicting_vote_value_fn),
            })
            .await
            .wrap_err("Failed to install Byzantine network proxy")?
            .with_default_consensus(ConsensusContext::from(identity.consensus))
            .with_default_sync(SyncContext::new(NetCodec))
            .with_default_request(RequestContext::new(CONSENSUS_REQUEST_CHANNEL_SIZE))
            .build()
            .await
            .wrap_err("Failed to start Byzantine consensus engine")?;

        Ok((channels, engine_handle))
    }

    /// Create a NetworkIdentity with a signed validator proof.
    ///
    /// The validator proof binds the consensus public key to the libp2p peer ID,
    /// proving that the validator controls both keys.
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
        metrics: AppMetrics,
    ) -> (
        mpsc::Sender<AppRequest>,
        mpsc::Receiver<AppRequest>,
        Option<JoinHandle<()>>,
    ) {
        let (tx_rpc_req, rx_app_req) = mpsc::channel(APP_REQUEST_CHANNEL_SIZE);

        let rpc_handle = if self.config.rpc.enabled {
            let join_handle = tokio::spawn({
                let listen_addr = self.config.rpc.listen_addr;
                let admin_enabled = self.config.rpc.admin;
                let request_handle = channels.requests.clone();
                let net_request_handle = channels.net_requests.clone();
                let metrics = metrics.clone();
                crate::rpc::serve_with_metrics(
                    listen_addr,
                    request_handle,
                    tx_rpc_req.clone(),
                    net_request_handle,
                    admin_enabled,
                    metrics,
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

    #[tracing::instrument(name = "node", skip_all, fields(moniker = %self.config.moniker))]
    pub async fn start(&mut self) -> eyre::Result<Handle> {
        // Read environment-based configuration once at startup
        let env_config = EnvConfig::from_env()?;

        // Apply config overrides from environment variables
        self.override_config_from_env(&env_config)?;

        // Setup node identity, uses ephemeral keys in follow mode (RPC sync mode))
        let identity = self.setup_node_identity().await?;

        // Setup metrics
        let (app_metrics, db_metrics, process_metrics) = self.setup_metrics();

        // Open the store
        let (store, store_monitor) = self
            .open_store(db_metrics, env_config.db_cache_size)
            .await?;

        // Connect to the execution engine and resolve consensus spec and genesis
        // hash. The EL may be restarting underneath us, so retry with a fresh
        // connection until it serves or the startup deadline elapses.
        let (engine, chain_id, genesis_block) = connect_and_resolve_chain_identity(
            || self.connect_to_execution_engine(),
            STARTUP_ENGINE_RESOLVE_TIMEOUT,
            STARTUP_ENGINE_RECONNECT_BACKOFF,
        )
        .await
        .wrap_err("Failed to resolve chain identity from execution engine")?;

        self.apply_chain_specific_config(chain_id);

        let consensus_spec = ConsensusSpec::from(chain_id);

        // Resolve default follow endpoints when --follow is used without explicit endpoints
        self.start_config
            .resolve_default_rpc_sync_endpoints(chain_id.as_u64())
            .wrap_err("Failed to resolve default follow endpoints")?;

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

        // Build the consensus context. When the byzantine feature is enabled
        // and the config opts in, attach a `ByzantineState` bundle so
        // `ArcContext::new_prevote` / `new_precommit` can intercept votes.
        #[cfg(feature = "byzantine")]
        let ctx = {
            let ctx = ArcContext::new();
            match self.config.byzantine.as_ref() {
                Some(byz) => {
                    use arc_consensus_types::context::ByzantineState;
                    use std::sync::Arc;
                    let state = Arc::new(ByzantineState::new(
                        byz.ignore_locks.clone(),
                        byz.force_precommit_nil.clone(),
                        identity.consensus.address(),
                        byz.seed,
                    ));
                    ctx.with_byzantine(state)
                }
                None => ctx,
            }
        };
        #[cfg(not(feature = "byzantine"))]
        let ctx = ArcContext::new();

        #[cfg(feature = "byzantine")]
        let state_ctx = ctx.clone();
        #[cfg(not(feature = "byzantine"))]
        let state_ctx = ctx;

        // Initialize the application state with the resolved spec and genesis block.
        let mut state = State::builder(state_ctx)
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
        let (tx_app_req, rx_app_req, rpc_handle) =
            self.start_rpc_server(&channels, state.metrics().clone());

        let tx_event = channels.events.clone();
        let cancel_token = CancellationToken::new();
        let graceful_shutdown = CancellationToken::new();

        // Watchdog: on unexpected EL IPC close, signal the run loop and cancel the app task;
        // the run loop performs the bounded Node stop and the process exit.
        let engine_for_watchdog = engine.clone();
        let (el_watchdog_tx, el_watchdog_rx) = oneshot::channel::<()>();
        tokio::spawn({
            let cancel_token = cancel_token.clone();
            async move {
                tokio::select! {
                    _ = engine_for_watchdog.wait_for_disconnect() => {
                        error!("EL IPC connection closed; shutting down");
                        // Signal before cancelling so the run loop observes the trigger before
                        // the app task exits, eliminating a try_recv race.
                        el_watchdog_tx.send(()).ok();
                        cancel_token.cancel();
                    }
                    _ = cancel_token.cancelled() => {}
                }
            }
        });

        // Start the application task
        let app_handle = tokio::spawn({
            let cancel_token = cancel_token.clone();
            let graceful_shutdown = graceful_shutdown.clone();
            crate::app::run(
                state,
                channels,
                engine,
                rx_app_req,
                cancel_token,
                graceful_shutdown,
            )
        });

        // Start the pprof server if enabled
        if let Some(pprof_bind_address) = self.start_config.pprof_bind_address {
            spawn_pprof_server(pprof_bind_address, self.start_config.pprof_heap_prof);
        }

        Ok(Handle {
            app: app_handle,
            rpc: rpc_handle,
            engine: engine_handle,
            store_monitor,
            tx_event,
            store,
            cancel_token,
            graceful_shutdown,
            el_watchdog_triggered: el_watchdog_rx,
            _tx_app_req: tx_app_req,
        })
    }

    pub async fn run(mut self) -> eyre::Result<()> {
        if self.start_config.is_rpc_sync_mode() {
            info!("Running in RPC sync mode");
        }

        // Start the application
        let mut handles = match self.start().await {
            Ok(handles) => handles,
            Err(e) => {
                let startup_error = e.wrap_err("Node failed to start");
                error!("{startup_error:?}");

                if is_execution_engine_unreachable(&startup_error) {
                    error!("Execution engine unreachable during startup. Exiting...");
                    return Err(startup_error);
                }

                error!("Manual intervention required! Waiting for termination signal (SIGTERM)...");
                wait_for_termination().await;
                return Err(startup_error);
            }
        };

        // Install SIGTERM handler for graceful shutdown
        install_sigterm_handler(&handles);

        // Wait for the application to finish
        let result = handles.app.await?;

        // EL IPC closed: stop the Node actor with a bounded timeout, then exit non-zero so
        // the orchestrator restarts the container.
        if handles.el_watchdog_triggered.try_recv().is_ok() {
            stop_node_and_teardown(
                handles.engine.actor.stop_and_wait(
                    Some("EL IPC connection closed".to_string()),
                    Some(Duration::from_secs(10)),
                ),
                &handles.cancel_token,
                &handles.graceful_shutdown,
            )
            .await;
            drain_before_exit(|| handles.store.savepoint()).await;
            std::process::exit(EL_IPC_SHUTDOWN_EXIT_CODE);
        }

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

/// Exit code for an EL IPC-triggered shutdown. Non-zero so the orchestrator restarts the
/// container.
const EL_IPC_SHUTDOWN_EXIT_CODE: i32 = 1;

/// Exit code for a SIGTERM-triggered shutdown: 143 = 128 + SIGTERM (15), the conventional
/// exit status for a process terminated by SIGTERM.
const SIGTERM_EXIT_CODE: i32 = 143;

/// Grace period for in-flight tasks to finish after teardown, before the process exits.
const SHUTDOWN_DRAIN_DELAY: Duration = Duration::from_millis(500);

/// Stops the consensus Node actor, then tears down the application loop.
///
/// Marks the shutdown as graceful before stopping the Node so the application treats the
/// consensus channel closing as a clean exit. Awaiting the Node stop first lets ractor
/// hard-kill the children (consensus, host, network, wal, sync) in order, so in-flight
/// effects never fail on a torn-down sibling. Teardown proceeds even when the Node stop
/// fails, so a stalled child cannot block shutdown.
async fn stop_node_and_teardown<E: std::fmt::Display>(
    stop_node: impl std::future::Future<Output = Result<(), E>>,
    cancel_token: &CancellationToken,
    graceful_shutdown: &CancellationToken,
) {
    graceful_shutdown.cancel();

    if let Err(e) = stop_node.await {
        warn!(%e, "Failed to stop the node gracefully");
    }

    cancel_token.cancel();
}

/// Persists a database savepoint, then gives in-flight tasks a brief moment to finish
/// before the caller exits the process.
///
/// The savepoint runs through a callback to keep this decoupled from the store. Callers
/// invoke [`std::process::exit`] afterwards; the explicit exit bounds shutdown time even
/// if a `Drop` implementation stalls during teardown.
async fn drain_before_exit(savepoint: impl FnOnce()) {
    savepoint();

    info!("Waiting for all tasks to finish...");
    tokio::time::sleep(SHUTDOWN_DRAIN_DELAY).await;
    info!("Shutdown complete, exiting");
}

/// Query the execution engine for the chain ID and genesis block.
///
/// Used during node startup to compute the initial network ID and to configure the
/// consensus state with the genesis block's hash and timestamp.
async fn resolve_chain_identity(engine: &Engine) -> eyre::Result<(ChainId, ExecutionBlock)> {
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

/// Connect to the execution engine and resolve chain identity, retrying with a fresh
/// connection until the engine serves or `deadline` elapses.
///
/// Each attempt rebuilds the connection via `connect` to cover the case where
/// the EL is restarting and the IPC socket is not yet available.
async fn connect_and_resolve_chain_identity<C, F>(
    connect: C,
    deadline: Duration,
    backoff: Duration,
) -> eyre::Result<(Engine, ChainId, ExecutionBlock)>
where
    C: Fn() -> F,
    F: std::future::Future<Output = eyre::Result<Engine>>,
{
    let resolved = tokio::time::timeout(deadline, async {
        loop {
            let engine = connect().await?;
            match resolve_chain_identity(&engine).await {
                Ok((chain_id, genesis_block)) => {
                    return Ok::<_, eyre::Report>((engine, chain_id, genesis_block));
                }
                Err(e) => {
                    warn!("Failed to resolve chain identity, retrying: {e:?}");
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    })
    .await;

    match resolved {
        Ok(result) => result,
        Err(_) => Err(eyre::Report::new(ExecutionEngineUnreachable)
            .wrap_err(format!("execution engine unreachable after {deadline:?}"))),
    }
}

/// Whether a startup error was caused by the execution engine being unreachable
/// past the resolution deadline (a transient, restart-recoverable condition). Walks
/// the error chain so the marker is found beneath the context layers added on the
/// way up through `start`/`run`.
fn is_execution_engine_unreachable(err: &eyre::Report) -> bool {
    err.chain()
        .any(|cause| cause.is::<ExecutionEngineUnreachable>())
}

/// Install a SIGTERM handler to gracefully shutdown the node
///
/// ## Note
/// This is only available on Unix systems.
#[cfg(unix)]
fn install_sigterm_handler(handle: &Handle) {
    use tokio::signal::unix::signal;

    let node = handle.engine.actor.clone();
    let store = handle.store.clone();
    let cancel_token = handle.cancel_token.clone();
    let graceful_shutdown = handle.graceful_shutdown.clone();

    let mut sigterm = signal(SignalKind::terminate()).expect("inside Tokio runtime");

    tokio::spawn(async move {
        sigterm.recv().await;

        warn!("Received SIGTERM, shutting down...");

        stop_node_and_teardown(
            node.stop_and_wait(
                Some("Received SIGTERM signal".to_string()),
                Some(Duration::from_secs(10)),
            ),
            &cancel_token,
            &graceful_shutdown,
        )
        .await;

        drain_before_exit(|| store.savepoint()).await;
        std::process::exit(SIGTERM_EXIT_CODE);
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
            tracing::error!(error = %e, "pprof server failed to start");
        }
    });
}

#[cfg(not(feature = "pprof"))]
fn spawn_pprof_server(_bind_address: std::net::SocketAddr, _heap_prof: bool) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use arc_eth_engine::mocks::{MockEngineAPI, MockEthereumAPI};
    use tempfile::tempdir;

    /// Minimal genesis block for mock execution engines.
    fn mock_genesis_block() -> ExecutionBlock {
        ExecutionBlock {
            block_hash: Default::default(),
            block_number: 0,
            parent_hash: Default::default(),
            timestamp: 0,
        }
    }

    #[test]
    fn check_pending_proposals_bound_accepts_normal_values() {
        assert!(check_pending_proposals_bound(5, 10).is_ok());
        assert!(check_pending_proposals_bound(1, 1).is_ok());
    }

    #[tokio::test]
    async fn stop_node_and_teardown_stops_node_before_tearing_down_app() {
        let cancel_token = CancellationToken::new();
        let graceful_shutdown = CancellationToken::new();

        let stop_node = {
            let cancel_token = cancel_token.clone();
            let graceful_shutdown = graceful_shutdown.clone();
            async move {
                // The shutdown is marked graceful and the application loop is still live
                // while the Node is being stopped.
                assert!(graceful_shutdown.is_cancelled());
                assert!(!cancel_token.is_cancelled());
                Ok::<(), std::convert::Infallible>(())
            }
        };

        stop_node_and_teardown(stop_node, &cancel_token, &graceful_shutdown).await;

        assert!(graceful_shutdown.is_cancelled());
        assert!(cancel_token.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn drain_before_exit_persists_savepoint_then_drains() {
        let saved = std::cell::Cell::new(false);

        drain_before_exit(|| saved.set(true)).await;

        assert!(saved.get());
    }

    #[tokio::test]
    async fn stop_node_and_teardown_tears_down_when_node_stop_fails() {
        let cancel_token = CancellationToken::new();
        let graceful_shutdown = CancellationToken::new();

        let stop_node = async { Err::<(), &str>("node stop timed out") };

        stop_node_and_teardown(stop_node, &cancel_token, &graceful_shutdown).await;

        assert!(graceful_shutdown.is_cancelled());
        assert!(cancel_token.is_cancelled());
    }

    #[test]
    fn check_pending_proposals_bound_rejects_overflow() {
        let err = check_pending_proposals_bound(usize::MAX, 2).unwrap_err();
        assert!(err.to_string().contains("overflows usize"));
    }

    #[tokio::test]
    async fn start_requires_execution_engine() {
        let tmp = tempfile::tempdir().unwrap();
        let start_config = StartConfig {
            rpc_sync_enabled: true,
            rpc_sync_endpoints: vec!["http://unused:8545".parse().unwrap()],
            ..Default::default()
        };
        let mut app = App::new(
            Config::default(),
            tmp.path().to_path_buf(),
            tmp.path().join("unused.key"),
            start_config,
        );
        match app.start().await {
            Err(err) => assert!(
                format!("{err:?}").contains("engine"),
                "unexpected error: {err:?}"
            ),
            Ok(_) => panic!("should fail without execution engine"),
        }
    }

    fn write_key_file(dir: &std::path::Path) -> (PathBuf, PrivateKey) {
        let key = PrivateKey::generate(OsRng);
        let path = dir.join("priv_validator_key.json");
        let json = serde_json::to_string(&key).expect("serialize key");
        std::fs::write(&path, json).expect("write key file");
        (path, key)
    }

    fn test_app(private_key_file: PathBuf, validator: bool) -> App {
        let config = Config {
            moniker: "test-node".to_string(),
            ..Default::default()
        };
        let start_config = StartConfig {
            validator,
            ..Default::default()
        };
        App::new(
            config,
            PathBuf::from("/tmp"),
            private_key_file,
            start_config,
        )
    }

    #[tokio::test]
    async fn full_node_uses_ephemeral_consensus_identity() {
        let dir = tempdir().unwrap();
        let (key_path, original_key) = write_key_file(dir.path());

        let app = test_app(key_path, false);
        let identity = app.setup_node_identity().await.unwrap();

        // P2P identity uses the key from file
        let expected_keypair =
            Keypair::ed25519_from_bytes(original_key.inner().to_bytes()).unwrap();
        assert_eq!(
            identity.p2p.keypair.public().to_peer_id(),
            expected_keypair.public().to_peer_id(),
        );

        // Consensus identity is ephemeral (address differs from the file key)
        let file_provider = LocalSigningProvider::new(original_key);
        let file_address = Address::from_public_key(&file_provider.public_key());
        assert_ne!(identity.consensus.address(), file_address);
    }

    #[tokio::test]
    async fn validator_loads_consensus_identity_from_key_file() {
        let dir = tempdir().unwrap();
        let (key_path, original_key) = write_key_file(dir.path());
        let app = test_app(key_path, true);
        let identity = app.setup_node_identity().await.unwrap();

        // Both P2P and consensus derive from the same key file
        let file_provider = LocalSigningProvider::new(original_key);
        let file_address = Address::from_public_key(&file_provider.public_key());
        assert_eq!(identity.consensus.address(), file_address);
    }

    #[test]
    fn ephemeral_consensus_identity_generates_valid_identity() {
        let dir = tempdir().unwrap();
        let (key_path, _) = write_key_file(dir.path());

        let app = test_app(key_path, false);
        let id1 = app.ephemeral_consensus_identity();
        let id2 = app.ephemeral_consensus_identity();

        // Each call produces a different address
        assert_ne!(id1.address(), id2.address());
    }

    #[tokio::test]
    async fn resolve_chain_identity_returns_chain_and_genesis() {
        let mut eth = MockEthereumAPI::new();
        eth.expect_get_chain_id()
            .returning(|| Ok("0x539".to_string()));
        eth.expect_get_genesis_block()
            .returning(|| Ok(mock_genesis_block()));
        let engine = Engine::new(Box::new(MockEngineAPI::new()), Box::new(eth));

        let (chain_id, genesis) = resolve_chain_identity(&engine).await.unwrap();

        assert_eq!(chain_id, ChainId::Localdev); // 0x539 == 1337
        assert_eq!(genesis.block_number, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn resolve_with_reconnect_recovers_after_transient_el_failures() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let connect_calls = Arc::new(AtomicUsize::new(0));

        let connect = {
            let attempts = attempts.clone();
            let connect_calls = connect_calls.clone();
            move || {
                connect_calls.fetch_add(1, Ordering::SeqCst);
                let attempts = attempts.clone();
                async move {
                    let n = attempts.fetch_add(1, Ordering::SeqCst);
                    let mut eth = MockEthereumAPI::new();
                    if n < 2 {
                        // Held connection is to a terminating reth: reset before it
                        // answers.
                        eth.expect_get_chain_id().returning(|| {
                            Err(eyre::eyre!("Connection reset by peer (os error 104)"))
                        });
                    } else {
                        eth.expect_get_chain_id()
                            .returning(|| Ok("0x539".to_string()));
                        eth.expect_get_genesis_block()
                            .returning(|| Ok(mock_genesis_block()));
                    }
                    Ok(Engine::new(Box::new(MockEngineAPI::new()), Box::new(eth)))
                }
            }
        };

        let (_engine, chain_id, genesis) = connect_and_resolve_chain_identity(
            connect,
            Duration::from_secs(30),
            Duration::from_millis(10),
        )
        .await
        .expect("should resolve once the replacement EL serves");

        assert_eq!(chain_id, ChainId::Localdev);
        assert_eq!(genesis.block_number, 0);
        assert!(
            connect_calls.load(Ordering::SeqCst) >= 3,
            "each retry must rebuild the connection (reconnect), got {}",
            connect_calls.load(Ordering::SeqCst)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn resolve_with_reconnect_errors_when_el_unreachable_past_deadline() {
        let connect = || async {
            let mut eth = MockEthereumAPI::new();
            eth.expect_get_chain_id()
                .returning(|| Err(eyre::eyre!("Connection reset by peer (os error 104)")));
            Ok(Engine::new(Box::new(MockEngineAPI::new()), Box::new(eth)))
        };

        let err = connect_and_resolve_chain_identity(
            connect,
            Duration::from_secs(30),
            Duration::from_secs(1),
        )
        .await
        .err()
        .expect("should error when the EL never serves");

        assert!(
            is_execution_engine_unreachable(&err),
            "deadline error must carry the ExecutionEngineUnreachable marker: {err:?}"
        );
    }

    #[test]
    fn is_execution_engine_unreachable_classifies_marker() {
        // Marker wrapped in the same context layers `run()` observes (resolve, then
        // start).
        let tagged = eyre::Report::new(ExecutionEngineUnreachable)
            .wrap_err("Failed to resolve chain identity from execution engine")
            .wrap_err("Node failed to start");
        assert!(is_execution_engine_unreachable(&tagged));

        // An unrecoverable startup error (e.g. corrupt store) carries no marker.
        let other = eyre::eyre!("corrupt store").wrap_err("Node failed to start");
        assert!(!is_execution_engine_unreachable(&other));
    }
}
