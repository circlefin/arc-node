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

use std::collections::HashMap;
use std::fs::create_dir_all;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

/// WebSocket request timeout for each `eth_sendRawTransaction` call.
/// Kept short so that EL backpressure (stalled sends) surfaces quickly as warnings
/// rather than silently inflating per-transaction latency measurements.
const WS_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// WebSocket connect timeout. Long enough to tolerate slow node startup during
/// experiment ramp-up while still failing hard if the node never comes up.
const WS_CONNECT_TIMEOUT: Duration = Duration::from_mins(30);

use color_eyre::eyre::{self, Result};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::time::{self, Duration};
use tracing::{debug, info};
use url::Url;

use alloy_consensus::TxEnvelope;

use crate::accounts::AccountBuilder;
use crate::generator::{AckOutcome, TxGenerator};
use crate::latency::{LatencyTracker, TxSubmitted};
use crate::rate_limiter::RateLimiter;
use crate::result_tracker::ResultTracker;
use crate::sender::TxSender;
use crate::ws::WsClientBuilder;
use crate::{Config, ResumeConfig};

/// Mnemonic for wallet generation.
///
/// This must match the mnemonic used in the genesis file to ensure the generated
/// accounts have pre-funded balances.
pub const TEST_MNEMONIC: &str = "test test test test test test test test test test test junk";

const LATENCY_CHANNEL_CAPACITY: usize = 100_000;

/// Captured generator state from a completed spammer run.
///
/// JSON-serialisable so callers can persist it across process restarts
/// (e.g. `quake run saturation` remote mode passes state from one phase's
/// spammer subprocess to the next via a JSON file on CC). Skipped fields
/// in `TxGenerator` (channels, WS clients) are repopulated by
/// [`Spammer::new_resuming`] before the next run starts.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SpammerState {
    pub(crate) generators: Vec<TxGenerator>,
}

/// Result of a completed [`Spammer::run_capturing_state`] run.
pub struct SpammerRunResult {
    /// Generator state to feed into the next [`Spammer::new_resuming`] call.
    pub state: SpammerState,
    /// JSON-RPC error counts grouped by `(code, head_message)`, collected in
    /// fire-and-forget mode from drained server responses. Empty when the
    /// spammer ran in backpressure mode (which surfaces errors directly).
    pub rpc_errors: HashMap<String, u64>,
    /// Unix millisecond timestamp at which the spammer began the spamming
    /// window (after WS connect + nonce warmup completed). Together with
    /// `finished_at_unix_ms`, defines the time interval that downstream
    /// orchestrators should use to query chain-side metrics so the numerator
    /// (Prometheus tx-count delta) aligns with the denominator (elapsed).
    pub started_at_unix_ms: i64,
    /// Unix millisecond timestamp at which the spammer stopped sending.
    pub finished_at_unix_ms: i64,
    /// Average TPS as observed locally by the spammer: total transactions
    /// submitted (regardless of server acceptance) divided by wall-clock run
    /// duration. Distinct from any server-side or chain-confirmed rate — this
    /// is what the load generator actually offered. Ideally matches the
    /// configured `max_rate`.
    pub actual_offered_tps: f64,
    /// Average bytes-per-second locally offered by the spammer: total tx
    /// bytes submitted divided by wall-clock run duration. Together with
    /// `actual_offered_tps` this distinguishes "high TPS, tiny transfers"
    /// from "high TPS, fat ERC20/guzzler payloads."
    pub actual_offered_bytes_per_sec: f64,
}

/// Serializable summary of a spammer run — the JSON-friendly subset of
/// [`SpammerRunResult`] (drops `state`, which holds non-Serialize generator
/// handles). Used by `quake run saturation` to recover phase metrics when the
/// spammer ran on a remote host: the binary writes `summary.json` at the end
/// of the run and the orchestrator reads it back over SCP.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpammerSummary {
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub actual_offered_tps: f64,
    pub actual_offered_bytes_per_sec: f64,
    pub rpc_errors: HashMap<String, u64>,
}

impl SpammerSummary {
    /// Seconds the spammer spent actually sending — `(finished - started) / 1000`.
    pub fn elapsed_secs(&self) -> f64 {
        ((self.finished_at_unix_ms - self.started_at_unix_ms) as f64 / 1000.0).max(0.0)
    }
}

impl From<&SpammerRunResult> for SpammerSummary {
    fn from(r: &SpammerRunResult) -> Self {
        Self {
            started_at_unix_ms: r.started_at_unix_ms,
            finished_at_unix_ms: r.finished_at_unix_ms,
            actual_offered_tps: r.actual_offered_tps,
            actual_offered_bytes_per_sec: r.actual_offered_bytes_per_sec,
            rpc_errors: r.rpc_errors.clone(),
        }
    }
}

impl SpammerState {
    /// Re-query on-chain pending nonces for all generators in parallel.
    ///
    /// Called automatically by [`Spammer::new_resuming`]. Exposed here so
    /// callers can trigger a resync before constructing the next phase if needed.
    pub async fn resync_nonces(&mut self) -> Result<()> {
        let mut handles = Vec::with_capacity(self.generators.len());
        for mut tx_gen in self.generators.drain(..) {
            handles.push(tokio::spawn(async move {
                tx_gen.resync_nonces().await?;
                Ok::<TxGenerator, eyre::Error>(tx_gen)
            }));
        }
        for handle in handles {
            self.generators.push(handle.await??);
        }
        Ok(())
    }

    /// Pre-derive every BIP32 signing key for every generator's account range,
    /// in parallel across generators via `tokio::task::spawn_blocking` (the
    /// derivation is CPU-bound, not async). Closes the warm-cache gap with
    /// local-mode `SpammerState` after deserialising from JSON. No-op when
    /// keys are already populated.
    pub async fn eagerly_derive_signers(&mut self) -> Result<()> {
        // Skip the drain + spawn_blocking + await fan-out for in-process
        // resumes where every generator already carries its derived keys.
        if self.generators.iter().all(TxGenerator::signers_populated) {
            return Ok(());
        }
        let mut handles = Vec::with_capacity(self.generators.len());
        for mut tx_gen in self.generators.drain(..) {
            handles.push(tokio::task::spawn_blocking(move || {
                tx_gen.eagerly_derive_signers()?;
                Ok::<TxGenerator, eyre::Error>(tx_gen)
            }));
        }
        for handle in handles {
            self.generators.push(handle.await??);
        }
        Ok(())
    }
}

/// Transaction load generator orchestrator.
///
/// Coordinates multiple transaction generators, senders, and trackers to produce
/// sustained transaction load against one or more Ethereum nodes.
pub struct Spammer {
    /// Transaction generators, each responsible for a subset of signer accounts.
    tx_generators: Vec<TxGenerator>,
    /// Transaction senders that fan out transactions to target nodes in round-robin
    /// fashion.
    tx_senders: Vec<TxSender>,
    /// Per-generator ack channels: each receiver pairs 1:1 with the
    /// generator at the same index and carries `AckOutcome` messages from
    /// the matching `TxSender` so the generator can advance or refresh the
    /// cached nonce based on the *actual* response from the node. Empty in
    /// backpressure mode where the sender owns the generator and acks inline.
    tx_ack_receivers: Vec<Receiver<AckOutcome>>,
    /// Tracks transaction results and reports statistics on them.
    result_tracker: ResultTracker,
    /// Optional tracker for submit-to-finalized latency measurement.
    latency_tracker: Option<LatencyTracker>,
    /// Channel to signal the result tracker to finish.
    finish_sender: Sender<()>,
}

impl Spammer {
    /// Create a new spammer instance connected to the given target nodes.
    ///
    /// Initializes all generators, senders, and trackers based on the provided
    /// configuration. Returns an error if no target nodes are provided or if
    /// connection setup fails.
    pub async fn new(target_ws_urls: Vec<(String, Url)>, config: &Config) -> Result<Self> {
        if target_ws_urls.is_empty() {
            eyre::bail!("No target nodes provided");
        }

        info!(
            "Creating {} generator for nodes {}, from {} accounts with {} generators, in {:?} partition mode, and num_txs={}, rate={}, time={}, max_txs_per_account={}",
            if config.fire_and_forget { "spam" } else { "load" },
            target_ws_urls
                .iter()
                .map(|(node, _)| node.clone())
                .collect::<Vec<String>>()
                .join(", "),
            config.max_num_accounts,
            config.num_generators,
            config.partition_mode,
            config.max_num_txs,
            config.max_rate,
            config.max_time,
            config.max_txs_per_account,
        );

        // Create channels for communication between components
        let (result_sender, result_receiver) = mpsc::channel::<Result<u64>>(10000);
        let (finish_sender, finish_receiver) = mpsc::channel::<()>(1);

        // WS clients to all target Quake endpoints
        let mut ws_client_builders = Vec::new();
        for (_, url) in target_ws_urls {
            ws_client_builders.push(
                WsClientBuilder::new(url.clone(), WS_REQUEST_TIMEOUT)
                    .with_connect_timeout(WS_CONNECT_TIMEOUT),
            );
        }

        let (tx_latency_sender, latency_tracker) = if config.tx_latency {
            let (sender, receiver) = mpsc::channel::<TxSubmitted>(LATENCY_CHANNEL_CAPACITY);
            let csv_name = format!(
                "tx_latency_{}.csv",
                chrono::Utc::now().format("%Y%m%d_%H%M%S")
            );
            let csv_path = match &config.csv_dir {
                Some(dir) => dir.join(&csv_name),
                None => PathBuf::from(csv_name),
            };
            // create .quake/results/ directory if it doesn't exist
            if let Some(parent) = csv_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                create_dir_all(parent)?;
            }

            let ws_builder = ws_client_builders
                .first()
                .cloned()
                .ok_or_else(|| eyre::eyre!("No RPC endpoints available"))?;
            let tracker = LatencyTracker::new(ws_builder, receiver, csv_path).await?;
            (Some(sender), Some(tracker))
        } else {
            (None, None)
        };

        // Shared rate limiter for all senders
        let rate_limiter = Arc::new(RateLimiter::new(
            config.max_rate,
            config.max_num_txs,
            config.num_generators,
        ));

        // Create transaction generators and senders
        let (tx_generators, tx_senders, tx_ack_receivers) = if config.fire_and_forget {
            Self::make_spammers(
                ws_client_builders.clone(),
                &result_sender,
                tx_latency_sender,
                &rate_limiter,
                config,
            )
            .await?
        } else {
            let (gens, sends) = Self::make_loaders(
                ws_client_builders.clone(),
                &result_sender,
                tx_latency_sender,
                &rate_limiter,
                config,
            )
            .await?;
            (gens, sends, Vec::new())
        };

        // Create result tracker
        let result_tracker = ResultTracker::new(
            ws_client_builders,
            result_receiver,
            finish_receiver,
            config.silent,
            config.show_pool_status,
        )
        .await?;

        Ok(Self {
            tx_generators,
            tx_senders,
            tx_ack_receivers,
            result_tracker,
            latency_tracker,
            finish_sender,
        })
    }

    /// Create all transaction generators and senders.
    ///
    /// Partitions the account space among generators according to the configured
    /// partition mode, then creates a generator-sender pair for each partition.
    #[allow(clippy::too_many_arguments)]
    async fn make_spammers(
        ws_client_builders: Vec<WsClientBuilder>,
        result_sender: &Sender<Result<u64>>,
        tx_latency_sender: Option<Sender<TxSubmitted>>,
        rate_limiter: &Arc<RateLimiter>,
        config: &Config,
    ) -> Result<(Vec<TxGenerator>, Vec<TxSender>, Vec<Receiver<AckOutcome>>)> {
        // Partition account space among generators
        let ranges = config
            .partition_mode
            .partition_accounts(config.max_num_accounts, config.num_generators)?;
        assert_eq!(ranges.len(), config.num_generators);
        debug!(
            "Creating tx generators with signers in ranges: {:?}",
            ranges
        );

        let account_builder = AccountBuilder::new(TEST_MNEMONIC.to_string());

        let mut tx_generators = Vec::new();
        let mut tx_senders = Vec::new();
        let mut ack_receivers = Vec::new();
        for (i, (start, end)) in ranges.into_iter().enumerate() {
            let (tx_gen, sender, ack_rx) = Self::make_spammer(
                i,
                start..end,
                &account_builder,
                ws_client_builders.to_owned(),
                result_sender,
                tx_latency_sender.clone(),
                rate_limiter,
                config,
            )
            .await?;

            tx_generators.push(tx_gen);
            tx_senders.push(sender);
            ack_receivers.push(ack_rx);
        }

        Ok((tx_generators, tx_senders, ack_receivers))
    }

    /// Create a single tx generator and sender for a given range of accounts.
    #[allow(clippy::too_many_arguments)]
    async fn make_spammer(
        i: usize,
        range: Range<usize>,
        account_builder: &AccountBuilder,
        ws_client_builders: Vec<WsClientBuilder>,
        result_sender: &Sender<Result<u64>>,
        tx_latency_sender: Option<Sender<TxSubmitted>>,
        rate_limiter: &Arc<RateLimiter>,
        config: &Config,
    ) -> Result<(TxGenerator, TxSender, Receiver<AckOutcome>)> {
        // Buffered channel to send transactions from generator to sender
        let (tx_sender, tx_receiver) = mpsc::channel::<(TxEnvelope, usize)>(10000);
        // Buffered ack channel sender→generator. Sized to match the tx channel
        // so backpressure on tx submission is mirrored on the ack path; if the
        // generator stalls processing acks the sender will eventually block on
        // emit_ack, which is the same shape of backpressure we already accept
        // on the tx side.
        let (ack_sender, ack_receiver) = mpsc::channel::<AckOutcome>(10000);

        debug!("TxGenerator {i}: creating with signers in range {range:?}...");
        let mut tx_gen = TxGenerator::new(
            i,
            range.clone(),
            account_builder.clone(),
            ws_client_builders.to_owned(),
            Some(tx_sender.clone()),
            config.max_txs_per_account,
            config.query_latest_nonce,
            config.tx_input_size,
            config.guzzler_fn_weights,
            config.erc20_fn_weights,
            config.tx_type_mix,
        );

        if config.preinit_accounts {
            debug!(
                "TxGenerator {i}: pre-initializing {} accounts...",
                range.len()
            );
            tx_gen
                .initialize_accounts(account_builder, range, config.query_latest_nonce)
                .await
                .unwrap_or_else(|e| {
                    panic!("Failed to initialize accounts for TxGenerator {i}: {e}")
                })
        }

        debug!("TxSender {i}: creating...");
        let sender = TxSender::new_channel(
            i,
            ws_client_builders.to_owned(),
            tx_receiver,
            result_sender.clone(),
            ack_sender,
            rate_limiter.clone(),
            crate::sender::TxSenderConfig {
                max_time: config.max_time,
                wait_response: config.wait_response,
                reconnect_attempts: config.reconnect_attempts,
                reconnect_period: config.reconnect_period,
                latency_sender: tx_latency_sender,
            },
        )
        .await?;

        Ok((tx_gen, sender, ack_receiver))
    }

    /// Create senders in backpressure mode: each sender owns its generator directly.
    #[allow(clippy::too_many_arguments)]
    async fn make_loaders(
        ws_client_builders: Vec<WsClientBuilder>,
        result_sender: &Sender<Result<u64>>,
        tx_latency_sender: Option<Sender<TxSubmitted>>,
        rate_limiter: &Arc<RateLimiter>,
        config: &Config,
    ) -> Result<(Vec<TxGenerator>, Vec<TxSender>)> {
        let ranges = config
            .partition_mode
            .partition_accounts(config.max_num_accounts, config.num_generators)?;
        assert_eq!(ranges.len(), config.num_generators);
        debug!(
            "Creating backpressure senders with signers in ranges: {:?}",
            ranges
        );

        let account_builder = AccountBuilder::new(TEST_MNEMONIC.to_string());

        let mut tx_senders = Vec::new();
        for (i, (start, end)) in ranges.into_iter().enumerate() {
            let range = start..end;
            debug!("TxGenerator {i}: creating (backpressure) with signers in range {range:?}...");
            let mut tx_gen = TxGenerator::new(
                i,
                range.clone(),
                account_builder.clone(),
                ws_client_builders.to_owned(),
                None,
                config.max_txs_per_account,
                config.query_latest_nonce,
                config.tx_input_size,
                config.guzzler_fn_weights,
                config.erc20_fn_weights,
                config.tx_type_mix,
            )
            .with_query_nonces_on_init(true);

            if config.preinit_accounts {
                debug!(
                    "TxGenerator {i}: pre-initializing {} accounts...",
                    range.len()
                );
                tx_gen
                    .initialize_accounts(&account_builder, range, config.query_latest_nonce)
                    .await
                    .unwrap_or_else(|e| {
                        panic!("Failed to initialize accounts for TxGenerator {i}: {e}")
                    });
            }

            debug!("TxSender {i}: creating (backpressure)...");
            let sender = TxSender::new_backpressure(
                i,
                ws_client_builders.to_owned(),
                tx_gen,
                result_sender.clone(),
                rate_limiter.clone(),
                crate::sender::TxSenderConfig {
                    max_time: config.max_time,
                    wait_response: false,
                    reconnect_attempts: config.reconnect_attempts,
                    reconnect_period: config.reconnect_period,
                    latency_sender: tx_latency_sender.clone(),
                },
            )
            .await?;

            tx_senders.push(sender);
        }

        // No separate generator tasks in backpressure mode
        Ok((vec![], tx_senders))
    }

    /// Run the Spammer and return captured generator state for reuse in [`new_resuming`](Self::new_resuming).
    /// Nonces cached during this run are preserved.
    pub async fn run_capturing_state(mut self) -> Result<SpammerRunResult> {
        let latency_handle = self
            .latency_tracker
            .map(|tracker| tokio::spawn(async move { tracker.run().await }));

        let mut tx_gen_handles: Vec<tokio::task::JoinHandle<Result<TxGenerator>>> = Vec::new();
        if !self.tx_generators.is_empty() {
            // The ack receiver list is built 1:1 with `tx_generators` in
            // `make_spammers` / `new_resuming` so popping in order pairs each
            // generator with its sender's outcome channel. Backpressure mode
            // leaves the list empty — generator runs inside the sender and
            // acks inline, so we never index past the available receivers.
            let mut ack_receivers: std::collections::VecDeque<Receiver<AckOutcome>> =
                self.tx_ack_receivers.into_iter().collect();
            for mut tx_gen in self.tx_generators {
                let ack_rx = ack_receivers.pop_front().ok_or_else(|| {
                    eyre::eyre!("tx_ack_receivers must match tx_generators count")
                })?;
                tx_gen_handles.push(tokio::spawn(async move {
                    tx_gen.run(ack_rx).await?;
                    Ok(tx_gen)
                }));
            }

            time::sleep(Duration::from_millis(100)).await;
            debug!("Buffering transactions during 5 seconds...");
            time::sleep(Duration::from_secs(5)).await;
        }

        let mut tx_sender_handles = Vec::new();
        for mut tx_sender in self.tx_senders {
            tx_sender_handles.push(tokio::spawn(async move { tx_sender.run().await }));
        }

        let started_at_unix_ms = chrono::Utc::now().timestamp_millis();
        let load_start = std::time::Instant::now();
        let tracker_handle = tokio::spawn(async move { self.result_tracker.run().await });
        info!("Load STARTED — tx_senders + tracker now running");

        for handle in tx_sender_handles {
            handle.await??;
        }
        let finished_at_unix_ms = chrono::Utc::now().timestamp_millis();
        info!(
            "Load STOPPED — load window was {:.1}s",
            load_start.elapsed().as_secs_f64(),
        );

        let mut generators = Vec::new();
        for handle in tx_gen_handles {
            generators.push(handle.await??);
        }

        let _ = self.finish_sender.send(()).await;
        let summary = tracker_handle.await??;

        if let Some(handle) = latency_handle {
            handle.await??;
        }

        let elapsed_secs = summary.elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
        let actual_offered_tps = summary.total_sent as f64 / elapsed_secs;
        let actual_offered_bytes_per_sec = summary.total_bytes as f64 / elapsed_secs;

        Ok(SpammerRunResult {
            state: SpammerState { generators },
            rpc_errors: summary.errors,
            started_at_unix_ms,
            finished_at_unix_ms,
            actual_offered_tps,
            actual_offered_bytes_per_sec,
        })
    }

    /// Create a spammer that reuses generators from a previous run.
    pub async fn new_resuming(
        target_ws_urls: Vec<(String, Url)>,
        mut state: SpammerState,
        config: &ResumeConfig,
    ) -> Result<Self> {
        if target_ws_urls.is_empty() {
            eyre::bail!("No target nodes provided");
        }
        // An empty state would silently produce a no-op run with zero
        // generators/senders. This happens when a previous backpressure-mode
        // run wrote --state-out: its generators are owned by senders and
        // never surface in SpammerState. Fail fast so the caller fixes the
        // upstream config rather than chasing a phantom "successful" phase.
        if state.generators.is_empty() {
            eyre::bail!(
                "SpammerState has zero generators; refusing to resume a no-op run \
                 (was --state-out produced by a backpressure-mode run?)"
            );
        }

        let resume_started = std::time::Instant::now();
        info!(
            "Resume start: {} generators, targets {}, max_rate={}, max_time={}s",
            state.generators.len(),
            target_ws_urls
                .iter()
                .map(|(node, _)| node.as_str())
                .collect::<Vec<_>>()
                .join(","),
            config.max_rate,
            config.max_time,
        );

        let mut ws_client_builders = Vec::new();
        for (_, url) in &target_ws_urls {
            ws_client_builders.push(
                WsClientBuilder::new(url.clone(), WS_REQUEST_TIMEOUT)
                    .with_connect_timeout(WS_CONNECT_TIMEOUT),
            );
        }
        for tx_gen in &mut state.generators {
            tx_gen.update_ws_client_builders(ws_client_builders.clone());
            // After deserialising from a JSON state file, `signers` is empty
            // (skipped by serde). Size it so the lazy per-account derivation
            // in next_tx has indexable slots. No-op for in-process resumes
            // where signers are still populated.
            tx_gen.ensure_signers_capacity();
        }
        // Pre-derive every signing key in parallel across generators so the
        // first cycle through accounts in the next phase doesn't pay BIP32
        // cost inside `next_tx`. Closes the warm-cache gap that local mode
        // has for free (signers persisted in-process).
        let derive_started = std::time::Instant::now();
        info!("Deriving signing keys (BIP32)...");
        state.eagerly_derive_signers().await?;
        info!(
            "Deriving signing keys: done in {:.1}s",
            derive_started.elapsed().as_secs_f64()
        );
        let resync_started = std::time::Instant::now();
        info!("Resyncing nonces against chain...");
        state.resync_nonces().await?;
        info!(
            "Resyncing nonces: done in {:.1}s",
            resync_started.elapsed().as_secs_f64()
        );

        let num_generators = state.generators.len();
        info!(
            "Resume setup complete in {:.1}s. About to wire {} generators + senders + tracker for {} nodes at {} TPS for {}s.",
            resume_started.elapsed().as_secs_f64(),
            num_generators,
            target_ws_urls.len(),
            config.max_rate,
            config.max_time,
        );

        let (result_sender, result_receiver) = mpsc::channel::<Result<u64>>(10000);
        let (finish_sender, finish_receiver) = mpsc::channel::<()>(1);

        let (tx_latency_sender, latency_tracker) = if config.tx_latency {
            let (sender, receiver) = mpsc::channel::<TxSubmitted>(LATENCY_CHANNEL_CAPACITY);
            let csv_name = format!(
                "tx_latency_{}.csv",
                chrono::Utc::now().format("%Y%m%d_%H%M%S")
            );
            let csv_path = match &config.csv_dir {
                Some(dir) => dir.join(&csv_name),
                None => PathBuf::from(csv_name),
            };
            if let Some(parent) = csv_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                create_dir_all(parent)?;
            }
            let ws_builder = ws_client_builders
                .first()
                .cloned()
                .ok_or_else(|| eyre::eyre!("No RPC endpoints available"))?;
            let tracker = LatencyTracker::new(ws_builder, receiver, csv_path).await?;
            (Some(sender), Some(tracker))
        } else {
            (None, None)
        };

        let rate_limiter = Arc::new(RateLimiter::new(
            config.max_rate,
            config.max_num_txs,
            num_generators,
        ));

        let mut tx_generators = Vec::new();
        let mut tx_senders = Vec::new();
        let mut tx_ack_receivers = Vec::new();
        for (i, mut tx_gen) in state.generators.into_iter().enumerate() {
            let (tx_channel_sender, tx_channel_receiver) =
                mpsc::channel::<(TxEnvelope, usize)>(10000);
            let (ack_sender, ack_receiver) = mpsc::channel::<AckOutcome>(10000);
            tx_gen.reset_tx_sender(tx_channel_sender);

            let sender = TxSender::new_channel(
                i,
                ws_client_builders.clone(),
                tx_channel_receiver,
                result_sender.clone(),
                ack_sender,
                rate_limiter.clone(),
                crate::sender::TxSenderConfig {
                    max_time: config.max_time,
                    wait_response: config.wait_response,
                    reconnect_attempts: config.reconnect_attempts,
                    reconnect_period: config.reconnect_period,
                    latency_sender: tx_latency_sender.clone(),
                },
            )
            .await?;

            tx_generators.push(tx_gen);
            tx_senders.push(sender);
            tx_ack_receivers.push(ack_receiver);
        }

        let result_tracker = ResultTracker::new(
            ws_client_builders,
            result_receiver,
            finish_receiver,
            config.silent,
            config.show_pool_status,
        )
        .await?;

        Ok(Self {
            tx_generators,
            tx_senders,
            tx_ack_receivers,
            result_tracker,
            latency_tracker,
            finish_sender,
        })
    }

    /// Run the spammer and discard captured state
    pub async fn run(self) -> Result<()> {
        self.run_capturing_state().await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_resume_config() -> ResumeConfig {
        ResumeConfig {
            max_rate: 1,
            max_num_txs: 0,
            max_time: 0,
            wait_response: false,
            reconnect_attempts: 0,
            reconnect_period: Duration::from_secs(1),
            silent: true,
            show_pool_status: false,
            tx_latency: false,
            csv_dir: None,
        }
    }

    #[tokio::test]
    async fn new_resuming_rejects_empty_state() {
        let target_ws_urls = vec![(
            "dummy".to_string(),
            Url::parse("ws://127.0.0.1:1").expect("parse ws url"),
        )];
        let state = SpammerState {
            generators: Vec::new(),
        };
        let config = dummy_resume_config();

        let err = match Spammer::new_resuming(target_ws_urls, state, &config).await {
            Ok(_) => panic!("empty state must not resume"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("zero generators"),
            "unexpected error message: {msg}"
        );
    }
}
