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
use tracing::warn;

use malachitebft_app_channel::app::streaming::StreamId;
use malachitebft_app_channel::app::types::core::Round;
use malachitebft_app_channel::app::types::ProposedValue;

use crate::streaming;

use arc_consensus_types::{
    signing::PublicKey, Address, AlloyAddress, ArcContext, BlockHash, ChainId, Config,
    ConsensusParams, ConsensusSpec, Height, NetworkId, ValidatorSet, ValueId,
};
use arc_eth_engine::json_structures::ExecutionBlock;
use arc_eth_engine::persistence_meter::{NoopPersistenceMeter, PersistenceMeter};
use arc_signer::ArcSigningProvider;
use malachitebft_core_types::HeightParams;

use crate::env_config::EnvConfig;
use crate::metrics::app::AppMetrics;
use crate::node::ConsensusIdentity;
use crate::request::Status;
use crate::stats::Stats;
use crate::store::Store;
use crate::streaming::PartStreamsMap;
use crate::utils::sync_state::SyncState;
use arc_consensus_types::proposal_monitor::ProposalMonitor;

/// A snapshot of the volatile consensus state fields needed to serve status queries.
///
/// This is published via a `tokio::sync::watch` channel after each consensus message,
/// so that the app-request task can read it without blocking the consensus loop.
///
/// The snapshot is eventually consistent with `State`: it is republished after the
/// consensus loop finishes handling a message, so a reader may observe a snapshot
/// that is up to one consensus-message stale. Fields derived from it (e.g. the
/// `(height, round)` used to query `get_undecided_blocks`) inherit that staleness.
/// This is acceptable for the `/status` RPC — strict freshness would require
/// re-coupling the request handler to the consensus loop.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusSnapshot {
    pub height: Height,
    pub round: Round,
    pub proposer: Option<Address>,
    pub address: Address,
    pub public_key: PublicKey,
    pub previous_block: Option<ExecutionBlock>,
    pub validator_set: ValidatorSet,
    pub sync_state: SyncState,
}

impl StatusSnapshot {
    /// Build the full `Status` response from this snapshot plus live DB/stats queries.
    pub async fn get_status(&self, store: &Store, stats: &Stats) -> eyre::Result<Status> {
        let undecided_blocks_count = store
            .get_undecided_blocks(self.height, self.round)
            .await
            .wrap_err_with(|| {
                format!(
                    "Failed to get undecided blocks for height {} and round {} from the state",
                    self.height, self.round,
                )
            })?
            .len();

        let db_latest_height = store
            .max_height()
            .await
            .wrap_err("Failed to get the latest height from the state")?
            .unwrap_or_default();

        let db_earliest_height = store
            .min_height()
            .await
            .wrap_err("Failed to get earliest height from the state")?
            .unwrap_or_default();

        let pending_proposal_parts = store
            .get_pending_proposal_parts_counts()
            .await
            .wrap_err("Failed to get pending proposal parts counts from the state")?;

        Ok(Status {
            height: self.height,
            round: self.round,
            address: self.address,
            public_key: self.public_key,
            proposer: self.proposer,
            // elapsed() is always <= time since epoch, so this won't underflow
            #[allow(clippy::arithmetic_side_effects)]
            height_start_time: SystemTime::now() - stats.height_started().elapsed(),
            prev_payload_hash: self.previous_block.map(|b| b.block_hash),
            db_latest_height,
            db_earliest_height,
            undecided_blocks_count,
            pending_proposal_parts,
            validator_set: self.validator_set.clone(),
            sync_state: self.sync_state,
        })
    }
}

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

/// Proposal-monitor data captured for a height before its monitor exists.
///
/// Proposal events can reach the application before round 0 of the
/// corresponding height has started locally:
///   * a `ProcessSyncedValue` is processed for a height the node has not
///     yet entered (note: today this branch is not reachable under the
///     current sync protocol, but the stash is kept for robustness);
///   * gossip-delivered proposal parts complete a payload for height `H+1`
///     while the node is still wrapping up height `H`.
///
/// Entries are consumed by [`State::init_proposal_monitor`] when round 0 of
/// the matching height starts.
#[derive(Clone, Debug)]
enum EarlyArrival {
    /// A synced value was processed for this height.
    Synced(SystemTime),
    /// Proposal parts for this height were buffered as pending before the
    /// monitor existed.
    PendingPartsTime(SystemTime),
}

/// Pre-populate a freshly-built monitor from a stashed early arrival.
fn apply_early_arrival(monitor: &mut ProposalMonitor, early: EarlyArrival) {
    match early {
        EarlyArrival::Synced(t) => {
            monitor.proposal_receive_time = Some(t);
            monitor.mark_synced();
        }
        // `synced` stays false and `value_id` stays `None`: the value id is
        // filled in by `attach_value_id_to_monitor` at reassembly.
        EarlyArrival::PendingPartsTime(t) => monitor.proposal_receive_time = Some(t),
    }
}

/// Build the round-0 monitor for `height`, consuming any early arrival stashed
/// for it (which yields the negative delay).
fn build_monitor(
    height: Height,
    proposer: Address,
    start_time: SystemTime,
    early: Option<EarlyArrival>,
) -> ProposalMonitor {
    let mut monitor = ProposalMonitor::new(height, proposer, start_time);
    if let Some(early) = early {
        apply_early_arrival(&mut monitor, early);
    }
    monitor
}

/// Stash an early arrival for `height`, keeping the first arrival except that a
/// pending-parts arrival supersedes a (dormant) synced one.
fn stash_early_arrival(
    early_arrivals: &mut HashMap<Height, EarlyArrival>,
    height: Height,
    arrival: EarlyArrival,
) {
    let supersedes = |existing: &EarlyArrival| {
        matches!(
            (&arrival, existing),
            (EarlyArrival::PendingPartsTime(_), EarlyArrival::Synced(_))
        )
    };
    if early_arrivals.get(&height).is_none_or(supersedes) {
        early_arrivals.insert(height, arrival);
    }
}

/// Record a synced value's receive time: directly on the monitor when it exists
/// for `height` (unless a normal proposal already set it), otherwise stashed.
fn record_synced(
    monitor: &mut Option<ProposalMonitor>,
    early_arrivals: &mut HashMap<Height, EarlyArrival>,
    height: Height,
    now: SystemTime,
) {
    match monitor {
        Some(m) if m.height == height => {
            if m.proposal_receive_time.is_some() {
                // Normal proposals take precedence over synced values
                return;
            }
            m.proposal_receive_time = Some(now);
            m.mark_synced();
        }
        _ => stash_early_arrival(early_arrivals, height, EarlyArrival::Synced(now)),
    }
}

/// Attach an assembled value id to the monitor, if it is the round-0 monitor
/// for `height`. A missing or mismatched monitor is an invariant violation
/// here (the monitor is created at round-0 start, before this runs), so it is
/// logged.
fn attach_value_id_to_monitor(
    monitor: &mut Option<ProposalMonitor>,
    height: Height,
    value_id: ValueId,
) {
    let Some(m) = monitor.as_mut() else {
        warn!(%height, "attach_assembled_value_id: no proposal monitor present");
        return;
    };
    if m.height != height {
        warn!(%height, monitor.height = %m.height, "attach_assembled_value_id: monitor height mismatch");
        return;
    }
    m.attach_assembled_value_id(value_id);
}

/// Attach the value id of every valid round-0 proposal to the monitor. No-op
/// off round 0; engine-invalid proposals are skipped (they can never be the
/// decided value).
pub(crate) fn attach_valid_proposal_value_ids(
    monitor: &mut Option<ProposalMonitor>,
    height: Height,
    round: Round,
    proposals: &[ProposedValue<ArcContext>],
) {
    if round.as_i64() != 0 {
        return;
    }
    for p in proposals.iter().filter(|p| p.validity.is_valid()) {
        attach_value_id_to_monitor(monitor, height, p.value.id());
    }
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

    /// Proposal events recorded for heights before their proposal monitor was
    /// initialized. Entries are consumed by [`Self::init_proposal_monitor`]
    /// when round 0 of the matching height starts.
    early_arrivals: HashMap<Height, EarlyArrival>,

    /// Meters EL block persistence to apply backpressure during sync catch-up.
    persistence_meter: Box<dyn PersistenceMeter>,

    /// Consensus-layer chain spec (fork activation by height).
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
            spec.fork_version_at(initial_height),
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
            streams_map: PartStreamsMap::new(initial_height, 0),
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
            early_arrivals: HashMap::new(),
            persistence_meter: Box::new(NoopPersistenceMeter),
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
        self.streams_map.set_num_validators(val_set.len());
        self.validator_set = val_set;
    }

    /// Sets the consensus parameters
    pub fn set_consensus_params(&mut self, consensus_params: ConsensusParams) {
        self.metrics.update_consensus_params(&consensus_params);
        self.consensus_params = consensus_params;
    }

    pub fn persistence_meter(&self) -> &dyn PersistenceMeter {
        self.persistence_meter.as_ref()
    }

    pub fn set_persistence_meter(&mut self, meter: Box<dyn PersistenceMeter>) {
        self.persistence_meter = meter;
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
        let fork_version = self.spec.fork_version_at(self.current_height);

        self.network_id =
            NetworkId::new(self.chain_id(), self.genesis_block.block_hash, fork_version);
        self.network_id
    }

    /// Initialize the proposal monitor for round 0, consuming any
    /// [`EarlyArrival`] stashed for this height (which yields the negative delay).
    fn init_proposal_monitor(&mut self, round: Round, proposer: Address) {
        assert_eq!(round.as_i64(), 0);
        let height = self.current_height;
        let early = self.early_arrivals.remove(&height);
        self.proposal_monitor = Some(build_monitor(height, proposer, SystemTime::now(), early));
    }

    /// Mark a height as having received a synced value, storing the receive time.
    pub fn mark_height_synced(&mut self, height: Height) {
        record_synced(
            &mut self.proposal_monitor,
            &mut self.early_arrivals,
            height,
            SystemTime::now(),
        );
    }

    /// Record the receive time of proposal parts buffered as pending for a
    /// future `height`. The value id is attached later, at reassembly.
    pub fn record_early_pending_parts_time(&mut self, height: Height) {
        stash_early_arrival(
            &mut self.early_arrivals,
            height,
            EarlyArrival::PendingPartsTime(SystemTime::now()),
        );
    }

    /// Drop early-arrival entries for heights below `current_height`.
    pub fn cleanup_early_arrivals(&mut self, current_height: Height) {
        self.early_arrivals.retain(|h, _| *h >= current_height);
    }

    /// Maximum number of pending proposals allowed
    /// Defined to be equal to the size of the consensus input buffer,
    /// which is itself sized to handle all in-flight sync responses.
    pub fn max_pending_proposals(&self) -> usize {
        let limit = self
            .config
            .value_sync
            .parallel_requests
            .checked_mul(self.config.value_sync.batch_size)
            .expect("max_pending_proposals overflow");
        assert!(limit > 0, "max_pending_proposals must be greater than 0");
        limit
    }

    /// Create a snapshot of the volatile consensus state fields.
    pub fn status_snapshot(&self) -> StatusSnapshot {
        StatusSnapshot {
            height: self.current_height,
            round: self.current_round,
            proposer: self.current_proposer,
            address: self.address(),
            public_key: *self.identity.public_key(),
            previous_block: self.previous_block,
            validator_set: self.validator_set().clone(),
            sync_state: self.sync_state,
        }
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

        // Clean up early-arrival entries for past heights
        self.cleanup_early_arrivals(info.next_height);
    }

    /// Build the next stream ID for a proposal at the given `height` and `round`.
    ///
    /// The height and round come from the proposal being streamed, so the
    /// stream_id agrees with the proposal's `Init` part by construction.
    pub fn next_stream_id(&mut self, height: Height, round: Round) -> StreamId {
        let nonce = self.stream_nonce;
        // Stream nonce increases monotonically; collision unreachable within a session
        #[allow(clippy::arithmetic_side_effects)]
        {
            self.stream_nonce += 1;
        }
        streaming::new_stream_id(height, round, nonce)
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

#[cfg(test)]
mod tests {
    use super::*;
    use arc_consensus_types::{Value, B256};
    use malachitebft_app_channel::app::types::core::Validity;

    fn test_address() -> Address {
        Address::new([0x42; 20])
    }

    fn test_value_id(seed: u8) -> ValueId {
        ValueId::new(B256::repeat_byte(seed))
    }

    fn test_proposed_value(
        height: Height,
        round: Round,
        proposer: Address,
        validity: Validity,
        seed: u8,
    ) -> ProposedValue<ArcContext> {
        ProposedValue {
            height,
            round,
            valid_round: Round::Nil,
            proposer,
            value: Value::new(BlockHash::repeat_byte(seed)),
            validity,
        }
    }

    #[test]
    fn apply_early_arrival_synced_marks_synced_and_sets_time() {
        let start = SystemTime::now();
        let mut monitor = ProposalMonitor::new(Height::new(1), test_address(), start);

        let earlier = start - Duration::from_millis(150);
        apply_early_arrival(&mut monitor, EarlyArrival::Synced(earlier));

        assert_eq!(monitor.proposal_receive_time, Some(earlier));
        assert!(monitor.synced);
        assert!(monitor.value_id.is_none());
    }

    #[test]
    fn apply_early_arrival_pending_parts_sets_time_leaves_synced_false_and_value_unset() {
        let start = SystemTime::now();
        let mut monitor = ProposalMonitor::new(Height::new(1), test_address(), start);

        let earlier = start - Duration::from_millis(150);
        apply_early_arrival(&mut monitor, EarlyArrival::PendingPartsTime(earlier));

        assert_eq!(monitor.proposal_receive_time, Some(earlier));
        assert!(!monitor.synced);
        // value id is filled in later, by attach_assembled_value_id.
        assert!(monitor.value_id.is_none());
    }

    #[test]
    fn apply_early_arrival_pending_parts_yields_negative_delay_when_earlier_than_start() {
        // The proposal arrived *before* round-0 start, so the delay computed downstream
        // from `(proposal_receive_time - start_time)` must be negative.
        let start = SystemTime::now();
        let mut monitor = ProposalMonitor::new(Height::new(7), test_address(), start);

        let earlier = start - Duration::from_millis(200);
        apply_early_arrival(&mut monitor, EarlyArrival::PendingPartsTime(earlier));

        let receive = monitor.proposal_receive_time.expect("set above");
        assert!(
            receive < monitor.start_time,
            "proposal_receive_time should precede start_time"
        );
    }

    #[test]
    fn pending_parts_then_attach_then_decide_marks_successful() {
        // End-to-end of the two-phase pending-parts recording.
        let start = SystemTime::now();
        let mut monitor = ProposalMonitor::new(Height::new(7), test_address(), start);

        // Phase 1: receive time applied at round-0 start (negative delay).
        apply_early_arrival(
            &mut monitor,
            EarlyArrival::PendingPartsTime(start - Duration::from_millis(100)),
        );
        assert!(monitor.value_id.is_none());

        // Phase 2: value id attached after reassembly.
        let value = test_value_id(0xDE);
        monitor.attach_assembled_value_id(value);
        assert_eq!(monitor.value_id, Some(value));
        assert!(!monitor.synced);

        monitor.mark_decided(&value);
        assert!(monitor.successful.is_successful());
    }

    #[test]
    fn build_monitor_without_early_arrival_is_plain() {
        let m = build_monitor(Height::new(3), test_address(), SystemTime::now(), None);
        assert_eq!(m.height, Height::new(3));
        assert!(m.proposal_receive_time.is_none());
        assert!(m.value_id.is_none());
        assert!(!m.synced);
    }

    #[test]
    fn build_monitor_consumes_pending_parts_arrival() {
        let start = SystemTime::now();
        let earlier = start - Duration::from_millis(90);
        let m = build_monitor(
            Height::new(3),
            test_address(),
            start,
            Some(EarlyArrival::PendingPartsTime(earlier)),
        );
        assert_eq!(m.proposal_receive_time, Some(earlier));
        assert!(!m.synced);
    }

    #[test]
    fn stash_early_arrival_precedence() {
        let h = Height::new(5);

        // Vacant -> insert.
        let mut map = HashMap::new();
        stash_early_arrival(&mut map, h, EarlyArrival::Synced(SystemTime::now()));
        assert!(matches!(map.get(&h), Some(EarlyArrival::Synced(_))));

        // Pending-parts supersedes a stashed synced.
        stash_early_arrival(
            &mut map,
            h,
            EarlyArrival::PendingPartsTime(SystemTime::now()),
        );
        assert!(matches!(
            map.get(&h),
            Some(EarlyArrival::PendingPartsTime(_))
        ));

        // Synced does NOT displace a stashed pending-parts.
        stash_early_arrival(&mut map, h, EarlyArrival::Synced(SystemTime::now()));
        assert!(matches!(
            map.get(&h),
            Some(EarlyArrival::PendingPartsTime(_))
        ));

        // Same-kind keeps the first (earliest) entry.
        let first = SystemTime::now() - Duration::from_millis(50);
        let mut map2 = HashMap::new();
        stash_early_arrival(&mut map2, h, EarlyArrival::PendingPartsTime(first));
        stash_early_arrival(
            &mut map2,
            h,
            EarlyArrival::PendingPartsTime(SystemTime::now()),
        );
        match map2.get(&h) {
            Some(EarlyArrival::PendingPartsTime(t)) => assert_eq!(*t, first),
            other => panic!("expected first PendingPartsTime, got {other:?}"),
        }
    }

    #[test]
    fn record_synced_direct_when_monitor_matches_and_unset() {
        let mut monitor = Some(ProposalMonitor::new(
            Height::new(4),
            test_address(),
            SystemTime::now(),
        ));
        let mut map = HashMap::new();
        let now = SystemTime::now();

        record_synced(&mut monitor, &mut map, Height::new(4), now);

        let m = monitor.unwrap();
        assert_eq!(m.proposal_receive_time, Some(now));
        assert!(m.synced);
        assert!(map.is_empty(), "should not stash when monitor matches");
    }

    #[test]
    fn record_synced_does_not_override_existing_receive_time() {
        let mut monitor = Some(ProposalMonitor::new(
            Height::new(4),
            test_address(),
            SystemTime::now(),
        ));
        let earlier = SystemTime::now() - Duration::from_millis(10);
        monitor.as_mut().unwrap().proposal_receive_time = Some(earlier);
        let mut map = HashMap::new();

        record_synced(&mut monitor, &mut map, Height::new(4), SystemTime::now());

        // Normal proposal already recorded -> synced does not overwrite.
        assert_eq!(monitor.unwrap().proposal_receive_time, Some(earlier));
    }

    #[test]
    fn record_synced_stashes_when_monitor_absent_or_wrong_height() {
        // No monitor -> stash.
        let mut monitor = None;
        let mut map = HashMap::new();
        record_synced(&mut monitor, &mut map, Height::new(9), SystemTime::now());
        assert!(matches!(
            map.get(&Height::new(9)),
            Some(EarlyArrival::Synced(_))
        ));

        // Monitor for a different height -> stash.
        let mut monitor = Some(ProposalMonitor::new(
            Height::new(8),
            test_address(),
            SystemTime::now(),
        ));
        let mut map = HashMap::new();
        record_synced(&mut monitor, &mut map, Height::new(9), SystemTime::now());
        assert!(matches!(
            map.get(&Height::new(9)),
            Some(EarlyArrival::Synced(_))
        ));
    }

    #[test]
    fn attach_value_id_to_monitor_sets_when_matching() {
        let mut monitor = Some(ProposalMonitor::new(
            Height::new(2),
            test_address(),
            SystemTime::now(),
        ));
        let v = test_value_id(0x55);
        attach_value_id_to_monitor(&mut monitor, Height::new(2), v);
        assert_eq!(monitor.unwrap().value_id, Some(v));
    }

    #[test]
    fn attach_value_id_to_monitor_noop_when_absent_or_mismatched() {
        // Absent monitor: no panic, nothing to assert beyond "still None".
        let mut monitor: Option<ProposalMonitor> = None;
        attach_value_id_to_monitor(&mut monitor, Height::new(2), test_value_id(0x55));
        assert!(monitor.is_none());

        // Wrong height: value id not set.
        let mut monitor = Some(ProposalMonitor::new(
            Height::new(2),
            test_address(),
            SystemTime::now(),
        ));
        attach_value_id_to_monitor(&mut monitor, Height::new(3), test_value_id(0x55));
        assert!(monitor.unwrap().value_id.is_none());
    }

    #[test]
    fn attach_valid_proposal_value_ids_round0_attaches_valid_skips_invalid() {
        let height = Height::new(6);
        let proposer = test_address();

        // Monitor initialized from an early pending-parts arrival (value_id unset).
        let mut monitor = Some(build_monitor(
            height,
            proposer,
            SystemTime::now(),
            Some(EarlyArrival::PendingPartsTime(
                SystemTime::now() - Duration::from_millis(50),
            )),
        ));

        // An invalid proposal must not be recorded.
        let invalid = test_proposed_value(height, Round::new(0), proposer, Validity::Invalid, 0x11);
        attach_valid_proposal_value_ids(&mut monitor, height, Round::new(0), &[invalid]);
        assert!(monitor.as_ref().unwrap().value_id.is_none());

        // A valid proposal is recorded.
        let valid = test_proposed_value(height, Round::new(0), proposer, Validity::Valid, 0x22);
        attach_valid_proposal_value_ids(&mut monitor, height, Round::new(0), &[valid]);
        assert_eq!(monitor.unwrap().value_id, Some(test_value_id(0x22)));
    }

    #[test]
    fn attach_valid_proposal_value_ids_noop_off_round0() {
        let height = Height::new(6);
        let proposer = test_address();
        let mut monitor = Some(ProposalMonitor::new(height, proposer, SystemTime::now()));

        let valid = test_proposed_value(height, Round::new(1), proposer, Validity::Valid, 0x22);
        attach_valid_proposal_value_ids(&mut monitor, height, Round::new(1), &[valid]);

        assert!(monitor.unwrap().value_id.is_none());
    }
}
