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

use alloy_consensus::{Signed, TxEip1559};
use color_eyre::eyre::{self, Result, WrapErr};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::generator::TxGenerator;
use crate::latency::{compute_tx_hash, timestamp_now, TxSubmitted};
use crate::spammer::RateLimiter;
use crate::ws::{is_connection_error, WsClient, WsClientBuilder};

/// Configuration for `TxSender` behavior.
///
/// Groups all runtime parameters that control how a sender operates, including
/// timeouts, retry behavior, and optional latency tracking.
pub(crate) struct TxSenderConfig {
    /// Maximum time in seconds to run before stopping (0 means unlimited).
    pub max_time: u64,
    /// Whether to wait for RPC response before counting the tx as sent.
    /// Only applies in fire-and-forget mode; backpressure mode always waits.
    pub wait_response: bool,
    /// Number of reconnection attempts on connection failure.
    pub reconnect_attempts: u32,
    /// Delay between reconnection attempts.
    pub reconnect_period: Duration,
    /// Optional channel for emitting transaction submission timestamps.
    pub latency_sender: Option<Sender<TxSubmitted>>,
}

/// The transaction source determines the sender's operating mode.
pub(crate) enum TxSource {
    /// Fire-and-forget: receives txs from a separate generator task via channel.
    Channel {
        rx: Receiver<Signed<TxEip1559>>,
        wait_response: bool,
    },
    /// Backpressure: owns the generator directly, waits for each response.
    Backpressure(Box<TxGenerator>),
}

/// Transaction sender for one tx generator.
/// Receive transactions from one tx generator and send them to multiple nodes in round-robin fashion.
/// Report the result of each transaction to the result tracker.
pub(crate) struct TxSender {
    id: usize,
    /// WebSocket clients used to dispatch transactions to nodes in round-robin.
    ws_clients: Vec<WsClient>,
    tx_source: TxSource,
    result_sender: Sender<Result<u64>>,
    /// Optional channel for emitting tx submission timestamps.
    latency_sender: Option<Sender<TxSubmitted>>,
    rate_limiter: Arc<RateLimiter>,
    node_index: usize,
    max_time: u64,
    reconnect_attempts: u32,
    reconnect_period: Duration,
}

/// Outcome of sending a transaction in backpressure mode.
enum SendOutcome {
    /// Transaction accepted by the node.
    Accepted,
    /// Transaction rejected by the node (account-level issue).
    Rejected(String),
    /// Transient failure not related to the account (connection error, pool full).
    Transient(String),
}

impl TxSender {
    /// Create a sender in channel (fire-and-forget mode).
    pub async fn new_channel(
        id: usize,
        ws_client_builders: Vec<WsClientBuilder>,
        tx_receiver: Receiver<Signed<TxEip1559>>,
        result_sender: Sender<Result<u64>>,
        rate_limiter: Arc<RateLimiter>,
        config: TxSenderConfig,
    ) -> Result<Self> {
        let ws_clients = Self::build_ws_clients(ws_client_builders).await?;
        Ok(Self {
            id,
            ws_clients,
            tx_source: TxSource::Channel {
                rx: tx_receiver,
                wait_response: config.wait_response,
            },
            result_sender,
            latency_sender: config.latency_sender,
            rate_limiter,
            node_index: 0,
            max_time: config.max_time,
            reconnect_attempts: config.reconnect_attempts,
            reconnect_period: config.reconnect_period,
        })
    }

    /// Create a sender in backpressure mode (owns the generator directly).
    pub async fn new_backpressure(
        id: usize,
        ws_client_builders: Vec<WsClientBuilder>,
        generator: TxGenerator,
        result_sender: Sender<Result<u64>>,
        rate_limiter: Arc<RateLimiter>,
        config: TxSenderConfig,
    ) -> Result<Self> {
        let ws_clients = Self::build_ws_clients(ws_client_builders).await?;
        Ok(Self {
            id,
            ws_clients,
            tx_source: TxSource::Backpressure(Box::new(generator)),
            result_sender,
            latency_sender: config.latency_sender,
            rate_limiter,
            node_index: 0,
            max_time: config.max_time,
            reconnect_attempts: config.reconnect_attempts,
            reconnect_period: config.reconnect_period,
        })
    }

    async fn build_ws_clients(builders: Vec<WsClientBuilder>) -> Result<Vec<WsClient>> {
        let mut ws_clients = Vec::new();
        for builder in builders {
            ws_clients.push(builder.build().await?);
        }
        Ok(ws_clients)
    }

    pub async fn run(&mut self) -> Result<()> {
        match &self.tx_source {
            TxSource::Channel { .. } => self.run_channel().await,
            TxSource::Backpressure(_) => self.run_backpressure().await,
        }
    }

    /// Fire-and-forget mode: read from channel, send, optionally wait for response.
    async fn run_channel(&mut self) -> Result<()> {
        debug!("TxSender {}: running (channel mode)...", self.id);
        let wait_response = match &self.tx_source {
            TxSource::Channel { wait_response, .. } => *wait_response,
            TxSource::Backpressure(_) => unreachable!("run_channel called in backpressure mode"),
        };
        let start_time = Instant::now();
        loop {
            match self.rate_limiter.inc() {
                Some(true) => {
                    let tx = match &mut self.tx_source {
                        TxSource::Channel { rx, .. } => rx.recv().await,
                        _ => unreachable!(),
                    };
                    if let Some(tx) = tx {
                        self.send(tx, wait_response).await?;
                    } else {
                        break;
                    }
                }
                Some(false) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
                None => {
                    break;
                }
            }

            if self.max_time > 0 && start_time.elapsed().as_secs() >= self.max_time {
                break;
            }
        }
        if let TxSource::Channel { rx, .. } = &mut self.tx_source {
            rx.close();
        }
        Ok(())
    }

    /// Backpressure mode: generate tx, send, wait for response, ack nonce on success.
    /// On rejection, re-query the node for the correct nonce before retrying.
    /// After `MAX_CONSECUTIVE_FAILURES` consecutive rejections for the same
    /// account, skip it to avoid an infinite retry loop (e.g., insufficient
    /// funds, blocklisted address). Transient errors (connection failures,
    /// txpool full) back off and retry without counting toward the failure
    /// threshold.
    async fn run_backpressure(&mut self) -> Result<()> {
        const MAX_CONSECUTIVE_FAILURES: u32 = 3;
        const TRANSIENT_BACKOFF: Duration = Duration::from_millis(100);

        debug!("TxSender {}: running (backpressure mode)...", self.id);
        let start_time = Instant::now();
        let mut consecutive_failures: HashMap<usize, u32> = HashMap::new();
        loop {
            match self.rate_limiter.inc() {
                Some(true) => {
                    let TxSource::Backpressure(generator) = &mut self.tx_source else {
                        unreachable!("run_backpressure called in channel mode");
                    };
                    let Some((signed_tx, account_index)) = generator.next_tx().await? else {
                        // All accounts exhausted
                        break;
                    };
                    let result = self.send_and_wait(signed_tx).await?;
                    // Re-borrow generator — the previous borrow was released for send_and_wait(&mut self).
                    let TxSource::Backpressure(generator) = &mut self.tx_source else {
                        unreachable!();
                    };
                    match result {
                        SendOutcome::Accepted => {
                            consecutive_failures.remove(&account_index);
                            generator.ack_nonce(account_index);
                        }
                        SendOutcome::Transient(reason) => {
                            debug!(
                                "TxSender {}: transient error for account {}, backing off: {}",
                                self.id, account_index, reason
                            );
                            tokio::time::sleep(TRANSIENT_BACKOFF).await;
                        }
                        SendOutcome::Rejected(reason) => {
                            let count = consecutive_failures.entry(account_index).or_default();
                            *count += 1;
                            if *count >= MAX_CONSECUTIVE_FAILURES {
                                warn!(
                                    "TxSender {}: account {} failed {} times consecutively, skipping (last error: {})",
                                    self.id, account_index, count, reason
                                );
                                consecutive_failures.remove(&account_index);
                                generator.skip_account(account_index);
                            } else {
                                debug!(
                                    "TxSender {}: tx rejected for account {} ({}/{}): {}",
                                    self.id, account_index, count, MAX_CONSECUTIVE_FAILURES, reason
                                );
                                if let Err(e) = generator.refresh_nonce(account_index).await {
                                    debug!(
                                        "TxSender {}: failed to refresh nonce for account {}: {}",
                                        self.id, account_index, e
                                    );
                                }
                            }
                        }
                    }
                }
                Some(false) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
                None => {
                    break;
                }
            }

            if self.max_time > 0 && start_time.elapsed().as_secs() >= self.max_time {
                break;
            }
        }
        Ok(())
    }

    /// Encode a signed transaction, pick the next node round-robin, and dispatch
    /// via `send_request_with_retry`. Returns `(request_id, node_index, tx_len, tx_hash)`.
    /// A `request_id` of 0 means the error was already reported to the tracker.
    async fn dispatch_raw_tx(
        &mut self,
        tx: Signed<TxEip1559>,
    ) -> Result<(u64, usize, u64, alloy_primitives::B256)> {
        let tx_len = tx.eip2718_encoded_length();

        let mut buf = Vec::with_capacity(tx_len);
        tx.eip2718_encode(&mut buf);

        let tx_hash = compute_tx_hash(&buf);
        let payload = hex::encode(buf);
        let tx_len = tx_len as u64;

        let len = self.ws_clients.len();
        let node_idx = self.node_index % len;
        self.node_index = (self.node_index + 1) % len;

        let ws_client = &mut self.ws_clients[node_idx];
        let request_id = Self::send_request_with_retry(
            self.id,
            ws_client,
            "eth_sendRawTransaction",
            json!([payload]),
            &mut self.result_sender,
            self.reconnect_attempts,
            self.reconnect_period,
        )
        .await?;

        Ok((request_id, node_idx, tx_len, tx_hash))
    }

    /// Fire-and-forget send: dispatch and optionally wait for the response.
    ///
    /// If latency tracking is enabled, records the submission only when the
    /// result is successful. With `wait_response` enabled, this means only
    /// transactions accepted by the node are tracked. Without it, the node's
    /// response is not checked, so rejected transactions may still be tracked.
    async fn send(&mut self, tx: Signed<TxEip1559>, wait_response: bool) -> Result<()> {
        // Capture timestamp before sending for accurate latency measurement
        let submitted_time = timestamp_now();
        let (request_id, node_idx, tx_len, tx_hash) = self.dispatch_raw_tx(tx).await?;

        if request_id == 0 {
            return Ok(());
        }

        let result = if wait_response {
            self.ws_clients[node_idx]
                .wait_for_response(request_id)
                .await
                .map(|_: String| tx_len)
        } else {
            Ok(tx_len)
        };

        // Only record submission if the node accepted the tx (or we didn't wait)
        if result.is_ok() {
            if let Some(latency_sender) = self.latency_sender.as_ref() {
                latency_sender
                    .send(TxSubmitted {
                        tx_hash,
                        submitted_time,
                    })
                    .await
                    .wrap_err_with(|| {
                        format!(
                            "Failed to send tx submission event for tx hash: {}",
                            tx_hash
                        )
                    })?;
            }
        }

        self.result_sender.send(result).await?;

        Ok(())
    }

    /// Send a transaction and always wait for the JSON-RPC response.
    /// Reports to result tracker in all cases. When latency tracking is
    /// enabled, records a [`TxSubmitted`] event on acceptance so the
    /// tracker can correlate submit time with finalized inclusion.
    async fn send_and_wait(&mut self, tx: Signed<TxEip1559>) -> Result<SendOutcome> {
        let submitted_time = timestamp_now();
        let (request_id, node_idx, tx_len, tx_hash) = self.dispatch_raw_tx(tx).await?;

        if request_id == 0 {
            return Ok(SendOutcome::Transient(
                "dispatch failed (tracked)".to_string(),
            ));
        }

        let result = self.ws_clients[node_idx]
            .wait_for_response(request_id)
            .await
            .map(|_: String| tx_len);

        let outcome = match &result {
            Ok(_) => {
                if let Some(latency_sender) = self.latency_sender.as_ref() {
                    latency_sender
                        .send(TxSubmitted {
                            tx_hash,
                            submitted_time,
                        })
                        .await
                        .wrap_err_with(|| {
                            format!(
                                "Failed to send tx submission event for tx hash: {}",
                                tx_hash
                            )
                        })?;
                }
                SendOutcome::Accepted
            }
            Err(e) if is_connection_error(e) => SendOutcome::Transient(e.to_string()),
            Err(e) => {
                let reason = e.to_string();
                if reason.contains("txpool is full") {
                    SendOutcome::Transient(reason)
                } else {
                    SendOutcome::Rejected(reason)
                }
            }
        };

        self.result_sender.send(result).await?;
        Ok(outcome)
    }

    /// Send a request with reconnection retry logic.
    /// Returns the request ID on success, or sends error to tracker and returns Ok(0) for tracked errors.
    async fn send_request_with_retry(
        sender_id: usize,
        ws_client: &mut WsClient,
        method: &str,
        params: serde_json::Value,
        result_sender: &mut Sender<Result<u64>>,
        reconnect_attempts: u32,
        reconnect_period: Duration,
    ) -> Result<u64> {
        let max_attempts = reconnect_attempts.max(1); // At least one attempt
        for attempt in 0..max_attempts {
            match ws_client.request(method, params.clone()).await {
                Ok(req_id) => return Ok(req_id),
                Err(e) if is_connection_error(&e) => {
                    debug!(
                        "TxSender {}: connection error on attempt {}/{} to {}: {}",
                        sender_id,
                        attempt + 1,
                        max_attempts,
                        ws_client.url,
                        e
                    );

                    // Try to reconnect
                    if let Err(reconnect_err) = ws_client.reconnect().await {
                        debug!(
                            "TxSender {}: reconnect failed on attempt {}/{}: {}",
                            sender_id,
                            attempt + 1,
                            max_attempts,
                            reconnect_err
                        );

                        // If this was the last attempt, send error to tracker
                        if attempt + 1 >= max_attempts {
                            let result = Err(eyre::eyre!(
                                "Connection error after {} attempts: {}, last reconnect error: {}",
                                max_attempts,
                                e,
                                reconnect_err
                            ));
                            result_sender.send(result).await?;
                            return Ok(0); // Return dummy ID, error was tracked
                        }

                        // Wait before next attempt
                        tokio::time::sleep(reconnect_period).await;
                        continue;
                    }

                    // Reconnected successfully
                    debug!(
                        "TxSender {}: reconnected successfully on attempt {}/{}",
                        sender_id,
                        attempt + 1,
                        max_attempts
                    );

                    // Wait before retrying the request to allow the node to be fully ready.
                    tokio::time::sleep(reconnect_period).await;
                }
                Err(e) => {
                    // Non-connection error (protocol error, etc.), fail the test
                    return Err(e);
                }
            }
        }

        // All attempts exhausted
        let result = Err(eyre::eyre!(
            "Failed to send request after {} attempts",
            max_attempts
        ));
        result_sender.send(result).await?;
        Ok(0) // Return dummy ID, error was tracked
    }
}
