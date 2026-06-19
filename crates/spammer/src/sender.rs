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

//! Transaction sending with two operating modes.
//!
//! ## Backpressure mode (default)
//!
//! The sender owns a [`TxGenerator`] directly and drives it in a tight loop:
//! generate a transaction, submit it via JSON-RPC, wait for the response, then
//! advance the nonce only on acceptance. On rejection the sender re-queries the
//! node for the correct nonce and retries. After three consecutive rejections
//! for the same account it skips that account to avoid an infinite retry loop.
//!
//! Use backpressure mode for correctness-sensitive workloads where every
//! transaction must land on-chain (e.g., contract deployments, nonce-dependent
//! sequences, reproducible load tests).
//!
//! ## Fire-and-forget mode
//!
//! A separate generator task pushes signed transactions into a buffered channel
//! (capacity 10,000). The sender reads from the channel and dispatches each
//! transaction without waiting for the JSON-RPC response (unless
//! `--wait-response` is set). Nonces are incremented optimistically at
//! generation time, so nonce gaps can occur on rejection.
//!
//! Use fire-and-forget mode for peak-throughput stress tests where some
//! transaction loss is acceptable.
//!
//! Both modes share the same rate limiter, round-robin node selection, and
//! optional latency tracking.

use alloy_consensus::TxEnvelope;
use alloy_eips::eip2718::Encodable2718;
use color_eyre::eyre::{self, Result, WrapErr};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::generator::{AckOutcome, TxGenerator};
use crate::latency::{compute_tx_hash, timestamp_now, TxSubmitted};
use crate::rate_limiter::RateLimiter;
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
    /// Fire-and-forget: receives `(tx, account_index)` from a separate
    /// generator task via channel. `account_index` is required so the
    /// sender can pair each tx with the eventual `AckOutcome` it sends
    /// back through `ack_sender`.
    Channel {
        rx: Receiver<(TxEnvelope, usize)>,
        wait_response: bool,
    },
    /// Backpressure: owns the generator directly, waits for each response.
    Backpressure(Box<TxGenerator>),
}

/// Dispatches signed transactions to one or more nodes over WebSocket JSON-RPC.
///
/// Each sender is paired with exactly one transaction source (a generator or a
/// channel) and fans out transactions across the target nodes in round-robin
/// order. Results are reported to a shared [`ResultTracker`] for aggregate
/// statistics, and optionally to a [`LatencyTracker`] for submit-to-finalized
/// latency measurement.
///
/// The sender's operating mode is determined by [`TxSource`]:
/// - **Backpressure** ([`TxSource::Backpressure`]): owns the generator, waits
///   for every JSON-RPC response, retries on rejection with nonce refresh.
/// - **Fire-and-forget** ([`TxSource::Channel`]): reads from a buffered
///   channel, sends without waiting (unless `wait_response` is set).
pub(crate) struct TxSender {
    id: usize,
    /// WebSocket clients used to dispatch transactions to nodes in round-robin.
    ws_clients: Vec<WsClient>,
    tx_source: TxSource,
    result_sender: Sender<Result<u64>>,
    /// Channel for relaying per-tx outcomes back to the paired generator so
    /// it can ack the nonce on acceptance or refresh on rejection. Only
    /// populated in fire-and-forget mode (`TxSource::Channel`) — backpressure
    /// mode owns the generator directly and acks inline.
    ack_sender: Option<Sender<AckOutcome>>,
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

/// Outcome of a single fire-and-forget `send()` call (named after the
/// `TxSource::Channel` variant that drives this code path).
///
/// In fire-and-forget mode the sender needs the caller to know whether the
/// tx was already fully observed (`WaitedAccepted` / `WaitedRejected` —
/// `wait_response=true`), is in flight awaiting an async response (`Fired` —
/// `wait_response=false`), or never made it to the wire (`DispatchFailed`).
/// Only the `Fired` variant adds an entry to the per-client inflight deque;
/// the other three are handled inline. Each variant determines whether/when
/// the caller emits an `AckOutcome` to the generator.
enum ChannelSendOutcome {
    /// Response already received in-band and was successful.
    WaitedAccepted,
    /// Response already received in-band and was an error.
    WaitedRejected,
    /// Tx is on the wire; response will arrive later on the WS stream and
    /// must be paired back via the per-client inflight deque.
    Fired { request_id: u64, node_idx: usize },
    /// Dispatch reported the error to the tracker without producing a wire
    /// frame (already-tracked failure). Caller should treat as a rejection.
    DispatchFailed,
}

impl TxSender {
    /// Create a sender in channel (fire-and-forget mode).
    #[allow(clippy::too_many_arguments)]
    pub async fn new_channel(
        id: usize,
        ws_client_builders: Vec<WsClientBuilder>,
        tx_receiver: Receiver<(TxEnvelope, usize)>,
        result_sender: Sender<Result<u64>>,
        ack_sender: Sender<AckOutcome>,
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
            ack_sender: Some(ack_sender),
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
            ack_sender: None,
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

    /// Fire-and-forget mode: read from channel, send, optionally wait for
    /// response. Tracks every in-flight tx as `(request_id → account_index)`
    /// per WS client so when the response eventually drains we can pair it
    /// back to the originating account and tell the generator to ack or
    /// refresh the nonce.
    ///
    /// Without this correlation, fire-and-forget mode silently drifted the
    /// generator's cached nonce whenever a tx was lost between channel and
    /// node (WS reconnect, RPC timeout, ...) — the cache had already been
    /// optimistically advanced but no tx with that nonce ever reached chain,
    /// so subsequent submissions for the account piled into the queued
    /// sub-pool. Pairing drained responses with account_indices closes that
    /// loop in real time at the cost of two `usize`s of bookkeeping per
    /// in-flight tx.
    async fn run_channel(&mut self) -> Result<()> {
        debug!("TxSender {}: running (fire-and-forget mode)...", self.id);
        let wait_response = match &self.tx_source {
            TxSource::Channel { wait_response, .. } => *wait_response,
            TxSource::Backpressure(_) => unreachable!("run_channel called in backpressure mode"),
        };
        // Per-client FIFO of in-flight `(request_id, account_index)` waiting
        // for a response. JSON-RPC over WebSocket preserves request order per
        // connection, so popping the front on each drained response correctly
        // pairs it with the corresponding tx.
        let mut inflight: Vec<VecDeque<(u64, usize)>> = (0..self.ws_clients.len())
            .map(|_| VecDeque::new())
            .collect();
        let start_time = Instant::now();
        loop {
            if !self.rate_limiter.wait().await {
                break;
            }
            let next = match &mut self.tx_source {
                TxSource::Channel { rx, .. } => rx.recv().await,
                _ => unreachable!(),
            };
            let Some((tx, account_index)) = next else {
                break;
            };

            let outcome = self.send(tx, wait_response).await?;

            // Buffer ack-outcome work and per-client error-forwarding here
            // so we can run the drain loop with `&mut self.ws_clients` and
            // emit on `self.ack_sender` / `self.result_sender` once it ends,
            // sidestepping the overlapping-borrow lint.
            let mut pending_acks: Vec<AckOutcome> = Vec::new();
            let mut pending_errors: Vec<eyre::Report> = Vec::new();

            match outcome {
                ChannelSendOutcome::WaitedAccepted => {
                    pending_acks.push(AckOutcome::Accepted);
                }
                ChannelSendOutcome::WaitedRejected => {
                    pending_acks.push(AckOutcome::Rejected(account_index));
                }
                ChannelSendOutcome::Fired {
                    request_id,
                    node_idx,
                } => {
                    inflight[node_idx].push_back((request_id, account_index));
                }
                ChannelSendOutcome::DispatchFailed => {
                    // dispatch_raw_tx already reported the error to the
                    // tracker; treat as a rejection so the next tx for this
                    // account refreshes its nonce.
                    pending_acks.push(AckOutcome::Rejected(account_index));
                }
            }

            // Drain whatever asynchronous responses are available right now.
            // Per-client FIFO assumption: responses arrive in the same order
            // the requests were issued, so popping the deque front gives us
            // the originating account.
            for (client_idx, client) in self.ws_clients.iter_mut().enumerate() {
                while let Some((resp_id, result)) = client.drain_one_result().await {
                    // resp_id == 0 marks a connection-level event (e.g. close
                    // frame) with no originating request. The connection is
                    // gone and `reconnect()` resets `next_id` to 1, so every
                    // entry currently in this client's inflight deque has a
                    // stale id that can never match a future response and
                    // would corrupt id-pairing if left behind. Drain the deque
                    // and Reject each pending account so the generator
                    // refreshes their nonces from chain.
                    if resp_id == 0 {
                        if let Err(e) = result {
                            pending_errors.push(e);
                        }
                        for (_stale_id, acct) in inflight[client_idx].drain(..) {
                            pending_acks.push(AckOutcome::Rejected(acct));
                        }
                        continue;
                    }
                    let acct = match inflight[client_idx].pop_front() {
                        Some((expected_id, acct)) if expected_id == resp_id => Some(acct),
                        Some((expected_id, acct)) => {
                            // Per-client JSON-RPC over WS is FIFO so a
                            // mismatch implies a request was silently dropped
                            // upstream (or a stale entry slipped past a
                            // reconnect drain). Don't attribute this response
                            // to the front account — that would emit an
                            // Ack/Reject against the wrong account index.
                            // Drop the mismatched front entry too so the deque
                            // doesn't keep poisoning subsequent matches; the
                            // affected accounts catch up at the next phase
                            // resync.
                            debug!(
                                "TxSender {}: response id {resp_id} did not match expected {expected_id} on client {client_idx} (dropped front entry for account {acct})",
                                self.id
                            );
                            None
                        }
                        None => {
                            // Response with no matching inflight entry.
                            // Close-frame sentinels are handled above; this
                            // arm only fires if the deque has been drained
                            // out from under us (shouldn't happen given the
                            // single-producer-per-client invariant). Skip.
                            None
                        }
                    };
                    // Emit an AckOutcome only when we have a confident
                    // account match. Forward errors to result_tracker
                    // unconditionally so phase-level `rpc_errors` still picks
                    // them up — these are two separate sinks (generator vs
                    // tracker). Successful responses were already counted at
                    // submit time by `send()`, so we only emit Accepted to
                    // the generator (cache ack) and skip forwarding to the
                    // tracker to avoid double-counting.
                    match (acct, &result) {
                        (Some(_acct), Ok(())) => {
                            pending_acks.push(AckOutcome::Accepted);
                        }
                        (Some(acct), Err(_)) => {
                            pending_acks.push(AckOutcome::Rejected(acct));
                        }
                        (None, _) => {}
                    }
                    if let Err(e) = result {
                        pending_errors.push(e);
                    }
                }
            }

            for ack in pending_acks {
                self.emit_ack(ack).await;
            }
            for err in pending_errors {
                let _ = self.result_sender.send(Err(err)).await;
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

    /// Best-effort emit on the ack channel. A dropped receiver (generator
    /// has exited) silently no-ops rather than aborting the sender.
    async fn emit_ack(&self, outcome: AckOutcome) {
        if let Some(ack) = &self.ack_sender {
            let _ = ack.send(outcome).await;
        }
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
            if !self.rate_limiter.wait().await {
                break;
            }
            let TxSource::Backpressure(generator) = &mut self.tx_source else {
                unreachable!("run_backpressure called in channel mode");
            };
            let Some((signed_tx, account_index)) = generator.next_tx().await? else {
                break;
            };
            let result = self.send_and_wait(signed_tx).await?;
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
        tx: TxEnvelope,
    ) -> Result<(u64, usize, u64, alloy_primitives::B256)> {
        let tx_len = tx.encode_2718_len();

        let mut buf = Vec::with_capacity(tx_len);
        tx.encode_2718(&mut buf);

        let tx_hash = compute_tx_hash(&buf);
        let payload = format!("0x{}", hex::encode(buf));
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
    /// Returns a [`ChannelSendOutcome`] so the caller knows whether the
    /// response has already been seen (and thus an `AckOutcome` can be
    /// emitted inline) or is still in flight (and thus the request_id
    /// should be parked in the per-client inflight deque until the async
    /// drain pairs it with a response).
    ///
    /// If latency tracking is enabled, records the submission only when the
    /// result is successful. With `wait_response` enabled, this means only
    /// transactions accepted by the node are tracked. Without it, the node's
    /// response is not checked synchronously here, so all dispatched txs are
    /// tracked optimistically at submit time and the async drain may later
    /// surface errors against the same tx.
    async fn send(&mut self, tx: TxEnvelope, wait_response: bool) -> Result<ChannelSendOutcome> {
        // Capture timestamp before sending for accurate latency measurement
        let submitted_time = timestamp_now();
        let (request_id, node_idx, tx_len, tx_hash) = self.dispatch_raw_tx(tx).await?;

        if request_id == 0 {
            return Ok(ChannelSendOutcome::DispatchFailed);
        }

        if wait_response {
            let result = self.ws_clients[node_idx]
                .wait_for_response(request_id)
                .await
                .map(|_: String| tx_len);
            let outcome = if result.is_ok() {
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
                ChannelSendOutcome::WaitedAccepted
            } else {
                ChannelSendOutcome::WaitedRejected
            };
            self.result_sender.send(result).await?;
            Ok(outcome)
        } else {
            // Fire-and-forget: record submission optimistically (the drain
            // path will surface real errors against the same request_id).
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
            self.result_sender.send(Ok(tx_len)).await?;
            Ok(ChannelSendOutcome::Fired {
                request_id,
                node_idx,
            })
        }
    }

    /// Send a transaction and always wait for the JSON-RPC response.
    /// Reports to result tracker in all cases. When latency tracking is
    /// enabled, records a [`TxSubmitted`] event on acceptance so the
    /// tracker can correlate submit time with finalized inclusion.
    async fn send_and_wait(&mut self, tx: TxEnvelope) -> Result<SendOutcome> {
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
        let is_send_timeout = |e: &eyre::Report| {
            matches!(
                e.downcast_ref::<tungstenite::error::Error>(),
                Some(tungstenite::error::Error::Io(io_err)) if io_err.kind() == std::io::ErrorKind::TimedOut
            )
        };

        let max_attempts = reconnect_attempts.max(1); // At least one attempt
        for attempt in 0..max_attempts {
            let send_started = Instant::now();
            match ws_client.request(method, params.clone()).await {
                Ok(req_id) => return Ok(req_id),
                Err(e) if is_connection_error(&e) => {
                    let elapsed = send_started.elapsed();
                    let hint = if is_send_timeout(&e) {
                        " (EL may be backpressuring)"
                    } else {
                        ""
                    };
                    warn!(
                        "TxSender {}: connection error on attempt {}/{} to {} after {:.1}s{}: {}",
                        sender_id,
                        attempt + 1,
                        max_attempts,
                        ws_client.url,
                        elapsed.as_secs_f32(),
                        hint,
                        e
                    );

                    // Try to reconnect
                    if let Err(reconnect_err) = ws_client.reconnect().await {
                        warn!(
                            "TxSender {}: reconnect failed on attempt {}/{} after {:.1}s: {}",
                            sender_id,
                            attempt + 1,
                            max_attempts,
                            send_started.elapsed().as_secs_f32(),
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

                    // Reconnected successfully. The total dead time is the timeout that
                    // just elapsed plus the upcoming sleep, during which no transactions
                    // are sent. Log at warn so this stall is visible without -vvv.
                    warn!(
                        "TxSender {}: reconnected after {:.1}s; sleeping {}ms before retry — total send stall ≈ {:.1}s",
                        sender_id,
                        send_started.elapsed().as_secs_f32(),
                        reconnect_period.as_millis(),
                        (send_started.elapsed() + reconnect_period).as_secs_f32(),
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
