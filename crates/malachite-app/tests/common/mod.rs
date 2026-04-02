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

//! Common test utilities for RPC integration tests

use std::net::SocketAddr;
use std::sync::Arc;

use arc_consensus_types::ArcContext;
use arc_node_consensus::request::AppRequest;
use malachitebft_app_channel::{ConsensusRequest, NetworkRequest};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

/// Test server wrapper that manages the RPC server lifecycle
pub struct TestServer {
    addr: SocketAddr,
    _server_handle: JoinHandle<()>,
    app_rx: Arc<Mutex<mpsc::Receiver<AppRequest>>>,
    consensus_rx: Arc<Mutex<mpsc::Receiver<ConsensusRequest<ArcContext>>>>,
    network_rx: Arc<Mutex<mpsc::Receiver<NetworkRequest>>>,
}

impl TestServer {
    /// Start a new test server on a random available port
    pub async fn start() -> Self {
        Self::start_with_capacity(100).await
    }

    /// Start a new test server with specified channel capacity
    pub async fn start_with_capacity(capacity: usize) -> Self {
        // Create channels for communication
        let (app_tx, app_rx) = mpsc::channel(capacity);
        let (consensus_tx, consensus_rx) = mpsc::channel(capacity);
        let (network_tx, network_rx) = mpsc::channel(capacity);

        // Bind to a random available port
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind to random port");
        let addr = listener.local_addr().expect("Failed to get local address");

        // Build the actual production router
        let router = arc_node_consensus::rpc::build_router(consensus_tx, app_tx, network_tx);

        // Spawn the server
        let server_handle = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("Server failed to start");
        });

        Self {
            addr,
            _server_handle: server_handle,
            app_rx: Arc::new(Mutex::new(app_rx)),
            consensus_rx: Arc::new(Mutex::new(consensus_rx)),
            network_rx: Arc::new(Mutex::new(network_rx)),
        }
    }

    /// Get the base URL for the test server
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Expect and handle an app request
    pub fn expect_app_request<F>(&self, handler: F)
    where
        F: FnOnce(AppRequest) + Send + 'static,
    {
        let rx = Arc::clone(&self.app_rx);
        tokio::spawn(async move {
            let mut rx = rx.lock().await;
            if let Some(req) = rx.recv().await {
                handler(req);
            }
        });
    }

    /// Expect and handle a consensus request
    pub fn expect_consensus_request<F>(&self, handler: F)
    where
        F: FnOnce(ConsensusRequest<ArcContext>) + Send + 'static,
    {
        let rx = Arc::clone(&self.consensus_rx);
        tokio::spawn(async move {
            let mut rx = rx.lock().await;
            if let Some(req) = rx.recv().await {
                handler(req);
            }
        });
    }

    /// Expect and handle a network request
    pub fn expect_network_request<F>(&self, handler: F)
    where
        F: FnOnce(NetworkRequest) + Send + 'static,
    {
        let rx = Arc::clone(&self.network_rx);
        tokio::spawn(async move {
            let mut rx = rx.lock().await;
            if let Some(req) = rx.recv().await {
                handler(req);
            }
        });
    }
}
