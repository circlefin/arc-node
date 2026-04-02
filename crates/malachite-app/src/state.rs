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

//! Internal state of the application. This is a simplified abstract to keep it simple.
//! A regular application would have mempool implemented, a proper database and input methods like RPC.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use eyre::Context as _;

use malachitebft_app_channel::app::streaming::StreamId;
use malachitebft_app_channel::app::types::core::Round;

use arc_consensus_types::{
    Address, AlloyAddress, ArcContext, BlockHash, Config, ConsensusParams, Height, ValidatorSet,
};
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_signer::ArcSigningProvider;
use malachitebft_core_types::HeightParams;

use crate::block::ConsensusBlock;
use crate::env_config::EnvConfig;
use crate::metrics::app::AppMetrics;
use crate::node::ConsensusIdentity;
use crate::request::Status;
use crate::spec::{ChainId, ConsensusSpec, NetworkId};
use crate::stats::Stats;
use crate::store::Store;
use crate::streaming::PartStreamsMap;
use crate::utils::sync_state::SyncState;
use arc_consensus_types::proposal_monitor::ProposalMonitor;

/// Information needed to start the next height after a decision is reached.
#[derive(Debug)]
pub struct NextHeightInfo {
    /// The next height to move to after the current height is finalized.
    pub next_height: Height,
    /// The validator set for the next height.
    pub validator_set: ValidatorSet,
    /// The consensus parameters for the next height.
    pub consensus_params: ConsensusParams,
    /// The block that was decided at the current height.
    pub decided_block: ExecutionBlock,
    /// The target time for the next block to be proposed.
    pub target_time: Option<Duration>,
}

impl NextHeightInfo {
    /// Get the height parameters for the next height, which are used to start the next height in consensus.
    pub fn height_params(&self) -> HeightParams<ArcContext> {
        HeightParams::new(
            self.validator_set.clone(),
            self.consensus_params.timeouts(),
            self.target_time,
        )
    }
}

/// Represents whether or not a decision was successfully committed for the current height.
/// This is used to determine the appropriate next steps when a height is finalized,
/// such as whether to start the next height or restart the current height.
#[derive(Debug)]
pub enum Decision {
    /// Decision was sucessfully committed, and we have the information needed to start the next height.
    Success(Box<NextHeightInfo>),

    /// Processing the decided value failed for the given height and round.
    Failure(eyre::Report),
}

/// Represents the internal state of the application node
/// Contains information about current height, round, proposals and blocks
pub struct State {
    pub ctx: ArcContext,

    identity: ConsensusIdentity,
    validator_set: ValidatorSet,
    store: Store,
    stream_nonce: u32,
    streams_map: PartStreamsMap,
    config: Config,
    env_config: EnvConfig,
    stats: Stats,

    /// The genesis block of the execution layer, fetched at startup.
    genesis_block: ExecutionBlock,

    /// Computed network identifier: `keccak256(rlp(chain_id, genesis_hash, cl_fork_version))`.
    /// Recomputed at each height start because the fork version can change at fork boundaries.
    network_id: NetworkId,

    /// Address set by a validator to receive tips (transactions' priority fee) and
    /// rewards. The execution layer deposits fees and rewards to this address
    /// whenever the validator successfully proposes a new block. Not setting it
    /// to a valid address will result in losing the tips/rewards.
    suggested_fee_recipient: Address,

    /// Information about the current height, round, and proposer.
    pub current_height: Height,
    pub current_round: Round,
    pub current_proposer: Option<Address>,

    /// Whether the commit for the current height and round was successful or not,
    /// along with relevant information for next steps.
    pub decision: Option<Decision>,

    /// The current synchronization state of the node.
    pub sync_state: SyncState,

    /// The block that was decided at the previous height.
    pub previous_block: Option<ExecutionBlock>,

    /// Consensus parameters
    pub consensus_params: ConsensusParams,

    /// Monitor for tracking round-0 proposal timing and success
    pub proposal_monitor: Option<ProposalMonitor>,

    /// Timestamps of heights that received a synced value via ProcessSyncedValue.
    synced_heights: HashMap<Height, SystemTime>,

    /// Consensus-layer chain spec (fork activation by height/time).
    #[allow(dead_code)]
    pub spec: ConsensusSpec,

    /// Metrics for the application.
    pub metrics: AppMetrics,
}

#[bon::bon]
impl State {
    /// Creates a new State instance with the given validator address and starting height.
    ///
    /// # Example
    /// ```rust,ignore
    /// State::builder(ctx)
    ///     .identity(identity.consensus.clone())
    ///     .store(store.clone())
    ///     .config(self.config.clone())
    ///     .env_config(env_config)
    ///     .spec(consensus_spec)
    ///     .genesis_block(genesis_block)
    ///     .metrics(app_metrics)
    ///     .build();
    /// ```
    #[builder(finish_fn = build)]
    pub fn new(
        #[builder(start_fn)] ctx: ArcContext,
        identity: ConsensusIdentity,
        store: Store,
        config: Config,
        env_config: EnvConfig,
        spec: ConsensusSpec,
        genesis_block: ExecutionBlock,
        metrics: AppMetrics,
    ) -> Self {
        let initial_height = Height::new(0);
        let network_id = NetworkId::new(
            spec.chain_id,
            genesis_block.block_hash,
            spec.fork_version_at(initial_height, genesis_block.timestamp),
        );

        Self {
            ctx,
            identity,
            current_height: initial_height, // will be updated from reth
            current_round: Round::Nil,
            current_proposer: None,
            validator_set: ValidatorSet::default(), // initially empty, will be updated from reth
            store,
            stream_nonce: 0,
            streams_map: PartStreamsMap::new(initial_height),
            config,
            env_config,
            stats: Stats::default(),
            genesis_block,
            network_id,
            suggested_fee_recipient: AlloyAddress::ZERO.into(),
            decision: None,
            sync_state: SyncState::CatchingUp, // assume node is catching up at startup until we know more
            previous_block: None,
            consensus_params: ConsensusParams::default(),
            proposal_monitor: None,
            synced_heights: HashMap::new(),
            spec,
            metrics,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn env_config(&self) -> &EnvConfig {
        &self.env_config
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn metrics(&self) -> &AppMetrics {
        &self.metrics
    }

    /// Get the chain ID.
    pub fn chain_id(&self) -> ChainId {
        self.spec.chain_id
    }

    /// Get the computed network ID.
    #[allow(dead_code)]
    pub fn network_id(&self) -> NetworkId {
        self.network_id
    }

    /// Get the genesis block hash of the execution layer.
    #[allow(dead_code)]
    pub fn genesis_hash(&self) -> BlockHash {
        self.genesis_block.block_hash
    }

    /// Get the validator's address
    pub fn address(&self) -> Address {
        self.identity.address()
    }

    /// Get the signing provider
    pub fn signing_provider(&self) -> &ArcSigningProvider {
        self.identity.signing_provider()
    }

    // Get the current validator set
    pub fn validator_set(&self) -> &ValidatorSet {
        &self.validator_set
    }

    /// Get the current consensus parameters
    pub fn consensus_params(&self) -> &ConsensusParams {
        &self.consensus_params
    }

    /// Sets the current validator set and updates metrics
    pub fn set_validator_set(&mut self, val_set: ValidatorSet) {
        self.metrics.update_validator_set(&val_set);
        self.validator_set = val_set;
    }

    /// Sets the consensus parameters
    pub fn set_consensus_params(&mut self, consensus_params: ConsensusParams) {
        self.metrics.update_consensus_params(&consensus_params);
        self.consensus_params = consensus_params;
    }

    /// Get mutable reference to the streams map
    pub fn streams_map_mut(&mut self) -> &mut PartStreamsMap {
        &mut self.streams_map
    }

    /// Get the fee recipient address
    pub fn fee_recipient(&self) -> Address {
        self.suggested_fee_recipient
    }

    /// Set the fee recipient address
    pub fn set_suggested_fee_recipient(&mut self, fee_recipient: Address) {
        self.suggested_fee_recipient = fee_recipient;
    }

    /// Update metrics when starting a new height
    #[must_use]
    pub fn started_height(&mut self, height: Height, round: Round, proposer: Address) -> NetworkId {
        let elapsed = self.stats.height_started().elapsed();
        self.metrics.observe_block_time(elapsed.as_secs_f64());
        self.stats.set_height_started(Instant::now());

        self.streams_map.set_current_height(height);

        let network_id = self.recompute_network_id();

        self.init_proposal_monitor(round, proposer);

        network_id
    }

    /// Recompute the network ID from the current chain ID, genesis hash, and fork version.
    #[must_use]
    fn recompute_network_id(&mut self) -> NetworkId {
        let timestamp = self
            .previous_block
            .map(|b| b.timestamp)
            .unwrap_or(self.genesis_block.timestamp);
        let fork_version = self.spec.fork_version_at(self.current_height, timestamp);

        self.network_id =
            NetworkId::new(self.chain_id(), self.genesis_block.block_hash, fork_version);
        self.network_id
    }

    /// Initialize the proposal monitor for round 0.
    fn init_proposal_monitor(&mut self, round: Round, proposer: Address) {
        assert_eq!(round.as_i64(), 0);
        let height = self.current_height;

        let start_time = SystemTime::now();
        let mut monitor = ProposalMonitor::new(height, proposer, start_time);

        // If an early `ProcessSyncedValue` event was processed for this height,
        // use the associated recorded timestamp as proposal receive time.
        if let Some(synced_time) = self.synced_heights.remove(&height) {
            monitor.proposal_receive_time = Some(synced_time);
            monitor.mark_synced();
        }

        self.proposal_monitor = Some(monitor);
    }

    /// Mark a height as having received a synced value, storing the receive time.
    pub fn mark_height_synced(&mut self, height: Height) {
        let now = SystemTime::now();

        let Some(monitor) = &mut self.proposal_monitor else {
            self.synced_heights.insert(height, now);
            return;
        };
        if monitor.height != height {
            self.synced_heights.insert(height, now);
            return;
        }
        if monitor.proposal_receive_time.is_some() {
            // Normal proposals take precedence over synced values
            return;
        }
        monitor.proposal_receive_time = Some(now);
        monitor.mark_synced();
    }

    /// Clean up synced height tracking for past heights.
    pub fn cleanup_synced_heights(&mut self, current_height: Height) {
        self.synced_heights.retain(|h, _| *h >= current_height);
    }

    /// Maximum number of pending proposals allowed
    /// Defined to be equal to the size of the consensus input buffer,
    /// which is itself sized to handle all in-flight sync responses.
    pub fn max_pending_proposals(&self) -> usize {
        let limit = self.config.value_sync.parallel_requests * self.config.value_sync.batch_size;
        assert!(limit > 0, "max_pending_proposals must be greater than 0");
        limit
    }

    /// Return important current information.
    pub async fn get_status(&self) -> eyre::Result<Status> {
        let undecided_blocks_count = self
            .get_undecided_blocks(self.current_height, self.current_round)
            .await
            .wrap_err_with(|| {
                format!(
                    "Failed to get undecided blocks for height {} and round {} from the state",
                    self.current_height, self.current_round,
                )
            })?
            .len();

        let pending_proposal_parts = self
            .store
            .get_pending_proposal_parts_counts()
            .await
            .wrap_err("Failed to get pending proposal parts counts from the state")?;

        Ok(Status {
            height: self.current_height,
            round: self.current_round,
            address: self.address(),
            proposer: self.current_proposer,
            height_start_time: SystemTime::now() - self.stats.height_started().elapsed(),
            prev_payload_hash: self.previous_block.map(|b| b.block_hash),
            db_latest_height: self
                .store()
                .max_height()
                .await
                .wrap_err("Failed to get the latest height from the state")?
                .unwrap_or_default(),
            db_earliest_height: self
                .store()
                .min_height()
                .await
                .wrap_err("Failed to get earliest height from the state")?
                .unwrap_or_default(),
            undecided_blocks_count,
            pending_proposal_parts,
            validator_set: self.validator_set().to_owned(),
        })
    }

    /// Return unit type. Used to check the app is active.
    pub fn get_health(&self) {}

    /// Retrieves all undecided blocks at the given height and round.
    pub async fn get_undecided_blocks(
        &self,
        height: Height,
        round: Round,
    ) -> eyre::Result<Vec<ConsensusBlock>> {
        self
            .store
            .get_undecided_blocks(height, round)
            .await
            .wrap_err_with(||format!("Failed to get undecided blocks for height {height} and round {round} from the database"))
    }

    /// Move to the next height, updating the previous block, validator set, and consensus params.
    ///
    /// # Arguments
    /// * `info` - The information needed to move to the next height
    pub fn move_to_next_height(&mut self, info: NextHeightInfo) {
        // Move to next height
        self.current_height = info.next_height;
        self.current_round = Round::Nil;

        // Update the previous block to the block that was decided
        self.previous_block = Some(info.decided_block);

        // Update the validator set for the next height
        self.set_validator_set(info.validator_set);

        // Update the consensus params for the next height
        self.set_consensus_params(info.consensus_params);

        // Clean up synced heights tracking for past heights
        self.cleanup_synced_heights(info.next_height);
    }

    pub fn next_stream_id(&mut self) -> StreamId {
        let mut bytes = Vec::with_capacity(size_of::<u64>() + size_of::<u32>());
        bytes.extend_from_slice(&self.current_height.as_u64().to_be_bytes());
        bytes.extend_from_slice(
            &self
                .current_round
                .as_u32()
                .expect("round is always Some in active consensus")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&self.stream_nonce.to_be_bytes());
        self.stream_nonce += 1;
        StreamId::new(bytes.into())
    }

    pub async fn restart_height(
        &mut self,
        height: Height,
        validator_set: ValidatorSet,
        consensus_params: ConsensusParams,
    ) -> eyre::Result<()> {
        // Reset the state to that of the height prior to the given height being restarted
        self.current_height = height;
        self.current_round = Round::Nil;
        self.current_proposer = None;
        self.set_validator_set(validator_set);
        self.set_consensus_params(consensus_params);

        let previous_block_height = height.saturating_sub(1);

        self.previous_block = self
            .store
            .get_decided_block(previous_block_height)
            .await
            .wrap_err_with(|| format!(
                "Failed to retrieve previous block at height {previous_block_height} for restart at height {height}"
            ))?
            .map(|b| b.execution_payload.payload_inner.payload_inner)
            .map(|p| ExecutionBlock {
                block_hash: p.block_hash,
                block_number: p.block_number,
                parent_hash: p.parent_hash,
                timestamp: p.timestamp,
            });

        // Clean up any consensus data for the height that we are about to restart
        self.store
            .clean_stale_consensus_data(height)
            .await
            .wrap_err_with(|| {
                format!("Failed to clean stale consensus data for restart at height {height}")
            })?;

        // Update metrics
        self.metrics.inc_height_restart_count();

        Ok(())
    }

    /// Create a savepoint in the database to ensure the allocator state table is up to date.
    /// Doing this before shutting down the database can help avoid repair on next startup.
    pub fn savepoint(&self) {
        self.store.savepoint();
    }
}
