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

use std::time::Duration;

use color_eyre::eyre::{eyre, Result};
use serde::Deserialize;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{debug, error, warn};

use crate::latency::timestamp::timestamp_now;
use crate::ws::{WsClient, WsClientBuilder};

/// Capacity of the block event channel between [`BlockStream`] and
/// the [`super::tracker::LatencyTracker`].
const BLOCK_CHANNEL_CAPACITY: usize = 100;

/// Capacity of the internal header notification channel between the
/// [`BlockStream`] producer and consumer.
const HEADER_CHANNEL_CAPACITY: usize = 100;

/// Maximum number of retries when a block fetch returns `null`.
///
/// Nodes may transiently return `null` for a just-announced block
/// before it is fully queryable.
const BLOCK_FETCH_MAX_RETRIES: u32 = 3;

/// Delay between block fetch retries.
const BLOCK_FETCH_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Initial backoff delay for [`BlockStream`] reconnection.
const RECONNECT_INITIAL_DELAY: Duration = Duration::from_secs(1);

/// Maximum backoff delay for [`BlockStream`] reconnection.
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

/// JSON-RPC API name for fetching a block by hash.
const ETH_GET_BLOCK_BY_HASH: &str = "eth_getBlockByHash";

/// JSON-RPC API name for fetching a block by number.
const ETH_GET_BLOCK_BY_NUMBER: &str = "eth_getBlockByNumber";

/// A header notification captured by the producer, paired with the
/// wall-clock time of arrival.
struct HeaderNotification {
    /// Block hash from the `newHeads` notification.
    hash: String,
    /// Block height (hex) from the `newHeads` notification.
    height: String,
    /// `timestamp_now()` captured when the notification was received.
    received_at: u64,
}

/// A full block fetched by the consumer, carrying the original
/// header-arrival timestamp for latency measurement.
pub(super) struct BlockEvent {
    pub block: RpcBlock,
    /// Propagated from [`HeaderNotification::received_at`].
    pub received_at: u64,
}

/// Minimal header returned by `eth_subscribe("newHeads")`.
#[derive(Debug, Deserialize)]
struct NewHeadHeader {
    /// Block hash.
    hash: String,
    /// Block height as hex string.
    number: String,
}

/// A minimal JSON-RPC block shape used for scanning tx hashes.
#[derive(Debug, Deserialize)]
pub(super) struct RpcBlock {
    /// Block height as a hex string.
    /// Ethereum JSON-RPC API returns the field as "number".
    #[serde(rename = "number")]
    pub height: String,
    /// Block hash as a hex string.
    pub hash: String,
    /// Timestamp as a hex string.
    pub timestamp: String,
    /// Transaction hashes (0x-prefixed), if requested as hashes.
    #[serde(default)]
    pub transactions: Vec<String>,
}

/// Parse a 0x-prefixed hex string as `u64`.
///
/// Returns `None` if parsing fails. The `0x` prefix is optional.
pub(super) fn parse_hex_u64(v: &str) -> Option<u64> {
    let s = v.strip_prefix("0x").unwrap_or(v);
    u64::from_str_radix(s, 16).ok()
}

/// Outcome of a block fetch with retries.
enum FetchResult {
    /// Block fetched successfully.
    Ok(RpcBlock),
    /// Block was `null` after all retries.
    /// This happens if the block wasn't fully indexed by the node, but in practice
    /// it should be a very rare occurrence.
    NotFound,
    /// RPC or connection error from the underlying WS client.
    Err(color_eyre::eyre::Report),
}

/// Subscribes to `newHeads` and fetches full blocks via a
/// producer-consumer model.
///
/// - Producer: listens for `newHeads` notifications, timestamps
///   each one immediately, and enqueues a [`HeaderNotification`].
/// - Consumer: dequeues notifications, fetches the full block via
///   `eth_getBlockByHash`, and sends [`BlockEvent`]s to the
///   [`super::LatencyTracker`].
///
/// On the first notification the consumer performs a catch-up scan
/// from `start_height` to fill any gap between tracker initialization
/// and the subscription start.
pub(super) struct BlockStream;

impl BlockStream {
    /// Spawn subscriber and consumer tasks, returning the receiver end
    /// of the block-event channel.
    ///
    /// Both tasks run until `block_sender` is closed
    /// ([`super::LatencyTracker`] dropped) or an unrecoverable error
    /// occurs.
    pub fn spawn(ws_builder: WsClientBuilder, start_height: u64) -> Receiver<BlockEvent> {
        let (block_sender, block_receiver) = mpsc::channel::<BlockEvent>(BLOCK_CHANNEL_CAPACITY);
        let (header_sender, header_receiver) =
            mpsc::channel::<HeaderNotification>(HEADER_CHANNEL_CAPACITY);

        let subscriber_builder = ws_builder.clone();
        tokio::spawn(async move {
            if let Err(e) = Self::run_subscriber(subscriber_builder, header_sender).await {
                error!("BlockStream subscriber: {e}");
            }
        });

        tokio::spawn(async move {
            if let Err(e) =
                Self::run_downloader(ws_builder, start_height, header_receiver, block_sender).await
            {
                error!("BlockStream downloader: {e}");
            }
        });

        block_receiver
    }

    /// Subscriber loop: subscribe to `newHeads` and enqueue
    /// timestamped header notifications.
    ///
    /// Retries on connection errors with exponential backoff up to
    /// [`RECONNECT_MAX_DELAY`]. Returns an error after a failed
    /// attempt at the maximum backoff.
    async fn run_subscriber(
        ws_builder: WsClientBuilder,
        header_sender: Sender<HeaderNotification>,
    ) -> Result<()> {
        let mut backoff = RECONNECT_INITIAL_DELAY;

        loop {
            let session_start = tokio::time::Instant::now();
            match Self::subscriber_session(&ws_builder, &header_sender).await {
                Ok(()) => {
                    debug!("BlockStream subscriber: channel closed");
                    return Ok(());
                }
                Err(e) => {
                    if session_start.elapsed() > RECONNECT_INITIAL_DELAY {
                        backoff = RECONNECT_INITIAL_DELAY;
                    }
                    if backoff >= RECONNECT_MAX_DELAY {
                        return Err(eyre!("giving up after max backoff: {e}"));
                    }
                    warn!("BlockStream subscriber: connection error: {e}. Retrying in {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RECONNECT_MAX_DELAY);
                }
            }
        }
    }

    /// A single subscriber session: connect, subscribe, and forward
    /// headers until disconnection.
    async fn subscriber_session(
        ws_builder: &WsClientBuilder,
        header_sender: &Sender<HeaderNotification>,
    ) -> Result<()> {
        let mut ws_client = ws_builder.clone().build().await?;

        let _sub_id: String = ws_client.subscribe(serde_json::json!(["newHeads"])).await?;

        debug!("BlockStream subscriber: subscribed to newHeads");

        loop {
            let header: NewHeadHeader = ws_client.next_notification().await?;
            let received_at = timestamp_now();

            if header_sender
                .send(HeaderNotification {
                    hash: header.hash,
                    height: header.number,
                    received_at,
                })
                .await
                .is_err()
            {
                // downloader dropped; shut down.
                return Ok(());
            }
        }
    }

    /// Block downloader loop: reconnect and delegate to
    /// [`Self::downloader_session`] until the header channel closes.
    ///
    /// Mirrors the `run_subscriber` / `subscriber_session` pattern:
    /// this function owns the reconnect-with-backoff logic, while
    /// the session function processes notifications on a single WS
    /// connection.
    async fn run_downloader(
        ws_builder: WsClientBuilder,
        start_height: u64,
        mut header_receiver: Receiver<HeaderNotification>,
        block_sender: Sender<BlockEvent>,
    ) -> Result<()> {
        let mut next_expected_height = start_height;
        let mut backoff = RECONNECT_INITIAL_DELAY;

        loop {
            let session_start = tokio::time::Instant::now();
            match Self::downloader_session(
                &ws_builder,
                &mut next_expected_height,
                &mut header_receiver,
                &block_sender,
            )
            .await
            {
                Ok(()) => {
                    debug!("BlockStream downloader: session ended");
                    return Ok(());
                }
                Err(e) => {
                    if session_start.elapsed() > RECONNECT_INITIAL_DELAY {
                        backoff = RECONNECT_INITIAL_DELAY;
                    }
                    if backoff >= RECONNECT_MAX_DELAY {
                        return Err(eyre!("giving up after max backoff: {e}"));
                    }
                    warn!("BlockStream downloader: connection error: {e}. Retrying in {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RECONNECT_MAX_DELAY);
                }
            }
        }
    }

    /// A single downloader session: connect, process header
    /// notifications, fetch full blocks, and forward them to the
    /// tracker.
    ///
    /// Returns `Ok(())` when the header channel closes (clean
    /// shutdown) or the tracker is dropped. Returns `Err` on WS
    /// errors so the caller can reconnect.
    async fn downloader_session(
        ws_builder: &WsClientBuilder,
        next_expected_height: &mut u64,
        header_receiver: &mut Receiver<HeaderNotification>,
        block_sender: &Sender<BlockEvent>,
    ) -> Result<()> {
        let mut ws_client = ws_builder.clone().build().await?;

        while let Some(notification) = header_receiver.recv().await {
            let height = match parse_hex_u64(&notification.height) {
                Some(h) => h,
                None => {
                    warn!(
                        "BlockStream downloader: unparseable height {:?}, skipping",
                        notification.height
                    );
                    continue;
                }
            };

            // Fill gap from next_expected to notification height.
            // On the first iteration this is the initial catch-up;
            // later it fills gaps from subscriber reconnects.
            if height > *next_expected_height {
                Self::catch_up_scan(
                    &mut ws_client,
                    *next_expected_height,
                    height - 1,
                    notification.received_at,
                    block_sender,
                )
                .await?;
            }

            // Fetch the notified block by hash.
            match Self::fetch_block(&mut ws_client, ETH_GET_BLOCK_BY_HASH, &notification.hash).await
            {
                FetchResult::Ok(block) => {
                    let h = parse_hex_u64(&block.height).unwrap_or(height);
                    if block_sender
                        .send(BlockEvent {
                            block,
                            received_at: notification.received_at,
                        })
                        .await
                        .is_err()
                    {
                        return Ok(()); // tracker dropped.
                    }
                    *next_expected_height = h + 1;
                }
                FetchResult::NotFound => {
                    warn!(
                        "BlockStream downloader: block not found for hash {} (height {}) after retries",
                        notification.hash, notification.height
                    );
                }
                FetchResult::Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    /// Fetch blocks from `start_height` through `end_height`
    /// (inclusive) and send them as [`BlockEvent`]s.
    ///
    /// Returns `Err` if a block cannot be fetched after retries, so
    /// the caller can rebuild the client and retry the scan.
    async fn catch_up_scan(
        ws_client: &mut WsClient,
        start_height: u64,
        end_height: u64,
        received_at: u64,
        block_sender: &Sender<BlockEvent>,
    ) -> Result<()> {
        if start_height > end_height {
            return Ok(());
        }
        debug!(
            "BlockStream downloader: catch-up scan from block {} to {}",
            start_height, end_height
        );
        for height in start_height..=end_height {
            let hex_height = format!("0x{height:x}");
            match Self::fetch_block(ws_client, ETH_GET_BLOCK_BY_NUMBER, &hex_height).await {
                FetchResult::Ok(block) => {
                    // Send the block to the tracker.
                    // The timestamp for catch-up blocks is the
                    // arrival time of the first notification.
                    // TODO: should we use the blocks' timestamps?
                    if block_sender
                        .send(BlockEvent { block, received_at })
                        .await
                        .is_err()
                    {
                        return Ok(()); // tracker dropped.
                    }
                }
                FetchResult::NotFound => {
                    return Err(eyre!(
                        "block at height {} returned null after retries",
                        height
                    ));
                }
                FetchResult::Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Fetch a block via an RPC method, retrying on `null`
    /// responses.
    ///
    /// `rpc_method` is the JSON-RPC API name
    /// (e.g. `"eth_getBlockByHash"`). `hash_or_height` is either the block hash
    ///  or its height in hex format.
    async fn fetch_block(
        ws_client: &mut WsClient,
        rpc_method: &str,
        hash_or_height: &str,
    ) -> FetchResult {
        for attempt in 0..BLOCK_FETCH_MAX_RETRIES {
            let params = serde_json::json!([hash_or_height, false]);
            let block: Option<RpcBlock> = match ws_client.request_response(rpc_method, params).await
            {
                Ok(rpc_block) => rpc_block,
                Err(e) => return FetchResult::Err(e),
            };
            if let Some(block) = block {
                return FetchResult::Ok(block);
            }
            if attempt + 1 < BLOCK_FETCH_MAX_RETRIES {
                debug!(
                    "BlockStream downloader: {rpc_method} null for block {hash_or_height}, retry {}/{}",
                    attempt + 1,
                    BLOCK_FETCH_MAX_RETRIES
                );
                tokio::time::sleep(BLOCK_FETCH_RETRY_DELAY).await;
            }
        }
        FetchResult::NotFound
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_u64_table() {
        let cases: &[(&str, Option<u64>, &str)] = &[
            ("0x10", Some(16), "with 0x prefix"),
            ("0xff", Some(255), "lowercase hex with 0x"),
            ("0xFFFF", Some(65535), "uppercase hex with 0x"),
            ("10", Some(16), "without 0x prefix"),
            ("ff", Some(255), "lowercase hex without 0x"),
            ("xyz", None, "invalid hex letters"),
            ("0xGHI", None, "invalid hex with 0x prefix"),
            ("hello", None, "non-hex string"),
            ("", None, "empty string"),
        ];

        for (input, expected, desc) in cases {
            assert_eq!(
                parse_hex_u64(input),
                *expected,
                "case '{}': parse_hex_u64({:?})",
                desc,
                input
            );
        }
    }
}
