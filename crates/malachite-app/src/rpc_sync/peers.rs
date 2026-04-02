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

//! RPC Peers
//!
//! Maps RPC endpoints to malachite PeerIds. Each RPC endpoint is treated
//! as a "peer" that malachite's sync actor can request blocks from.
//! Peer heights are updated via WebSocket subscriptions (newHeads).

use std::collections::HashMap;

use multihash::Multihash;
use sha3::{Digest, Keccak256};
use tracing::{debug, info};
use url::Url;

use arc_consensus_types::rpc_sync::SyncEndpointUrl;
use arc_consensus_types::Height;
use malachitebft_peer::PeerId;

/// RPC peer information
#[derive(Debug, Clone)]
pub struct RpcPeer {
    /// HTTP RPC endpoint URL
    pub http_url: Url,
    /// WebSocket RPC endpoint URL (derived from HTTP URL)
    pub ws_url: Option<Url>,
    /// Derived PeerId for this endpoint
    pub peer_id: PeerId,
    /// Current tip height (latest known block)
    pub tip_height: Height,
    /// Minimum height in history (RPC endpoints typically have full history)
    pub history_min_height: Height,
    /// Whether this peer is currently connected/reachable
    pub connected: bool,
}

impl RpcPeer {
    /// Create a new RPC peer from an endpoint URL
    pub fn new(url: SyncEndpointUrl) -> Self {
        let peer_id = url_to_peer_id(url.http());

        Self {
            http_url: url.http().clone(),
            ws_url: Some(url.websocket()),
            peer_id,
            tip_height: Height::new(0),
            history_min_height: Height::new(0), // Assumes RPC endpoint is an archive node with full history
            connected: false,
        }
    }

    /// Update the tip height for this peer
    pub fn update_tip_height(&mut self, height: Height) {
        if height > self.tip_height {
            debug!(
                peer_id = %self.peer_id,
                old_height = %self.tip_height,
                new_height = %height,
                "RPC peer tip height updated"
            );
            self.tip_height = height;
        }
    }

    /// Convert to malachite Status
    pub fn to_status(&self) -> malachitebft_sync::Status<arc_consensus_types::ArcContext> {
        malachitebft_sync::Status {
            peer_id: self.peer_id,
            tip_height: self.tip_height,
            history_min_height: self.history_min_height,
        }
    }
}

/// Collection of RPC peers
#[derive(Debug, Clone)]
pub struct RpcPeers {
    /// Map from PeerId to RpcPeer
    peers: HashMap<PeerId, RpcPeer>,
    /// List of endpoints in order (for iteration)
    endpoints: Vec<SyncEndpointUrl>,
}

impl RpcPeers {
    /// Create a new RpcPeers collection from a list of endpoint URLs
    pub fn new(endpoints: Vec<SyncEndpointUrl>) -> Self {
        let mut peers = HashMap::new();

        for url in &endpoints {
            let peer = RpcPeer::new(url.clone());
            info!(
                peer_id = %peer.peer_id,
                http_url = %peer.http_url,
                ws_url = ?peer.ws_url,
                "Added RPC peer"
            );
            peers.insert(peer.peer_id, peer);
        }

        Self { peers, endpoints }
    }

    /// Get a peer by PeerId
    pub fn get(&self, peer_id: &PeerId) -> Option<&RpcPeer> {
        self.peers.get(peer_id)
    }

    /// Get a mutable peer by PeerId
    pub fn get_mut(&mut self, peer_id: &PeerId) -> Option<&mut RpcPeer> {
        self.peers.get_mut(peer_id)
    }

    /// Get a peer by HTTP URL
    pub fn get_by_url(&self, url: &Url) -> Option<&RpcPeer> {
        let peer_id = url_to_peer_id(url);
        self.peers.get(&peer_id)
    }

    /// Update tip height for a peer identified by URL
    pub fn update_tip_height_by_url(&mut self, url: &Url, height: Height) {
        let peer_id = url_to_peer_id(url);
        if let Some(peer) = self.peers.get_mut(&peer_id) {
            peer.update_tip_height(height);
        }
    }

    /// Update tip height for a peer
    pub fn update_tip_height(&mut self, peer_id: &PeerId, height: Height) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.update_tip_height(height);
        }
    }

    /// Mark a peer as connected
    pub fn set_connected(&mut self, peer_id: &PeerId, connected: bool) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.connected = connected;
        }
    }

    /// Get all peers
    pub fn all(&self) -> impl Iterator<Item = &RpcPeer> {
        self.peers.values()
    }

    /// Get all connected peers
    pub fn connected(&self) -> impl Iterator<Item = &RpcPeer> {
        self.peers.values().filter(|p| p.connected)
    }

    /// Get endpoint URLs in order
    pub fn endpoints(&self) -> &[SyncEndpointUrl] {
        &self.endpoints
    }

    /// Get the PeerId for an endpoint URL
    pub fn peer_id_for_url(&self, url: &Url) -> PeerId {
        url_to_peer_id(url)
    }

    /// Get the HTTP URL for a PeerId
    pub fn url_for_peer(&self, peer_id: &PeerId) -> Option<&Url> {
        self.peers.get(peer_id).map(|p| &p.http_url)
    }

    /// Get the highest tip height among all connected peers
    pub fn max_tip_height(&self) -> Height {
        self.peers
            .values()
            .filter(|p| p.connected)
            .map(|p| p.tip_height)
            .max()
            .unwrap_or(Height::new(0))
    }

    /// Convert to malachite peer statuses for the sync state machine
    pub fn to_statuses(&self) -> Vec<malachitebft_sync::Status<arc_consensus_types::ArcContext>> {
        self.peers
            .values()
            .filter(|p| p.connected)
            .map(|p| p.to_status())
            .collect()
    }
}

/// Derive a PeerId from an endpoint URL
///
/// Uses Keccak256 hash of the URL to create a deterministic PeerId.
/// This ensures the same URL always maps to the same PeerId.
fn url_to_peer_id(url: &Url) -> PeerId {
    // Hash the URL to get 32 bytes
    let mut hasher = Keccak256::new();
    hasher.update(url.as_str().as_bytes());
    let hash = hasher.finalize();

    // Create PeerId using identity multihash format:
    // [0x00, 0x20] = identity hash code + 32 byte length
    // followed by 32 bytes of hash
    let multihash = Multihash::wrap(0x00, &hash[..32]).expect("Valid multihash from URL hash");
    PeerId::from_multihash(multihash).expect("Valid peer ID from multihash")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_to_peer_id_deterministic() {
        let url = Url::parse("http://localhost:8545").unwrap();
        let peer_id1 = url_to_peer_id(&url);
        let peer_id2 = url_to_peer_id(&url);
        assert_eq!(peer_id1, peer_id2);
    }

    #[test]
    fn test_url_to_peer_id_different_urls() {
        let peer_id1 = url_to_peer_id(&"http://localhost:8545".parse().unwrap());
        let peer_id2 = url_to_peer_id(&"http://localhost:8546".parse().unwrap());
        assert_ne!(peer_id1, peer_id2);
    }

    #[test]
    fn test_rpc_peers_creation() {
        let endpoints = vec![
            "http://localhost:8545".parse().unwrap(),
            "http://localhost:8546".parse().unwrap(),
        ];
        let peers = RpcPeers::new(endpoints.clone());

        assert_eq!(peers.endpoints().len(), 2);
        assert!(peers
            .get_by_url(&"http://localhost:8545".parse().unwrap())
            .is_some());
        assert!(peers
            .get_by_url(&"http://localhost:8546".parse().unwrap())
            .is_some());
    }
}
