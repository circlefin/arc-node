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

//! RPC Sync Mode
//!
//! This module provides an alternative to P2P-based block synchronization.
//! Instead of participating in the gossip network, nodes in RPC sync mode
//! fetch blocks directly from trusted RPC endpoints.
//!
//! ## Architecture
//!
//! - **RPC Network Actor**: Manages WebSocket subscriptions to RPC endpoints,
//!   sends `NetworkEvent::Status` when peer heights update, and handles
//!   `OutgoingRequest` from malachite's sync to fetch blocks via RPC.
//!
//! - **Malachite's Default Sync**: Receives Status events, tracks peers,
//!   and makes parallel batch requests based on configuration.
//!
//! - **RPC Peers**: Maps RPC endpoints to malachite PeerIds.
//!
//! ## Flow
//!
//! 1. Engine starts, consensus and sync subscribe to Network
//! 2. WebSocket subscriptions connect and receive block notifications
//! 3. Network sends `NetworkEvent::Status` to subscribers
//! 4. Malachite's sync sees peers ahead and sends `OutgoingRequest`
//! 5. Network fetches blocks via RPC, sends `NetworkEvent::SyncResponse`
//! 6. Consensus validates and decides
//! 7. Repeat
//!
//! ## Usage
//!
//! Enable with `--follow` and provide `--follow.endpoint`.

pub mod client;
pub mod network;
pub mod peers;
pub mod ws_subscription;

pub use arc_consensus_types::rpc_sync::SyncEndpointUrl;
pub use client::{RpcSyncClient, SyncedBlock};
pub use network::{spawn_rpc_network_actor, RpcNetworkActor};
pub use peers::{RpcPeer, RpcPeers};
pub use ws_subscription::{PeerHeightUpdate, WsSubscriptionManager};
