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

use std::fs::create_dir_all;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use color_eyre::eyre::{self, Result};
use tokio::sync::mpsc::{self, Sender};
use tokio::time::{self, Duration};
use tracing::{debug, info};
use url::Url;

use alloy_consensus::{Signed, TxEip1559};

use crate::accounts::AccountBuilder;
use crate::generator::TxGenerator;
use crate::latency::{LatencyTracker, TxSubmitted};
use crate::result_tracker::ResultTracker;
use crate::sender::TxSender;
use crate::ws::WsClientBuilder;
use crate::Config;

/// Mnemonic for wallet generation.
///
/// This must match the mnemonic used in the genesis file to ensure the generated
/// accounts have pre-funded balances.
pub const TEST_MNEMONIC: &str = "test test test test test test test test test test test junk";

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
    /// Shared rate limiter for all senders, enforcing TPS and total transaction
    /// limits.
    rate_limiter: Arc<RateLimiter>,
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
                WsClientBuilder::new(url.clone(), Duration::from_secs(10))
                    .with_connect_timeout(Duration::from_mins(30)),
            );
        }

        let (tx_latency_sender, latency_tracker) = if config.tx_latency {
            let (sender, receiver) = mpsc::channel::<TxSubmitted>(100_000);
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
        let rate_limiter = Arc::new(RateLimiter::new(config.max_rate, config.max_num_txs));

        // Create transaction generators and senders
        let (tx_generators, tx_senders) = if config.fire_and_forget {
            Self::make_spammers(
                ws_client_builders.clone(),
                &result_sender,
                tx_latency_sender,
                &rate_limiter,
                config,
            )
            .await?
        } else {
            Self::make_loaders(
                ws_client_builders.clone(),
                &result_sender,
                tx_latency_sender,
                &rate_limiter,
                config,
            )
            .await?
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
            rate_limiter,
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
    ) -> Result<(Vec<TxGenerator>, Vec<TxSender>)> {
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
        for (i, (start, end)) in ranges.into_iter().enumerate() {
            let (tx_gen, sender) = Self::make_spammer(
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
        }

        Ok((tx_generators, tx_senders))
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
    ) -> Result<(TxGenerator, TxSender)> {
        // Buffered channel to send transactions from generator to sender
        let (tx_sender, tx_receiver) = mpsc::channel::<Signed<TxEip1559>>(10000);

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

        Ok((tx_gen, sender))
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

    pub async fn run(mut self) -> Result<()> {
        let latency_handle = self
            .latency_tracker
            .map(|tracker| tokio::spawn(async move { tracker.run().await }));

        // Fire-and-forget mode: spawn generator tasks and buffer before sending.
        // Backpressure mode: generators are owned by senders, so this is empty.
        let mut tx_gen_handles = Vec::new();
        if !self.tx_generators.is_empty() {
            for mut tx_gen in self.tx_generators {
                tx_gen_handles.push(tokio::spawn(async move { tx_gen.run().await }));
            }

            time::sleep(Duration::from_millis(100)).await;
            debug!("Buffering transactions during 5 seconds...");
            time::sleep(Duration::from_secs(5)).await;
        }

        let mut tx_sender_handles = Vec::new();
        for mut tx_sender in self.tx_senders {
            tx_sender_handles.push(tokio::spawn(async move { tx_sender.run().await }));
        }

        let rate_limiter_handle = tokio::spawn(async move { self.rate_limiter.run().await });
        let tracker_handle = tokio::spawn(async move { self.result_tracker.run().await });

        for handle in tx_sender_handles {
            handle.await??;
        }
        rate_limiter_handle.abort();

        for handle in tx_gen_handles {
            handle.await??;
        }

        let _ = self.finish_sender.send(()).await;
        tracker_handle.await??;

        if let Some(handle) = latency_handle {
            handle.await??;
        }

        Ok(())
    }
}

/// Rate limiter for transaction sending.
///
/// Enforces a maximum transactions-per-second rate and an optional total
/// transaction count limit. Shared across all senders via `Arc`.
pub(crate) struct RateLimiter {
    /// Maximum transactions per second.
    tps: u64,
    /// Maximum total transactions (0 means unlimited).
    max_num_txs: u64,
    /// Counter for transactions in the current second.
    tps_counter: AtomicU64,
    /// Counter for total transactions sent.
    total_counter: AtomicU64,
}

impl RateLimiter {
    /// Create a new rate limiter with the given TPS and total transaction limits.
    pub fn new(tps: u64, max_num_txs: u64) -> Self {
        Self {
            tps,
            max_num_txs,
            tps_counter: AtomicU64::new(0),
            total_counter: AtomicU64::new(0),
        }
    }

    /// Run the rate limiter's background task.
    ///
    /// Resets the per-second counter every second to allow the next batch of
    /// transactions. This task runs indefinitely until aborted.
    pub async fn run(&self) -> Result<()> {
        let mut interval = time::interval(Duration::from_secs(1));
        let _ = interval.tick().await; // consume the first tick at 0 seconds
        loop {
            let _ = interval.tick().await;
            self.tps_counter.store(0, Ordering::SeqCst);
        }
    }

    /// Increment counters and determine whether to send, wait, or abort.
    ///
    /// Returns:
    /// - `Some(true)`: Send a transaction now.
    /// - `Some(false)`: TPS limit reached, wait until next second.
    /// - `None`: Total transaction limit reached, abort the load.
    pub fn inc(&self) -> Option<bool> {
        let current_tps = self.tps_counter.fetch_add(1, Ordering::SeqCst);
        if current_tps >= self.tps {
            return Some(false); // wait until next second
        }

        let current_total = self.total_counter.fetch_add(1, Ordering::SeqCst);
        if self.max_num_txs > 0 && current_total >= self.max_num_txs {
            return None; // abort the load
        }

        Some(true) // send transaction
    }
}
