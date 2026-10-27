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

//! RPC Network Actor for RPC Sync Mode
//!
//! This module provides a Network actor that integrates with malachite's default
//! sync actor. It:
//! 1. Manages WebSocket subscriptions to RPC endpoints for real-time block notifications
//! 2. Sends `NetworkEvent::Status` to malachite when peer heights update
//! 3. Handles `OutgoingRequest` from malachite's sync and fetches blocks via RPC
//! 4. Handles `CancelRequest` from malachite's sync and aborts the matching in-flight fetch
//! 5. Returns blocks via `NetworkEvent::SyncResponse`
//!
//! ## Architecture
//!
//! ```text
//!  WebSocket (newHeads)
//!         │
//!         ▼
//!  ┌─────────────────┐   NetworkEvent::Status    ┌─────────────────┐
//!  │  Network Actor  │ ────────────────────────▶ │   Sync Actor    │
//!  │                 │ ◀──────────────────────── │                 │
//!  │                 │     OutgoingRequest       │                 │
//!  │                 │ ◀──────────────────────── │                 │
//!  └────────┬────────┘     CancelRequest         └────────▲────────┘
//!           │           (aborts in-flight                 │
//!           │            fetch, no reply)                 │
//!           │                                             │
//!           │ (fetch blocks via RPC,                      │
//!           │  per OutgoingRequest)                       │
//!           │                                             │
//!           └──────── NetworkEvent::SyncResponse ─────────┤
//!                                                         │
//!                                                         ▼
//!                                                ┌─────────────────┐
//!                                                │    Consensus    │
//!                                                └─────────────────┘
//! ```

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use arc_consensus_types::rpc_sync::SyncEndpointUrl;
use eyre::{eyre, Context};
use parking_lot::Mutex;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use malachitebft_app_channel::app::engine::network::{
    Msg as NetworkActorMsg, NetworkEvent, NetworkRef, Status as NetworkStatus, Subscriber,
};
use malachitebft_app_channel::app::engine::util::output_port::OutputPort;
use malachitebft_app_channel::NetworkMsg;
use malachitebft_core_types::{ExtendedCommitCertificate, VoteExtensions};
use malachitebft_peer::PeerId;
use malachitebft_sync::{OutboundRequestId, RawDecidedValue, Response, ValueResponse};

use arc_consensus_types::{ArcContext, Height};

use crate::rpc_sync::client::RpcSyncClient;
use crate::rpc_sync::peers::RpcPeers;
use crate::rpc_sync::ws_subscription::WsSubscriptionManager;

/// State for the RPC Network actor
pub struct RpcNetworkState {
    /// RPC peers for peer-to-URL lookups and WebSocket subscriptions
    peers: RpcPeers,
    /// Cancellation token for background tasks
    cancel_token: CancellationToken,
    /// Request-local cancellation tokens for in-flight RPC fetches
    pending_requests: PendingRequests,
    /// Request ID counter
    next_request_id: u64,
    /// Whether WebSocket forwarding has been started
    ws_started: bool,
    /// Staleness bound applied to each `newHeads` WebSocket subscription
    ws_stale_timeout: Duration,
}

type PendingRequests = Arc<Mutex<BTreeMap<OutboundRequestId, CancellationToken>>>;

/// Arguments for spawning the RPC Network actor
pub struct RpcNetworkArgs {
    /// RPC endpoint URLs
    pub endpoints: Vec<SyncEndpointUrl>,
    /// Staleness bound applied to each `newHeads` WebSocket subscription
    pub ws_stale_timeout: Duration,
}

/// RPC Network Actor
///
/// Manages WebSocket subscriptions for peer status and handles fetch requests.
pub struct RpcNetworkActor {
    /// Shared output port for sending events to Consensus
    output_port: Arc<OutputPort<NetworkEvent<ArcContext>>>,
    /// RPC client for fetching blocks
    rpc_client: RpcSyncClient,
}

impl RpcNetworkActor {
    pub fn new(
        output_port: Arc<OutputPort<NetworkEvent<ArcContext>>>,
        rpc_client: RpcSyncClient,
    ) -> Self {
        Self {
            output_port,
            rpc_client,
        }
    }
}

/// Spawn the RPC Network actor
///
/// Returns:
/// - `NetworkRef<ArcContext>`: Reference to the actor
/// - `mpsc::Sender<NetworkMsg<ArcContext>>`: Channel for the app to send network messages
pub async fn spawn_rpc_network_actor(
    endpoints: Vec<SyncEndpointUrl>,
    ws_stale_timeout: Duration,
) -> eyre::Result<(NetworkRef<ArcContext>, mpsc::Sender<NetworkMsg<ArcContext>>)> {
    // Create RPC client and shared output port
    let rpc_client = RpcSyncClient::new();
    let output_port = Arc::new(OutputPort::with_capacity(256));

    let actor = RpcNetworkActor::new(output_port.clone(), rpc_client);
    let args = RpcNetworkArgs {
        endpoints,
        ws_stale_timeout,
    };

    let (actor_ref, _handle) = Actor::spawn(Some("rpc-network".to_string()), actor, args)
        .await
        .map_err(|e| eyre::eyre!("Failed to spawn RPC network actor: {e}"))?;

    // Create the network message channel (app → network)
    // In this mode, the node does not participate in consensus,
    // and therefore never needs to send a NetworkMsg to the network actor.
    // However, we still need to return a sender for the NetworkRef,
    // so we create a dummy channel that just logs a warning if any messages are sent to it.
    let (network_tx, mut network_rx) = mpsc::channel(16);

    tokio::spawn(async move {
        // Show a warning if any messages are sent to the network actor,
        // since this should never happen in RPC sync mode
        while let Some(msg) = network_rx.recv().await {
            warn!(?msg, "Received unexpected NetworkMsg in RPC sync mode");
        }
    });

    Ok((actor_ref, network_tx))
}

/// Fetch a range of blocks from an RPC endpoint
async fn fetch_range(
    rpc_client: &RpcSyncClient,
    endpoint: &url::Url,
    range: &RangeInclusive<Height>,
    cancel_token: &CancellationToken,
) -> eyre::Result<Vec<RawDecidedValue<ArcContext>>> {
    // Heights are validated in fetch_blocks_batch
    let blocks = cancel_token
        .run_until_cancelled(rpc_client.fetch_blocks_batch(endpoint, range))
        .await
        .ok_or_else(|| eyre!("Fetch cancelled"))?
        .wrap_err("Fetch failed")?
        .into_iter()
        .map(|block| RawDecidedValue {
            value_bytes: block.value_bytes,
            certificate: ExtendedCommitCertificate::from_commit_certificate_and_extensions(
                block.certificate,
                VoteExtensions::default(),
            ),
        })
        .collect();

    Ok(blocks)
}

#[ractor::async_trait]
impl Actor for RpcNetworkActor {
    type Msg = NetworkActorMsg<ArcContext>;
    type State = RpcNetworkState;
    type Arguments = RpcNetworkArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!(endpoints = ?args.endpoints, "RPC Network actor starting");

        // Create RPC peers from endpoints (for WebSocket subscriptions)
        // WebSocket subscriptions will be started lazily when first subscriber registers
        let peers = RpcPeers::new(args.endpoints);

        // Create cancellation token
        let cancel_token = CancellationToken::new();

        Ok(RpcNetworkState {
            peers,
            cancel_token,
            pending_requests: Arc::new(Mutex::new(BTreeMap::new())),
            next_request_id: 1,
            ws_started: false,
            ws_stale_timeout: args.ws_stale_timeout,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            NetworkActorMsg::Subscribe(subscriber) => {
                on_subscribe(state, subscriber, &self.output_port);
            }

            NetworkActorMsg::OutgoingRequest(peer_id, request, reply) => {
                on_outgoing_request(
                    state,
                    peer_id,
                    request,
                    reply,
                    &self.output_port,
                    &self.rpc_client,
                );
            }

            NetworkActorMsg::CancelRequest(request_id) => {
                on_cancel_request(state, request_id);
            }

            // These messages are not used in RPC sync mode
            NetworkActorMsg::PublishConsensusMsg(_) => {
                trace!("RPC sync mode: ignoring PublishConsensusMsg");
            }
            NetworkActorMsg::PublishLivenessMsg(_) => {
                trace!("RPC sync mode: ignoring PublishLivenessMsg");
            }
            NetworkActorMsg::PublishProposalPart(_) => {
                trace!("RPC sync mode: ignoring PublishProposalPart");
            }
            NetworkActorMsg::BroadcastStatus(_) => {
                trace!("RPC sync mode: ignoring BroadcastStatus");
            }
            NetworkActorMsg::OutgoingResponse(_, _) => {
                trace!("RPC sync mode: ignoring OutgoingResponse");
            }
            NetworkActorMsg::UpdateValidatorSet(_) => {
                trace!("RPC sync mode: ignoring UpdateValidatorSet");
            }
            NetworkActorMsg::DumpState(reply) => {
                let _ = reply.send(Default::default());
            }
            _ => {
                trace!("RPC sync mode: ignoring unhandled network message");
            }
        }

        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("RPC Network actor stopping");
        state.cancel_token.cancel();
        Ok(())
    }
}

// ----------------------------------------------------------------------------
// Message handlers
// ----------------------------------------------------------------------------

/// Handle a Subscribe message from consensus or sync
fn on_subscribe(
    state: &mut RpcNetworkState,
    subscriber: Box<dyn Subscriber<NetworkEvent<ArcContext>>>,
    output_port: &Arc<OutputPort<NetworkEvent<ArcContext>>>,
) {
    info!("Subscriber registered to RPC network events");
    subscriber.subscribe_to_port(output_port);

    // Start WebSocket subscriptions on first subscribe
    // This ensures sync actor is subscribed before we send Status events
    if state.ws_started {
        return;
    }

    state.ws_started = true;

    info!("Starting WebSocket subscriptions (first subscriber registered)");

    // Create and start WebSocket subscription manager
    let (ws_manager, mut ws_update_rx) =
        WsSubscriptionManager::new(state.ws_stale_timeout, state.cancel_token.clone());

    // Clone peers for the async task (we keep the original for URL lookups)
    let peers = state.peers.clone();

    // Start subscriptions in a separate task to not block
    let port = output_port.clone();
    let cancel_token = state.cancel_token.clone();

    tokio::spawn(async move {
        // Start WebSocket subscriptions
        ws_manager.start_subscriptions(&peers);

        // Forward updates to consensus via NetworkEvent::Status
        loop {
            tokio::select! {
                Some(update) = ws_update_rx.recv() => {
                    // Send Status event to subscribers (sync and consensus)
                    let status = NetworkStatus::new(update.height, Height::new(0));
                    let event = NetworkEvent::Status(update.peer_id, status);
                    port.send(event);
                }
                _ = cancel_token.cancelled() => {
                    break;
                }
            }
        }
    });

    // Emit NetworkEvent::Listening to trigger Consensus to send ConsensusReady
    let fake_address = "/ip4/127.0.0.1/tcp/0".parse().expect("valid multiaddr");
    info!("Emitting NetworkEvent::Listening to trigger ConsensusReady");
    output_port.send(NetworkEvent::Listening(fake_address));
}

/// Handle an OutgoingRequest (sync request) from malachite's sync actor
fn on_outgoing_request(
    state: &mut RpcNetworkState,
    peer_id: PeerId,
    request: malachitebft_sync::Request<ArcContext>,
    reply: ractor::RpcReplyPort<OutboundRequestId>,
    output_port: &Arc<OutputPort<NetworkEvent<ArcContext>>>,
    rpc_client: &RpcSyncClient,
) {
    debug!(
        ?peer_id,
        ?request,
        "Received sync request from malachite sync actor"
    );

    let request_id = OutboundRequestId::new(state.next_request_id);
    // Request IDs are sequential and won't reach u64::MAX in practice
    #[allow(clippy::arithmetic_side_effects)]
    {
        state.next_request_id += 1;
    }

    // Send request ID back immediately
    if let Err(e) = reply.send(request_id.clone()) {
        error!("Failed to send request ID reply: {e:?}");
        return;
    }

    // Look up the endpoint URL for this peer
    // Malachite's sync actor selects which peer to request from based on peer scores
    let endpoint = match state.peers.url_for_peer(&peer_id) {
        Some(url) => url.clone(),
        None => {
            warn!(%peer_id, "Unknown peer_id in sync request, no endpoint found");
            let event = NetworkEvent::SyncResponse(request_id, peer_id, None);
            output_port.send(event);
            return;
        }
    };

    // Handle the value request
    let malachitebft_sync::Request::ValueRequest(value_req) = request;
    let range = value_req.range.clone();
    let rpc_client = rpc_client.clone();
    let output_port = output_port.clone();
    let cancel_token = state.cancel_token.child_token();
    let pending_requests = state.pending_requests.clone();

    pending_requests
        .lock()
        .insert(request_id.clone(), cancel_token.clone());

    tokio::spawn(async move {
        let result = fetch_range(&rpc_client, &endpoint, &range, &cancel_token).await;

        remove_pending_request(&pending_requests, &request_id);

        // A racing CancelRequest can still slip through here (if it observes
        // the map entry already removed by the line above and so does not
        // fire the token). The malachite sync actor drops late responses for
        // evicted request IDs, so this is safe.
        if cancel_token.is_cancelled() {
            debug!(%request_id, "RPC sync fetch cancelled; dropping response");
            return;
        }

        match result {
            Ok(values) if !values.is_empty() => {
                let start_height = values.first().expect("non-empty").certificate.height;
                info!(
                    %request_id,
                    %start_height,
                    count = values.len(),
                    "✅ Fetch completed, sending blocks to consensus"
                );

                let response = ValueResponse {
                    start_height,
                    values,
                };

                let event = NetworkEvent::SyncResponse(
                    request_id,
                    peer_id,
                    Some(Response::ValueResponse(response)),
                );
                output_port.send(event);
            }
            Ok(_) => {
                warn!(%request_id, "Fetch returned no values");
                let event = NetworkEvent::SyncResponse(request_id, peer_id, None);
                output_port.send(event);
            }
            Err(e) => {
                warn!(%request_id, error = format!("{e:#}"), "Fetch failed");
                let event = NetworkEvent::SyncResponse(request_id, peer_id, None);
                output_port.send(event);
            }
        }
    });
}

/// Cancel an in-flight RPC sync request.
fn on_cancel_request(state: &mut RpcNetworkState, request_id: OutboundRequestId) {
    let cancel_token = state.pending_requests.lock().remove(&request_id);

    match cancel_token {
        Some(cancel_token) => {
            cancel_token.cancel();
            debug!(%request_id, "Cancelled in-flight RPC sync request");
        }
        None => {
            debug!(%request_id, "Cancel requested for unknown RPC sync request");
        }
    }
}

fn remove_pending_request(pending_requests: &PendingRequests, request_id: &OutboundRequestId) {
    pending_requests.lock().remove(request_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> RpcNetworkState {
        RpcNetworkState {
            peers: RpcPeers::new(Vec::new()),
            cancel_token: CancellationToken::new(),
            pending_requests: Arc::new(Mutex::new(BTreeMap::new())),
            next_request_id: 1,
            ws_started: false,
            ws_stale_timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn cancel_request_cancels_and_removes_pending_request() {
        let mut state = test_state();
        let request_id = OutboundRequestId::new("req1");
        let cancel_token = CancellationToken::new();

        state
            .pending_requests
            .lock()
            .insert(request_id.clone(), cancel_token.clone());

        on_cancel_request(&mut state, request_id.clone());

        assert!(cancel_token.is_cancelled());
        assert!(!state.pending_requests.lock().contains_key(&request_id));
    }

    #[test]
    fn cancel_unknown_request_is_noop() {
        let mut state = test_state();
        let request_id = OutboundRequestId::new("missing");

        on_cancel_request(&mut state, request_id);

        assert!(state.pending_requests.lock().is_empty());
    }
}
