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

use alloy_rpc_types_txpool::TxpoolStatus;
use color_eyre::eyre::Result;
use core::fmt;
use serde_json::json;
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;
use tokio::time::{self, Duration, Instant};

use crate::ws::{WsClient, WsClientBuilder};

pub(crate) struct ResultTracker {
    ws_clients: Vec<WsClient>,
    result_receiver: Receiver<Result<u64>>,
    finish_receiver: Receiver<()>,
    silent: bool,
    show_pool_status: bool,
}

/// Aggregate statistics from a single [`ResultTracker::run`].
pub(crate) struct ResultSummary {
    /// Per-category server error counts (grouped by `(code, head_message)`).
    pub errors: HashMap<String, u64>,
    /// Total transactions locally submitted, regardless of server acceptance.
    pub total_sent: u64,
    /// Total transaction bytes locally submitted, regardless of server acceptance.
    pub total_bytes: u64,
    /// Wall-clock duration of the tracker's run.
    pub elapsed: Duration,
}

impl ResultTracker {
    pub async fn new(
        ws_client_builders: Vec<WsClientBuilder>,
        result_receiver: Receiver<Result<u64>>,
        finish_receiver: Receiver<()>,
        silent: bool,
        show_pool_status: bool,
    ) -> Result<Self> {
        let mut ws_clients = Vec::new();
        for builder in ws_client_builders {
            ws_clients.push(builder.build().await?);
        }
        Ok(Self {
            ws_clients,
            result_receiver,
            finish_receiver,
            silent,
            show_pool_status,
        })
    }

    /// Track and report statistics on sent transactions.
    ///
    /// Returns the aggregated error counter, the total number of locally-sent
    /// transactions (regardless of server acceptance), and the wall-clock
    /// duration of the run. The caller computes locally-observed TPS as
    /// `total_sent / elapsed.as_secs_f64()`.
    pub async fn run(&mut self) -> Result<ResultSummary> {
        // Initialize counters
        let start_time = Instant::now();
        let mut stats = Stats::new(start_time);

        let mut interval = time::interval(Duration::from_secs(1));
        let _ = interval.tick().await; // consume first tick at 0 seconds

        loop {
            tokio::select! {
                // Stop tracking
                _ = self.finish_receiver.recv() => {
                    break;
                }
                // Update counters
                res = self.result_receiver.recv() => {
                    match res {
                        Some(Ok(tx_length)) => stats.incr_ok(tx_length),
                        Some(Err(error)) => stats.incr_err(&error.to_string()),
                        None => break,
                    }
                }
                // Report stats every second
                _ = interval.tick() => {
                    if !self.silent {
                        let len = self.ws_clients.len();
                        let mut debug_output = String::new();
                        if self.show_pool_status {
                            let statuses = self.get_txpool_statuses().await?
                                .iter().take(10).enumerate()
                                .map(|(i, (queued, pending))| format!("{i}:({queued:>5},{pending:>3})"))
                                .collect::<Vec<_>>()
                                .join("; ");
                            let sep = if len > 7 { "\n  " } else { " -- " };
                            debug_output = format!("{sep}Pool status: {statuses}");
                        };
                        println!("* {stats}{debug_output}");
                    }
                    stats.reset();
                }
            }
        }
        println!("{}", stats.total_display());
        Ok(ResultSummary {
            errors: stats.total_errors,
            total_sent: stats.total_succeed,
            total_bytes: stats.total_bytes,
            elapsed: start_time.elapsed(),
        })
    }

    async fn get_txpool_statuses(&mut self) -> Result<Vec<(u64, u64)>> {
        let mut statuses = Vec::new();
        for client in self.ws_clients.iter_mut() {
            // Gracefully handle connection failures during node upgrades/restarts
            match client
                .request_response::<TxpoolStatus>("txpool_status", json!([]))
                .await
            {
                Ok(pool_status) => {
                    statuses.push((pool_status.queued, pool_status.pending));
                }
                Err(_) => {
                    // Push zeros for unavailable nodes
                    statuses.push((0, 0));
                }
            }
        }
        Ok(statuses)
    }
}

/// Statistics on sent transactions.
struct Stats {
    start_time: Instant,
    succeed: u64,
    bytes: u64,
    errors_counter: HashMap<String, u64>,
    total_succeed: u64,
    total_bytes: u64,
    total_errors: HashMap<String, u64>,
}

impl Stats {
    fn new(start_time: Instant) -> Self {
        Self {
            start_time,
            succeed: 0,
            bytes: 0,
            errors_counter: HashMap::new(),
            total_succeed: 0,
            total_bytes: 0,
            total_errors: HashMap::new(),
        }
    }

    fn incr_ok(&mut self, tx_length: u64) {
        self.succeed += 1;
        self.bytes += tx_length;
        self.total_succeed += 1;
        self.total_bytes += tx_length;
    }

    fn incr_err(&mut self, error: &str) {
        let category = categorize_error(error);
        self.errors_counter
            .entry(category.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        self.total_errors
            .entry(category)
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }

    fn reset(&mut self) {
        self.succeed = 0;
        self.bytes = 0;
        self.errors_counter.clear();
    }

    fn total_display(&self) -> String {
        let elapsed = self.start_time.elapsed().as_millis() as f64 / 1000f64;
        let mut stats = String::new();
        let tps = self.total_succeed as f64 / elapsed; // since the start of the load
        stats += &format!(
            "{:>7.3}s: Total sent {:>5} txs ({:>6} bytes), {:>4.1} tx/s",
            elapsed, self.total_succeed, self.total_bytes, tps
        );
        for (error, count) in self.total_errors.iter() {
            stats += &format!("\n  - \x1b[31m{count} failed\x1b[0m with \"{error}\"");
        }
        stats
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let elapsed = self.start_time.elapsed().as_millis() as f64 / 1000f64;
        let mut stats = String::new();
        let tps = self.total_succeed as f64 / elapsed; // since the start of the load
        stats += &format!(
            "{:>7.3}s: Sent {:>5} txs ({:>6} bytes), {:>4.1} tx/s",
            elapsed, self.succeed, self.bytes, tps
        );
        for (error, count) in self.errors_counter.iter() {
            stats += &format!(", \x1b[31m{count} failed\x1b[0m with \"{error}\"");
        }
        write!(f, "{}", stats)
    }
}

/// Collapse server error strings to a stable `(code, head_message)` key.
///
/// Server errors arrive as `Server Error <code>: <message>[: <variable_data>]`.
/// The third token onwards holds varying data (nonces, addresses, balances)
/// that would otherwise explode the cardinality of the error map. The first
/// two tokens — error code and message head — uniquely identify the failure
/// mode, since `-32000` covers several distinct categories.
///
/// Errors that don't follow this shape (fewer than two `: `-separated tokens)
/// are returned unchanged so they remain visible rather than being silently
/// merged with unrelated entries.
fn categorize_error(error: &str) -> String {
    error.splitn(3, ": ").take(2).collect::<Vec<_>>().join(": ")
}

#[cfg(test)]
mod tests {
    use super::categorize_error;

    #[test]
    fn nonce_too_low_collapses_across_distinct_nonces() {
        let a =
            categorize_error("Server Error -32000: nonce too low: next nonce 962, tx nonce 846");
        let b =
            categorize_error("Server Error -32000: nonce too low: next nonce 943, tx nonce 930");
        assert_eq!(a, "Server Error -32000: nonce too low");
        assert_eq!(a, b);
    }

    #[test]
    fn same_code_distinct_messages_stay_separate() {
        let pool = categorize_error("Server Error -32003: txpool is full");
        let funds = categorize_error(
            "Server Error -32003: insufficient funds for gas * price + value: address 0xabc balance 0 want 1",
        );
        assert_ne!(pool, funds);
        assert_eq!(pool, "Server Error -32003: txpool is full");
        assert_eq!(
            funds,
            "Server Error -32003: insufficient funds for gas * price + value"
        );
    }

    #[test]
    fn message_without_variable_suffix_is_preserved() {
        let s = categorize_error("Server Error -32003: txpool is full");
        assert_eq!(s, "Server Error -32003: txpool is full");
    }

    #[test]
    fn single_token_error_is_returned_as_is() {
        let s = categorize_error("connection closed");
        assert_eq!(s, "connection closed");
    }
}
