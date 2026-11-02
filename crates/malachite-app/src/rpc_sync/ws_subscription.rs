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

//! WebSocket Subscriptions for RPC Sync
//!
//! Subscribes to `eth_subscribe("newHeads")` on each RPC endpoint to receive
//! real-time block header notifications. The network actor converts these to
//! `NetworkEvent::Status` messages to inform malachite's sync actor about
//! peer heights, triggering block fetch requests when peers are ahead.

use std::time::Duration;

use alloy_provider::{Provider, ProviderBuilder};
use alloy_transport_ws::WsConnect;
use malachitebft_peer::PeerId;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use arc_consensus_types::Height;
use url::Url;

/// Message sent when a peer's tip height is updated
#[derive(Debug, Clone)]
pub struct PeerHeightUpdate {
    pub peer_id: PeerId,
    pub http_url: Url,
    pub height: Height,
}

/// Spawn a WebSocket subscription task for a single endpoint.
///
/// This task:
/// 1. Connects to the WebSocket endpoint
/// 2. Subscribes to newHeads
/// 3. Sends height updates through the channel
/// 4. Reconnects on disconnection
/// 5. If no header arrives within `stale_timeout`, the task errors and the outer
///    backoff loop reconnects
pub fn spawn_ws_subscription(
    http_url: Url,
    ws_url: Url,
    peer_id: PeerId,
    update_tx: mpsc::Sender<PeerHeightUpdate>,
    cancel_token: CancellationToken,
    stale_timeout: Duration,
) {
    tokio::spawn(run_ws_subscription(
        http_url,
        ws_url,
        peer_id,
        update_tx,
        cancel_token,
        stale_timeout,
    ));
}

/// Backoff strategy for WebSocket reconnections
///
/// Uses a Fibonacci backoff with jitter, starting at 5 seconds and maxing out at 60 seconds,
/// no maximum number of attempts (will keep trying indefinitely until cancelled).
const WS_BACKOFF: backon::FibonacciBuilder = backon::FibonacciBuilder::new()
    .with_min_delay(Duration::from_secs(5))
    .with_max_delay(Duration::from_secs(60))
    .without_max_times()
    .with_jitter();

/// Run the WebSocket subscription loop with reconnection
async fn run_ws_subscription(
    http_url: Url,
    ws_url: Url,
    peer_id: PeerId,
    update_tx: mpsc::Sender<PeerHeightUpdate>,
    cancel_token: CancellationToken,
    stale_timeout: Duration,
) {
    use backon::RetryableWithContext;

    struct Context {
        ws_url: Url,
        http_url: Url,
        peer_id: PeerId,
        update_tx: mpsc::Sender<PeerHeightUpdate>,
        stale_timeout: Duration,
    }

    async fn task(ctx: Context) -> (Context, eyre::Result<()>) {
        info!(ws_url = %ctx.ws_url, "Connecting to WebSocket endpoint");
        let result = connect_and_subscribe(
            &ctx.ws_url,
            &ctx.http_url,
            ctx.peer_id,
            &ctx.update_tx,
            ctx.stale_timeout,
        )
        .await;
        (ctx, result)
    }

    let mut retry_attempts: usize = 0;

    let retryable_task = task
        .retry(WS_BACKOFF)
        .context(Context {
            ws_url: ws_url.clone(),
            http_url,
            peer_id,
            update_tx,
            stale_timeout,
        })
        .notify(|error, delay| {
            // Unbounded retries, but at ~60s backoff, overflow takes ~10^10 years
            #[allow(clippy::arithmetic_side_effects)]
            {
                retry_attempts += 1;
            }

            warn!(
                %ws_url, error = format!("{error:#}"), attempt = retry_attempts,
                "WebSocket connection failed, retrying in {delay:?}"
            );
        });

    match cancel_token.run_until_cancelled(retryable_task).await {
        Some((_, Ok(()))) => {
            // Normal termination
            info!(%ws_url, "WebSocket subscription ended normally");
        }
        Some((_, Err(error))) => {
            // The task failed with an error that was not retried (should not happen since we have no max attempts)
            error!(%ws_url, error = format!("{error:#}"), "WebSocket subscription failed with unrecoverable error");
        }
        None => {
            // The WebSocket subscription task was cancelled
            info!(%ws_url, "WebSocket subscription cancelled");
        }
    }
}

/// Connect to a WebSocket endpoint, subscribe to newHeads,
/// and forward height updates until the connection is lost, the channel closes,
/// or `stale_timeout` elapses without any header arriving.
async fn connect_and_subscribe(
    ws_url: &Url,
    http_url: &Url,
    peer_id: PeerId,
    update_tx: &mpsc::Sender<PeerHeightUpdate>,
    stale_timeout: Duration,
) -> eyre::Result<()> {
    // Connect to WebSocket
    let ws = WsConnect::new(ws_url.to_string());
    let provider = ProviderBuilder::new().connect_ws(ws).await?;

    info!(%ws_url, "Connected to WebSocket");

    // Subscribe to new block headers
    let mut subscription = provider.subscribe_blocks().await?;

    info!(%ws_url, "Subscribed to newHeads");

    let make_update = |height| PeerHeightUpdate {
        peer_id,
        http_url: http_url.clone(),
        height,
    };

    match provider.get_block_number().await {
        Ok(block_number) => {
            let height = Height::new(block_number);
            debug!(%ws_url, %height, "Initial block height");
            let _ = update_tx.send(make_update(height)).await;
        }
        Err(error) => {
            warn!(%ws_url, %error, "Failed to get initial block number");
        }
    }

    // Stream incoming block headers until the subscription breaks or stalls.
    loop {
        let header = recv_next_header(ws_url, stale_timeout, subscription.recv()).await?;

        let height = Height::new(header.number);

        debug!(%ws_url, %height, hash = %header.hash, "Received new block header via WebSocket");

        if update_tx.send(make_update(height)).await.is_err() {
            // Channel closed, exit
            return Ok(());
        }
    }
}

/// Await one header from a `subscription.recv()` future with a stale timeout.
///
/// On timeout, increments `arc_rpc_sync_ws_stale_total`, logs a warning, and returns an
/// error so the caller can drop the connection and let backoff reconnect.
async fn recv_next_header<R, E, F>(
    ws_url: &Url,
    stale_timeout: Duration,
    recv: F,
) -> eyre::Result<R>
where
    F: std::future::Future<Output = Result<R, E>>,
    E: std::fmt::Display,
{
    match tokio::time::timeout(stale_timeout, recv).await {
        Ok(Ok(header)) => Ok(header),
        Ok(Err(e)) => Err(eyre::eyre!("WebSocket subscription error: {e}")),
        Err(_) => {
            warn!(
                %ws_url,
                ?stale_timeout,
                "newHeads subscription stalled, reconnecting"
            );
            crate::metrics::ws_subscription::record_stale();
            Err(eyre::eyre!(
                "newHeads subscription stalled: no header within {stale_timeout:?}"
            ))
        }
    }
}

/// Manager for all WebSocket subscriptions
pub struct WsSubscriptionManager {
    /// Sender for spawning new subscriptions
    update_tx: mpsc::Sender<PeerHeightUpdate>,
    /// Cancellation token for all subscriptions
    cancel_token: CancellationToken,
    /// Staleness bound applied to each `newHeads` subscription
    stale_timeout: Duration,
}

impl WsSubscriptionManager {
    /// Create a new subscription manager
    pub fn new(
        stale_timeout: Duration,
        cancel_token: CancellationToken,
    ) -> (Self, mpsc::Receiver<PeerHeightUpdate>) {
        let (update_tx, update_rx) = mpsc::channel(256);

        let this = Self {
            update_tx,
            cancel_token,
            stale_timeout,
        };

        (this, update_rx)
    }

    /// Start WebSocket subscriptions for all peers
    pub fn start_subscriptions(&self, peers: &crate::rpc_sync::peers::RpcPeers) {
        for peer in peers.all() {
            if let Some(ws_url) = &peer.ws_url {
                spawn_ws_subscription(
                    peer.http_url.clone(),
                    ws_url.clone(),
                    peer.peer_id,
                    self.update_tx.clone(),
                    self.cancel_token.clone(),
                    self.stale_timeout,
                );
            } else {
                warn!(
                    http_url = %peer.http_url,
                    "No WebSocket URL for peer, skipping subscription"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future;

    fn test_url() -> Url {
        "wss://example.invalid/ws".parse().unwrap()
    }

    /// A `subscription.recv()` future that never resolves must not block
    /// `recv_next_header` indefinitely — the timeout must fire and return an error,
    /// triggering the outer backoff loop to reconnect.
    #[tokio::test(start_paused = true)]
    async fn recv_next_header_errors_when_recv_never_resolves() {
        let url = test_url();
        let timeout = Duration::from_secs(30);
        let never: future::Pending<Result<u64, std::io::Error>> = future::pending();

        let mut fut = std::pin::pin!(recv_next_header(&url, timeout, never));

        // Advance time just before the timeout — the future must still be pending.
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(
            futures::poll!(&mut fut).is_pending(),
            "future must still be pending before the timeout elapses"
        );

        // Advance past the timeout — the future must now resolve with a stale error.
        tokio::time::advance(Duration::from_secs(2)).await;
        let err = fut
            .await
            .expect_err("stalled subscription must produce an error");

        let msg = format!("{err:#}");
        assert!(msg.contains("stalled"), "expected stall error, got: {msg}");
    }

    #[tokio::test(start_paused = true)]
    async fn recv_next_header_forwards_recv_error() {
        let url = test_url();
        let err_fut = async { Err::<u64, _>(std::io::Error::other("boom")) };

        let err = recv_next_header(&url, Duration::from_secs(30), err_fut)
            .await
            .expect_err("recv error must propagate");

        let msg = format!("{err:#}");
        assert!(
            msg.contains("WebSocket subscription error") && msg.contains("boom"),
            "expected wrapped recv error, got: {msg}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn recv_next_header_returns_header_when_ready() {
        let url = test_url();
        let ok_fut = async { Ok::<u64, std::io::Error>(7) };

        let value = recv_next_header(&url, Duration::from_secs(30), ok_fut)
            .await
            .expect("ready future must return Ok");
        assert_eq!(value, 7);
    }
}
