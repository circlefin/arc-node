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

//! The Application (or Node) definition. The Node trait implements the Consensus context and the
//! cryptographic library used for signing.

use std::net::SocketAddr;

use arc_consensus_types::rpc_sync::SyncEndpointUrl;
use backon::{BackoffBuilder, Retryable};
use tracing::warn;
use url::Url;

use malachitebft_app_channel::app::consensus::Multiaddr;

use arc_consensus_db::DbUpgrade;
use arc_consensus_types::Address;

use crate::hardcoded_config::GossipSubOverrides;
use arc_eth_engine::{engine::Engine, INITIAL_RETRY_DELAY};

pub enum EngineConfig<'a> {
    Ipc(EthIpcConfig<'a>),
    Rpc(EthRpcConfig<'a>),
}

impl<'a> EngineConfig<'a> {
    pub async fn connect(self) -> eyre::Result<Engine> {
        // Retry indefinitely with a constant delay of `INITIAL_RETRY_DELAY`
        // seconds
        let retry_policy = backon::ConstantBuilder::new()
            .with_delay(INITIAL_RETRY_DELAY)
            .without_max_times()
            .build();
        match self {
            EngineConfig::Ipc(EthIpcConfig {
                eth_socket,
                execution_socket,
            }) => (|| Engine::new_ipc(execution_socket, eth_socket))
                .retry(retry_policy)
                .notify(|e, dur| {
                    warn!("Failed to connect to Ethereum node via IPC: {e}, retrying in {dur:?}...")
                })
                .await,

            EngineConfig::Rpc(EthRpcConfig {
                eth_rpc_endpoint,
                execution_endpoint,
                execution_ws_endpoint,
                execution_jwt,
            }) => {
                let ws_endpoint = execution_ws_endpoint;
                (move || {
                    Engine::new_rpc(
                        execution_endpoint.clone(),
                        eth_rpc_endpoint.clone(),
                        ws_endpoint.clone(),
                        execution_jwt,
                    )
                })
                .retry(retry_policy)
                .notify(|e, dur| {
                    warn!("Failed to connect to Ethereum node via RPC: {e}, retrying in {dur:?}...")
                })
                .await
            }
        }
    }
}

pub struct EthIpcConfig<'a> {
    pub eth_socket: &'a str,
    pub execution_socket: &'a str,
}

pub struct EthRpcConfig<'a> {
    pub eth_rpc_endpoint: &'a Url,
    pub execution_endpoint: &'a Url,
    pub execution_ws_endpoint: Option<Url>,
    pub execution_jwt: &'a str,
}

/// Configuration parameters for the start.
#[derive(Clone, Default)]
pub struct StartConfig {
    /// The persistent peers to connect to on startup
    pub persistent_peers: Vec<Multiaddr>,

    /// Only allow connections to/from persistent peers
    pub persistent_peers_only: bool,

    /// GossipSub overrides from CLI flags
    pub gossipsub_overrides: GossipSubOverrides,

    /// The Ethereum IPC socket
    pub eth_socket: Option<String>,
    /// The execution socket
    pub execution_socket: Option<String>,

    /// The Ethereum RPC endpoint
    pub eth_rpc_endpoint: Option<Url>,
    /// The execution endpoint
    pub execution_endpoint: Option<Url>,
    /// The execution WebSocket endpoint
    pub execution_ws_endpoint: Option<Url>,
    /// The execution JWT
    pub execution_jwt: Option<String>,
    /// The bind address for the pprof server
    pub pprof_bind_address: Option<SocketAddr>,
    /// The address to receive the fees and rewards from the execution layer
    pub suggested_fee_recipient: Option<Address>,
    /// Skip database schema upgrade on startup
    pub skip_db_upgrade: bool,

    /// Enable RPC sync mode, a.k.a. follow (fetch blocks via HTTP RPC instead of P2P)
    pub rpc_sync_enabled: bool,
    /// RPC endpoints to fetch blocks from (only used in RPC sync mode)
    pub rpc_sync_endpoints: Vec<SyncEndpointUrl>,
}

impl StartConfig {
    /// Check if RPC sync mode is enabled
    pub fn is_rpc_sync_mode(&self) -> bool {
        if self.rpc_sync_enabled && self.rpc_sync_endpoints.is_empty() {
            warn!("RPC sync mode is enabled but no RPC sync endpoints are configured. Falling back to P2P sync mode.");
        }

        self.rpc_sync_enabled && !self.rpc_sync_endpoints.is_empty()
    }

    pub fn engine_config(&'_ self) -> Option<EngineConfig<'_>> {
        if let (Some(eth_socket), Some(execution_socket)) =
            (self.eth_socket.as_ref(), self.execution_socket.as_ref())
        {
            Some(EngineConfig::Ipc(EthIpcConfig {
                eth_socket,
                execution_socket,
            }))
        } else if let (Some(eth_rpc_endpoint), Some(execution_endpoint), Some(execution_jwt)) = (
            self.eth_rpc_endpoint.as_ref(),
            self.execution_endpoint.as_ref(),
            self.execution_jwt.as_ref(),
        ) {
            let ws_endpoint = self
                .execution_ws_endpoint
                .clone()
                .or_else(|| derive_ws_url(eth_rpc_endpoint));

            Some(EngineConfig::Rpc(EthRpcConfig {
                eth_rpc_endpoint,
                execution_endpoint,
                execution_ws_endpoint: ws_endpoint,
                execution_jwt,
            }))
        } else {
            None
        }
    }

    pub fn db_upgrade(&self) -> DbUpgrade {
        if self.skip_db_upgrade {
            DbUpgrade::Skip
        } else {
            DbUpgrade::Perform
        }
    }
}

/// Derive a WebSocket URL from an HTTP RPC URL using the reth convention:
/// `http(s)://host:port` → `ws(s)://host:(port+1)`.
///
/// Same convention used by `--follow.endpoint` (see [`SyncEndpointUrl::websocket`]).
/// Returns `None` if the URL has no explicit port or an unsupported scheme.
fn derive_ws_url(http_url: &Url) -> Option<Url> {
    let mut ws_url = http_url.clone();
    match http_url.scheme() {
        "http" => ws_url.set_scheme("ws").ok()?,
        "https" => ws_url.set_scheme("wss").ok()?,
        _ => return None,
    }
    let port = http_url.port()?.checked_add(1)?;
    ws_url.set_port(Some(port)).ok()?;
    Some(ws_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_ws_url_increments_port() {
        let http = Url::parse("http://localhost:8545").unwrap();
        let ws = derive_ws_url(&http).unwrap();
        assert_eq!(ws.as_str(), "ws://localhost:8546/");
    }

    #[test]
    fn derive_ws_url_https_to_wss() {
        let https = Url::parse("https://localhost:8545").unwrap();
        let wss = derive_ws_url(&https).unwrap();
        assert_eq!(wss.as_str(), "wss://localhost:8546/");
    }

    #[test]
    fn derive_ws_url_returns_none_on_port_overflow() {
        let http = Url::parse("http://localhost:65535").unwrap();
        assert!(derive_ws_url(&http).is_none());
    }

    #[test]
    fn derive_ws_url_returns_none_without_port() {
        let http = Url::parse("http://localhost").unwrap();
        assert!(derive_ws_url(&http).is_none());
    }

    #[test]
    fn derive_ws_url_returns_none_for_unsupported_scheme() {
        let ftp = Url::parse("ftp://localhost:8545").unwrap();
        assert!(derive_ws_url(&ftp).is_none());
    }
}
