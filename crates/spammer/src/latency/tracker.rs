// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
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
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use tokio::sync::mpsc::Receiver;
use tracing::{debug, info, warn};

use alloy_primitives::{keccak256, TxHash};

use crate::latency::block_stream::{parse_hex_u64, BlockEvent, BlockStream, RpcBlock};
use crate::latency::csv::{BlockInfo, CsvRow};
use crate::latency::timestamp::{
    format_rfc3339_millis_utc, format_rfc3339_secs_utc, timestamp_now,
};

use crate::ws::WsClientBuilder;

/// Flush to csv file to disk after this many appended rows.
const FLUSH_THRESHOLD: u64 = 10_000;

/// Maximum time to wait for a tx to finalize before evicting from the
/// submitted transactions set.
const SUBMITTED_TX_TTL: Duration = Duration::from_secs(300);

/// Check for stale entries every N blocks received.
///
/// At ~500ms block times this triggers eviction roughly every 10
/// seconds, matching the cadence of the previous poll-based approach.
const EVICTION_CHECK_INTERVAL: u32 = 20;

/// Maximum time to spend draining block events after all
/// senders finish. Blocks arriving after this deadline are
/// ignored.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// A submission event emitted by the sender.
///
/// This exists so the latency tracker can correlate the local submit
/// time with the eventual finalized inclusion.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TxSubmitted {
    /// The transaction hash as computed from EIP-2718 encoded bytes.
    pub tx_hash: TxHash,
    /// Milliseconds since Unix epoch when the tx was submitted.
    pub submitted_time: u64,
}

/// Compute the transaction hash from its EIP-2718 encoded bytes.
///
/// This avoids waiting for the JSON-RPC `eth_sendRawTransaction`
/// response.
pub(crate) fn compute_tx_hash(eip2718_bytes: &[u8]) -> TxHash {
    keccak256(eip2718_bytes)
}

/// Track submit-to-finalized transaction latency and write a CSV log.
///
/// Receives transaction submissions from senders and block events
/// from an internal [`BlockStream`]. Matches submitted transaction
/// hashes against block contents to compute inclusion latency.
pub(crate) struct LatencyTracker {
    /// Builder used to create [`BlockStream`] clients.
    ///
    /// Wrapped in `Option` so `run()` can move it out while
    /// continuing to use other fields via `self`. Always `Some`
    /// after construction; `take()`n exactly once in `run()`.
    ws_builder: Option<WsClientBuilder>,
    /// Channel receiving submission events from senders.
    tx_submission_receiver: Receiver<TxSubmitted>,
    /// Map from tx hash to submit time (unix ms) for not yet
    /// finalized txs.
    submitted_txs: HashMap<TxHash, u64>,
    /// Buffered writer for the CSV output file.
    csv_writer: BufWriter<File>,
    /// Path to the CSV output file, for the final summary log.
    csv_path: PathBuf,
    /// Block height when the tracker was created.
    ///
    /// Between tracker creation and the `newHeads` subscription
    /// becoming active, blocks may be produced. The
    /// [`BlockStream`] performs a catch-up scan from this height
    /// so transactions finalized in those blocks are not missed.
    start_height: u64,
    /// How many rows have been appended since the last flush.
    rows_since_flush: u64,
    /// Block counter for periodic eviction checks.
    block_count: u32,
}

impl LatencyTracker {
    /// Initialize the tracker and prepare the CSV output file.
    ///
    /// Captures the current block height so that the [`BlockStream`]
    /// can perform a catch-up scan from this point.
    pub async fn new(
        ws_builder: WsClientBuilder,
        submission_receiver: Receiver<TxSubmitted>,
        csv_path: PathBuf,
    ) -> Result<Self> {
        let mut ws_client = ws_builder.clone().build().await?;

        // Fetch starting block height before any transactions are
        // submitted.
        let params = serde_json::json!(["latest", false]);
        let block: Option<RpcBlock> = ws_client
            .request_response("eth_getBlockByNumber", params)
            .await?;
        let start_height = block
            .and_then(|b| parse_hex_u64(&b.height))
            .ok_or_else(|| eyre!("Failed to get starting block height"))?;

        let file = File::create(&csv_path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(Self::csv_header().as_bytes())?;
        writer.flush()?;

        Ok(Self {
            ws_builder: Some(ws_builder),
            tx_submission_receiver: submission_receiver,
            submitted_txs: HashMap::new(),
            csv_writer: writer,
            csv_path,
            start_height,
            rows_since_flush: 0,
            block_count: 0,
        })
    }

    /// Execute the main tracking loop.
    ///
    /// Spawns a [`BlockStream`] that subscribes to new block headers
    /// and fetches full blocks. The manager then `select!`s over
    /// transaction submissions and block events.
    ///
    /// Exits when the submission channel closes (all senders finished).
    pub async fn run(mut self) -> Result<()> {
        let ws_builder = self
            .ws_builder
            .take()
            .ok_or_else(|| eyre!("ws_builder already consumed"))?;
        let mut block_receiver = BlockStream::spawn(ws_builder, self.start_height);
        let mut txs_submissions_recv: u64 = 0;
        let mut txs_finalized: u64 = 0;

        loop {
            tokio::select! {
                // comes from the senders submitting transactions.
                tx_submission = self.tx_submission_receiver.recv() => {
                    match tx_submission {
                        Some(sub) => {
                            self.submitted_txs.insert(
                                sub.tx_hash,
                                sub.submitted_time,
                            );
                            txs_submissions_recv += 1;
                        }
                        None => {
                            // Channel closed: senders finished.
                            break;
                        }
                    }
                }
                // comes from the BlockStream when a new block is finalized.
                block_event = block_receiver.recv() => {
                    match block_event {
                        Some(evt) => {
                            // Move any buffered submissions into submitted_txs
                            // before scanning, so transactions submitted before
                            // this block arrived are not missed.
                            while let Ok(sub) = self.tx_submission_receiver.try_recv() {
                                self.submitted_txs.insert(sub.tx_hash, sub.submitted_time);
                                txs_submissions_recv += 1;
                            }

                            txs_finalized += self.scan_and_record_block(
                                &evt.block,
                                evt.received_at,
                            )?;

                            self.block_count += 1;
                            if self.block_count
                                .is_multiple_of(EVICTION_CHECK_INTERVAL)
                            {
                                self.evict_stale_txs();
                            }

                            debug!(
                                pending_txs_count = self.submitted_txs.len(),
                                finalized_txs_count = txs_finalized,
                                "LatencyTracker: awaiting finalization"
                            );
                        }
                        None => {
                            // BlockStream died. Propagate as error
                            // so the caller knows tracking stopped.
                            return Err(eyre!(
                                "BlockStream channel closed unexpectedly; latency tracking is incomplete"
                            ));
                        }
                    }
                }
            }
        }

        txs_finalized += self.drain_remaining_blocks(&mut block_receiver).await?;

        let txs_not_finalized = self.submitted_txs.len();
        info!(
            "LatencyTracker finished: \
             {} txs found in blocks out of {} submitted, \
             {} txs not found in blocks. \
             CSV file: {}",
            txs_finalized,
            txs_submissions_recv,
            txs_not_finalized,
            self.csv_path.display(),
        );

        self.csv_writer.flush()?;
        Ok(())
    }

    /// Evict submitted transactions older than [`SUBMITTED_TX_TTL`].
    ///
    /// Transactions that are never finalized (dropped from mempool,
    /// underpriced, etc.) would otherwise accumulate indefinitely.
    /// This bounds memory usage during long runs with high
    /// transaction drop rates.
    fn evict_stale_txs(&mut self) {
        let evicted = evict_stale_from_submitted_txs(&mut self.submitted_txs, SUBMITTED_TX_TTL);
        if evicted > 0 {
            debug!(
                "Evicted {} stale submitted transactions (older than {}s)",
                evicted,
                SUBMITTED_TX_TTL.as_secs()
            );
        }
    }

    /// Drain remaining block events for up to [`DRAIN_TIMEOUT`].
    ///
    /// After all senders finish, in-flight block fetches in
    /// [`BlockStream`] may still deliver blocks containing the
    /// last submitted transactions. This method processes them
    /// so those final matches are not lost.
    async fn drain_remaining_blocks(
        &mut self,
        block_receiver: &mut Receiver<BlockEvent>,
    ) -> Result<u64> {
        let mut matched = 0u64;
        let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, block_receiver.recv()).await {
                Ok(Some(evt)) => {
                    matched += self.scan_and_record_block(&evt.block, evt.received_at)?;
                }
                _ => break, // timeout or channel closed
            }
        }
        Ok(matched)
    }

    /// Record latency data for all submitted transactions included
    /// in this block. Returns the number of matched transactions.
    fn scan_and_record_block(&mut self, block: &RpcBlock, block_observed_at: u64) -> Result<u64> {
        let Some(blk_height) = parse_hex_u64(&block.height) else {
            return Ok(0);
        };
        let blk_hash = &block.hash;
        let blk_timestamp_secs = match parse_hex_u64(&block.timestamp) {
            Some(ts) => ts,
            None => {
                warn!(
                    "unparseable block timestamp {:?} at height {}, using wall clock",
                    block.timestamp, blk_height
                );
                // block timestamp is in seconds.
                timestamp_now() / 1000
            }
        };

        let matches = match_submitted_txs(&mut self.submitted_txs, &block.transactions);
        let matched_count = matches.len() as u64;

        for (tx_hash, submitted_ms) in matches.into_iter() {
            let submitted_at = format_rfc3339_millis_utc(submitted_ms);
            let finalized_at = format_rfc3339_millis_utc(block_observed_at);
            let blk_timestamp_at = format_rfc3339_secs_utc(blk_timestamp_secs);
            let row = CsvRow::new(
                tx_hash,
                submitted_at,
                finalized_at,
                BlockInfo {
                    height: blk_height,
                    hash: blk_hash.to_string(),
                    timestamp: blk_timestamp_at,
                },
            );
            self.append_csv_row(&row)?;
        }
        Ok(matched_count)
    }

    /// Write a latency record to the CSV file.
    ///
    /// Batches writes and flushes periodically.
    fn append_csv_row(&mut self, row: &CsvRow) -> Result<()> {
        let ts = &row.block.timestamp;

        writeln!(
            self.csv_writer,
            "{},{},{},{},{},{}",
            row.tx_hash,
            row.submitted_at,
            row.finalized_observed_at,
            row.block.height,
            row.block.hash,
            ts,
        )?;
        self.rows_since_flush = self.rows_since_flush.saturating_add(1);
        if self.rows_since_flush >= FLUSH_THRESHOLD {
            self.csv_writer.flush()?;
            self.rows_since_flush = 0;
        }
        Ok(())
    }

    /// Return the CSV header row for the latency log.
    ///
    /// Column order must match the format string in
    /// [`LatencyTracker::append_csv_row`].
    fn csv_header() -> &'static str {
        concat!(
            "tx_hash,submitted_at,finalized_observed_at,",
            "included_block_number,included_block_hash,",
            "included_block_timestamp\n",
        )
    }
}

/// Remove submitted transactions that appear in the given list of
/// transaction hashes.
///
/// Returns a list of matched transactions with their original
/// submission timestamps. Unmatched hashes (transactions not in
/// submitted transactions set) are ignored.
/// Extracted as a free function for unit testing without constructing
/// a full [`LatencyTracker`].
fn match_submitted_txs(
    submitted_txs: &mut HashMap<TxHash, u64>,
    tx_hashes: &[String],
) -> Vec<(String, u64)> {
    let mut matched = Vec::new();
    for tx_hash_str in tx_hashes.iter() {
        let Ok(tx_hash) = tx_hash_str.parse::<TxHash>() else {
            continue;
        };
        let Some(submitted_ms) = submitted_txs.remove(&tx_hash) else {
            continue;
        };
        matched.push((tx_hash_str.clone(), submitted_ms));
    }
    matched
}

/// Evict entries older than `ttl` from the submitted transactions
/// set.
///
/// Returns the number of evicted entries. This is extracted as a
/// free function to enable unit testing without constructing a full
/// [`LatencyTracker`].
fn evict_stale_from_submitted_txs(
    submitted_txs: &mut HashMap<TxHash, u64>,
    ttl: Duration,
) -> usize {
    let now_ms = timestamp_now();
    let ttl_ms = ttl.as_millis() as u64;
    let curr_submitted_txs = submitted_txs.len();
    submitted_txs.retain(|_, submitted_ms| now_ms.saturating_sub(*submitted_ms) < ttl_ms);
    curr_submitted_txs - submitted_txs.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_tx_hash_matches_known_keccak_vector() {
        let got_hash = compute_tx_hash(b"hello");
        let want_hash_str = "0x1c8aff950685c2ed4bc3174f3472287b56d9517b\
             9c948127319a09a7a36deac8"
            .to_string();
        let want_hash = want_hash_str.parse::<TxHash>().expect("valid hash");
        assert_eq!(got_hash, want_hash);
    }

    #[test]
    fn match_submitted_txs_finds_matches() {
        let tx1 = "0x1c8aff950685c2ed4bc3174f3472287b56d9517b\
             9c948127319a09a7a36deac8"
            .to_string();
        let tx2 = "0x00000000000000000000000000000000\
             00000000000000000000000000000001"
            .to_string();
        let tx1_hash = tx1.parse::<TxHash>().expect("tx1 isn't a valid hash");
        let tx2_hash = tx2.parse::<TxHash>().expect("tx2 isn't a valid hash");

        let mut submitted_txs = HashMap::new();
        submitted_txs.insert(tx1_hash, 1_000);
        submitted_txs.insert(tx2_hash, 10_000);

        let block_txs = vec![tx1.clone()];
        let matches = match_submitted_txs(&mut submitted_txs, &block_txs);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, tx1);
        assert_eq!(matches[0].1, 1_000);
        assert!(!submitted_txs.contains_key(&tx1_hash));
        assert!(submitted_txs.contains_key(&tx2_hash));
    }

    #[test]
    fn match_submitted_txs_empty_block() {
        let tx_hash = "0x00000000000000000000000000000000\
             00000000000000000000000000000001"
            .parse::<TxHash>()
            .unwrap();
        let mut submitted_txs = HashMap::new();
        submitted_txs.insert(tx_hash, 1_000);

        let matches = match_submitted_txs(&mut submitted_txs, &[]);

        assert!(matches.is_empty());
        assert_eq!(submitted_txs.len(), 1);
    }

    #[test]
    fn match_submitted_txs_no_submitted() {
        let mut submitted_txs: HashMap<TxHash, u64> = HashMap::new();
        let tx_hash = "0x00000000000000000000000000000000\
             00000000000000000000000000000001";
        let block_txs = vec![tx_hash.to_string()];
        let matches = match_submitted_txs(&mut submitted_txs, &block_txs);

        assert!(matches.is_empty());
    }

    #[test]
    fn evict_stale_from_submitted_txs_no_stale() {
        let mut submitted_txs = HashMap::new();
        let now_ms = timestamp_now();
        let tx_hash = "0x00000000000000000000000000000000\
             00000000000000000000000000000001"
            .parse::<TxHash>()
            .unwrap();
        submitted_txs.insert(tx_hash, now_ms);

        let evicted = evict_stale_from_submitted_txs(&mut submitted_txs, SUBMITTED_TX_TTL);

        assert_eq!(evicted, 0);
        assert_eq!(submitted_txs.len(), 1);
    }

    #[test]
    fn evict_stale_from_submitted_txs_all_stale() {
        let mut submitted_txs = HashMap::new();
        let old_ms = timestamp_now() - SUBMITTED_TX_TTL.as_millis() as u64 - 1000;
        let tx1 = "0x00000000000000000000000000000000\
             00000000000000000000000000000001"
            .parse::<TxHash>()
            .unwrap();
        let tx2 = "0x00000000000000000000000000000000\
             00000000000000000000000000000002"
            .parse::<TxHash>()
            .unwrap();
        submitted_txs.insert(tx1, old_ms);
        submitted_txs.insert(tx2, old_ms - 1000);

        let evicted = evict_stale_from_submitted_txs(&mut submitted_txs, SUBMITTED_TX_TTL);

        assert_eq!(evicted, 2);
        assert!(submitted_txs.is_empty());
    }

    #[test]
    fn evict_stale_from_submitted_txs_mixed() {
        let mut submitted_txs = HashMap::new();
        let now_ms = timestamp_now();
        let old_ms = now_ms - SUBMITTED_TX_TTL.as_millis() as u64 - 1000;

        let fresh_tx = "0x00000000000000000000000000000000\
             00000000000000000000000000000001"
            .parse::<TxHash>()
            .unwrap();
        let stale_tx = "0x00000000000000000000000000000000\
             00000000000000000000000000000002"
            .parse::<TxHash>()
            .unwrap();

        submitted_txs.insert(fresh_tx, now_ms);
        submitted_txs.insert(stale_tx, old_ms);

        let evicted = evict_stale_from_submitted_txs(&mut submitted_txs, SUBMITTED_TX_TTL);

        assert_eq!(evicted, 1);
        assert_eq!(submitted_txs.len(), 1);
        assert!(submitted_txs.contains_key(&fresh_tx));
        assert!(!submitted_txs.contains_key(&stale_tx));
    }
}
