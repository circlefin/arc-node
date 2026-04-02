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

/// Spawn a WebSocket subscription task for a single endpoint
///
/// This task:
/// 1. Connects to the WebSocket endpoint
/// 2. Subscribes to newHeads
/// 3. Sends height updates through the channel
/// 4. Reconnects on disconnection
pub async fn spawn_ws_subscription(
    http_url: Url,
    ws_url: Url,
    peer_id: PeerId,
    update_tx: mpsc::Sender<PeerHeightUpdate>,
    cancel_token: CancellationToken,
) {
    tokio::spawn(run_ws_subscription(
        http_url,
        ws_url,
        peer_id,
        update_tx,
        cancel_token,
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
) {
    use backon::RetryableWithContext;

    struct Context {
        ws_url: Url,
        http_url: Url,
        peer_id: PeerId,
        update_tx: mpsc::Sender<PeerHeightUpdate>,
    }

    async fn task(ctx: Context) -> (Context, eyre::Result<()>) {
        info!(ws_url = %ctx.ws_url, "Connecting to WebSocket endpoint");
        let result =
            connect_and_subscribe(&ctx.ws_url, &ctx.http_url, ctx.peer_id, &ctx.update_tx).await;
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
        })
        .notify(|error, delay| {
            retry_attempts += 1;

            warn!(
                %ws_url, %error, attempt = retry_attempts,
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
            error!(%ws_url, %error, "WebSocket subscription failed with unrecoverable error");
        }
        None => {
            // The WebSocket subscription task was cancelled
            info!(%ws_url, "WebSocket subscription cancelled");
        }
    }
}

/// Connect to a WebSocket endpoint, subscribe to newHeads,
/// and forward height updates until the connection is lost or the channel closes.
async fn connect_and_subscribe(
    ws_url: &Url,
    http_url: &Url,
    peer_id: PeerId,
    update_tx: &mpsc::Sender<PeerHeightUpdate>,
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

    // Stream incoming block headers until the subscription breaks
    loop {
        let header = subscription
            .recv()
            .await
            .map_err(|e| eyre::eyre!("WebSocket subscription error: {e}"))?;

        let height = Height::new(header.number);

        debug!(%ws_url, %height, hash = %header.hash, "Received new block header via WebSocket");

        if update_tx.send(make_update(height)).await.is_err() {
            // Channel closed, exit
            return Ok(());
        }
    }
}

/// Manager for all WebSocket subscriptions
pub struct WsSubscriptionManager {
    /// Sender for spawning new subscriptions
    update_tx: mpsc::Sender<PeerHeightUpdate>,
    /// Cancellation token for all subscriptions
    cancel_token: CancellationToken,
}

impl WsSubscriptionManager {
    /// Create a new subscription manager
    pub fn new() -> (Self, mpsc::Receiver<PeerHeightUpdate>) {
        let (update_tx, update_rx) = mpsc::channel(256);

        let this = Self {
            update_tx,
            cancel_token: CancellationToken::new(),
        };

        (this, update_rx)
    }

    /// Set a custom cancellation token
    pub fn set_cancel_token(&mut self, token: CancellationToken) {
        self.cancel_token = token;
    }

    /// Start WebSocket subscriptions for all peers
    pub async fn start_subscriptions(&self, peers: &crate::rpc_sync::peers::RpcPeers) {
        for peer in peers.all() {
            if let Some(ws_url) = &peer.ws_url {
                spawn_ws_subscription(
                    peer.http_url.clone(),
                    ws_url.clone(),
                    peer.peer_id,
                    self.update_tx.clone(),
                    self.cancel_token.clone(),
                )
                .await;
            } else {
                warn!(
                    http_url = %peer.http_url,
                    "No WebSocket URL for peer, skipping subscription"
                );
            }
        }
    }

    /// Stop all subscriptions
    pub fn stop(&self) {
        self.cancel_token.cancel();
    }

    /// Get the cancellation token
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }
}
