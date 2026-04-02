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

use std::collections::HashSet;
use std::fmt::Write;
use std::ops::Deref;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use arc_consensus_types::ConsensusParams;
use arc_consensus_types::{Address, ValidatorSet};
use malachitebft_app_channel::app::metrics::prometheus::encoding::{
    EncodeLabelSet, EncodeLabelValue, LabelValueEncoder,
};
use malachitebft_app_channel::app::metrics::prometheus::metrics::counter::Counter;
use malachitebft_app_channel::app::metrics::prometheus::metrics::family::Family;
use malachitebft_app_channel::app::metrics::prometheus::metrics::gauge::Gauge;
use malachitebft_app_channel::app::metrics::prometheus::metrics::histogram::{
    exponential_buckets, exponential_buckets_range, Histogram,
};
use malachitebft_app_channel::app::metrics::prometheus::metrics::info::Info;
use malachitebft_app_channel::app::metrics::SharedRegistry;

/// Metrics for the database.
/// Metrics for the application.
#[derive(Clone, Debug)]
pub struct AppMetrics(Arc<Inner>);

impl Deref for AppMetrics {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Inner struct for database metrics.
/// Inner struct for application metrics.
#[derive(Debug)]
pub struct Inner {
    /// Time between two blocks
    block_time: Histogram,

    /// Time taken to finalize a block
    block_finalize_time: Histogram,

    /// Time taken to build a block
    block_build_time: Histogram,

    /// Number of transactions in each finalized block
    block_transactions_count: Histogram,

    /// Size of each finalized block in bytes
    block_size_bytes: Histogram,

    /// Gas used in each finalized block
    block_gas_used: Histogram,

    /// Total number of transactions finalized since start
    total_transactions_count: Counter,

    /// Total size of all finalized blocks in bytes since start
    total_chain_bytes: Counter,

    /// Size of the validator set
    validators_count: Gauge,

    /// Total voting power of the validator set
    validators_total_voting_power: Gauge,

    /// Voting power of each validator
    validator_voting_power: Family<AddressLabel, Gauge>,

    /// Time taken to process a message
    msg_process_time: Family<ProcessMsgLabel, Histogram>,

    /// Time taken for each Engine API call
    engine_api_time: Family<EngineApiLabel, Histogram>,

    /// The number of times the consensus height has been restarted
    height_restart_count: Counter,

    /// Number of times the node fell behind and transitioned from InSync to CatchingUp
    sync_fell_behind_count: Counter,

    /// Number of pending proposal parts waiting to be processed at a future height or round
    pending_proposal_parts_count: Gauge,

    /// Consensus parameters
    consensus_params: Family<ConsensusParamsLabel, Gauge<f64, AtomicU64>>,

    /// Number of blocks replayed from CL to EL during startup handshake
    handshake_replay_blocks: Gauge<u64, AtomicU64>,

    /// Internal state recording the previous validator set.
    /// Useful field to manage validators' metrics.
    /// This field is only accessible internally, and is not a metrics itself.
    previous_validator_set: RwLock<HashSet<AddressLabel>>,
}

impl Inner {
    /// Create a new `Inner` struct.
    pub fn new() -> Self {
        Self {
            block_time: Histogram::new(exponential_buckets_range(0.01, 2.0, 10)),
            block_finalize_time: Histogram::new(exponential_buckets_range(0.01, 2.0, 10)),
            block_build_time: Histogram::new(exponential_buckets_range(0.01, 2.0, 10)),
            block_transactions_count: Histogram::new(exponential_buckets(1.0, 2.0, 15)), // 1, 2, 4, .. , 16384 txs
            block_size_bytes: Histogram::new(exponential_buckets(1000.0, 2.0, 21)), // 1KB, 2KB, 4KB, .. , 1GB
            block_gas_used: Histogram::new(exponential_buckets(1000.0, 2.0, 16)), // 1K, 2K, 4K, .. , 32M gas (fixed: was 15, now 16 buckets)
            total_transactions_count: Counter::default(),
            total_chain_bytes: Counter::default(),
            validators_count: Gauge::default(),
            validators_total_voting_power: Gauge::default(),
            validator_voting_power: Family::default(),
            previous_validator_set: RwLock::new(HashSet::new()),
            msg_process_time: Family::new_with_constructor(|| {
                Histogram::new(exponential_buckets_range(0.01, 2.0, 10))
            }),
            engine_api_time: Family::new_with_constructor(|| {
                Histogram::new(exponential_buckets_range(0.001, 2.0, 10))
            }),
            height_restart_count: Counter::default(),
            sync_fell_behind_count: Counter::default(),
            pending_proposal_parts_count: Gauge::default(),
            consensus_params: Family::default(),
            handshake_replay_blocks: Gauge::default(),
        }
    }
}

impl Default for Inner {
    fn default() -> Self {
        Self::new()
    }
}

impl AppMetrics {
    /// Create a new `AppMetrics` struct.
    pub fn new() -> Self {
        Self(Arc::new(Inner::new()))
    }

    /// Register the metrics with the given registry.
    pub fn register(registry: &SharedRegistry) -> Self {
        let metrics = Self::new();

        registry.with_prefix("arc_malachite_app", |registry| {
            registry.register(
                "block_time",
                "Interval between two blocks, in seconds",
                metrics.block_time.clone(),
            );

            registry.register(
                "block_finalize_time",
                "Time taken to finalize a block, in seconds",
                metrics.block_finalize_time.clone(),
            );

            registry.register(
                "block_build_time",
                "Time taken to build a block, in seconds",
                metrics.block_build_time.clone(),
            );

            registry.register(
                "block_transactions_count",
                "Number of transactions in each finalized block",
                metrics.block_transactions_count.clone(),
            );

            registry.register(
                "block_size_bytes",
                "Size of each finalized block in bytes",
                metrics.block_size_bytes.clone(),
            );

            registry.register(
                "block_gas_used",
                "Gas used in each finalized block",
                metrics.block_gas_used.clone(),
            );

            registry.register(
                "total_transactions_count",
                "Total number of transactions finalized since start",
                metrics.total_transactions_count.clone(),
            );

            registry.register(
                "total_chain_bytes",
                "Total size of all finalized blocks in bytes since start",
                metrics.total_chain_bytes.clone(),
            );

            registry.register(
                "validators_count",
                "Number of validators in the current validator set",
                metrics.validators_count.clone(),
            );

            registry.register(
                "validators_total_voting_power",
                "Total voting power of the current validator set",
                metrics.validators_total_voting_power.clone(),
            );

            registry.register(
                "validator_voting_power",
                "Voting power of each validator",
                metrics.validator_voting_power.clone(),
            );

            registry.register(
                "consensus_params",
                "Consensus parameters",
                metrics.consensus_params.clone(),
            );

            registry.register(
                "msg_process_time",
                "Time taken to process a message, in seconds",
                metrics.msg_process_time.clone(),
            );

            registry.register(
                "engine_api_time",
                "Time taken for each Engine API call, in seconds",
                metrics.engine_api_time.clone(),
            );

            registry.register(
                "height_restart_count",
                "The number of times the consensus height has been restarted",
                metrics.height_restart_count.clone(),
            );

            registry.register(
                "sync_fell_behind_count",
                "Number of times the node fell behind and transitioned from InSync to CatchingUp",
                metrics.sync_fell_behind_count.clone(),
            );

            registry.register(
                "pending_proposal_parts_count",
                "Number of pending proposal parts waiting to be processed at a future height or round",
                metrics.pending_proposal_parts_count.clone(),
            );

            registry.register(
                "handshake_replay_blocks",
                "Number of blocks replayed from CL to EL during startup handshake",
                metrics.handshake_replay_blocks.clone(),
            );

            // Register version info as a separate Info metric
            let version_info = Info::new(VersionInfoLabel {
                version: arc_version::SHORT_VERSION,
                git_commit: arc_version::GIT_COMMIT_HASH,
            });
            registry.register(
                "version", // NOTE: The Prometheus exporter will add an `_info` suffix automatically
                "Version information for the consensus layer",
                version_info,
            );

        });

        metrics
    }

    /// Observe the time between two blocks, in seconds.
    pub fn observe_block_time(&self, seconds: f64) {
        self.block_time.observe(seconds);
    }

    /// Observe the time taken to finalize a block, in seconds.
    pub fn observe_block_finalize_time(&self, seconds: f64) {
        self.block_finalize_time.observe(seconds);
    }

    /// Observe the time taken to build a block, in seconds.
    pub fn observe_block_build_time(&self, seconds: f64) {
        self.block_build_time.observe(seconds);
    }

    /// Observe the number of transactions in a finalized block.
    pub fn observe_block_transactions_count(&self, count: u64) {
        self.block_transactions_count.observe(count as f64);
    }

    /// Observe the size of a finalized block in bytes.
    pub fn observe_block_size_bytes(&self, size: u64) {
        self.block_size_bytes.observe(size as f64);
    }

    /// Observe the gas used in a finalized block.
    pub fn observe_block_gas_used(&self, gas: u64) {
        self.block_gas_used.observe(gas as f64);
    }

    /// Increment the total number of transactions finalized.
    pub fn inc_total_transactions_count(&self, count: u64) {
        self.total_transactions_count.inc_by(count);
    }

    /// Increment the total size of all finalized blocks in bytes.
    pub fn inc_total_chain_bytes(&self, size: u64) {
        self.total_chain_bytes.inc_by(size);
    }

    /// Update metrics related to the active validator set.
    pub fn update_validator_set(&self, validator_set: &ValidatorSet) {
        self.validators_count.set(validator_set.len() as i64);

        self.validators_total_voting_power
            .set(validator_set.total_voting_power() as i64);

        // Update the voting power of the current validator set
        let mut current_validators = HashSet::new();
        for validator in validator_set.iter() {
            let label = AddressLabel::new(validator.address);

            current_validators.insert(label);

            self.validator_voting_power
                .get_or_create(&label)
                .set(validator.voting_power as i64);
        }

        // Set the validator power of validators that are no longer in the validator
        // set to 0.
        let mut prev_validators = self
            .previous_validator_set
            .write()
            .expect("lock poisoning is unrecoverable");
        for removed_validator in prev_validators.difference(&current_validators) {
            self.validator_voting_power
                .get_or_create(removed_validator)
                .set(0);
        }

        *prev_validators = current_validators;
    }

    /// Update consensus parameters
    pub fn update_consensus_params(&self, p: &ConsensusParams) {
        let data = [
            ("timeout_propose", p.timeouts().propose),
            ("timeout_propose_delta", p.timeouts().propose_delta),
            ("timeout_prevote", p.timeouts().prevote),
            ("timeout_prevote_delta", p.timeouts().prevote_delta),
            ("timeout_precommit", p.timeouts().precommit),
            ("timeout_precommit_delta", p.timeouts().precommit_delta),
            ("timeout_rebroadcast", p.timeouts().rebroadcast),
            (
                "target_block_time",
                p.target_block_time().unwrap_or_default(),
            ),
        ];

        for (param, duration) in data.iter() {
            self.consensus_params
                .get_or_create(&ConsensusParamsLabel { param })
                .set(duration.as_secs_f64());
        }
    }

    /// Start a timer for processing a message.
    ///
    /// The returned guard will record the time taken to process the message when dropped.
    #[must_use]
    pub fn start_msg_process_timer(&self, msg: &'static str) -> MetricsGuard {
        MetricsGuard::new(self.clone(), msg, |metrics, msg, elapsed| {
            metrics
                .msg_process_time
                .get_or_create(&ProcessMsgLabel::new(msg))
                .observe(elapsed.as_secs_f64());
        })
    }

    /// Start a timer for an Engine API call.
    ///
    /// The returned guard will record the time taken for the API call when dropped.
    #[must_use]
    pub fn start_engine_api_timer(&self, api: &'static str) -> MetricsGuard {
        MetricsGuard::new(self.clone(), api, |metrics, api, elapsed| {
            metrics
                .engine_api_time
                .get_or_create(&EngineApiLabel::new(api))
                .observe(elapsed.as_secs_f64());
        })
    }

    /// Increment the number of times the consensus height has been restarted
    pub fn inc_height_restart_count(&self) {
        self.height_restart_count.inc();
    }

    /// Increment the number of times the node fell behind (transitioned from InSync to CatchingUp)
    pub fn inc_sync_fell_behind_count(&self) {
        self.sync_fell_behind_count.inc();
    }

    /// Observe the number of pending proposal parts
    pub fn observe_pending_proposal_parts_count(&self, count: usize) {
        self.pending_proposal_parts_count.set(count as i64);
    }

    /// Set the number of blocks replayed during the startup handshake.
    pub fn set_handshake_replay_blocks(&self, count: u64) {
        self.handshake_replay_blocks.set(count);
    }

    /// Get the number of blocks replayed during the startup handshake.
    #[cfg(test)]
    pub fn get_handshake_replay_blocks(&self) -> u64 {
        self.handshake_replay_blocks.get()
    }
}

impl Default for AppMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// This wrapper allows us to derive `AsLabelValue` for any type without running into Rust orphan rules.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct AsLabelValue<T>(T);

impl EncodeLabelValue for AsLabelValue<Address> {
    fn encode(&self, encoder: &mut LabelValueEncoder) -> Result<(), std::fmt::Error> {
        encoder.write_fmt(format_args!("{}", self.0))
    }
}

use malachitebft_app_channel::app::metrics::prometheus as prometheus_client;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct AddressLabel {
    address: AsLabelValue<Address>,
}

impl AddressLabel {
    fn new(address: Address) -> Self {
        Self {
            address: AsLabelValue(address),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ProcessMsgLabel {
    msg: &'static str,
}

impl ProcessMsgLabel {
    fn new(msg: &'static str) -> Self {
        Self { msg }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct EngineApiLabel {
    api: &'static str,
}

impl EngineApiLabel {
    fn new(api: &'static str) -> Self {
        Self { api }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct VersionInfoLabel {
    pub version: &'static str,
    pub git_commit: &'static str,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ConsensusParamsLabel {
    pub param: &'static str,
}

pub struct MetricsGuard {
    inner: AppMetrics,
    label: &'static str,
    callback: fn(&AppMetrics, &'static str, Duration),
    start_time: Instant,
}

impl MetricsGuard {
    fn new(
        inner: AppMetrics,
        label: &'static str,
        callback: fn(&AppMetrics, &'static str, Duration),
    ) -> Self {
        Self {
            inner,
            label,
            callback,
            start_time: Instant::now(),
        }
    }
}

impl Drop for MetricsGuard {
    fn drop(&mut self) {
        let elapsed = self.start_time.elapsed();

        (self.callback)(&self.inner, self.label, elapsed);
    }
}
