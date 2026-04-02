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

use std::time::Duration;

use backon::{Backoff, Retryable};
use eyre::{eyre, Context};
use jsonrpsee::{
    async_client::Client,
    core::{
        client::{ClientT, Error as RpcClientError},
        params::BatchRequestBuilder,
        traits::ToRpcParams,
    },
};
use reth_ipc::client::IpcClientBuilder;
use serde::de::DeserializeOwned;
use tracing::{debug, info, warn};

use crate::constants::IPC_CLIENT_TIMEOUT;
use crate::rpc::EngineApiRpcError;

/// Common IPC client functionality for connecting to IPC endpoints via Unix Domain Socket.
#[derive(Debug)]
pub struct Ipc {
    client: Client,
    socket_path: String,
}

impl std::fmt::Display for Ipc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IPC:{}", self.socket_path)
    }
}

impl Ipc {
    /// Create a new `IPC` struct with custom timeout. Defaults to 60 seconds.
    pub async fn new_with_timeout(socket_path: &str, timeout: Duration) -> eyre::Result<Self> {
        let client = IpcClientBuilder::default()
            .request_timeout(timeout)
            .build(socket_path)
            .await?;

        info!("🟢 Connected to IPC: {}", socket_path);

        Ok(Self {
            client,
            socket_path: socket_path.to_string(),
        })
    }

    /// Build an RPC request to the IPC endpoint.
    pub fn build_rpc_request<'a, 'b, P>(
        &'a self,
        method: &'b str,
        params: P,
    ) -> RpcRequestBuilder<'a, P>
    where
        'b: 'a,
        P: ToRpcParams + Clone + Send,
    {
        RpcRequestBuilder::new(&self.client, method, params)
    }

    /// Send an RPC request to the IPC endpoint.
    pub async fn rpc_request<D: DeserializeOwned>(
        &self,
        method: &str,
        params: impl ToRpcParams + Clone + Send,
        timeout: Duration,
        retry_policy: impl Backoff,
    ) -> eyre::Result<D> {
        self.build_rpc_request(method, params)
            .timeout(timeout)
            .retry(retry_policy)
            .send()
            .await
    }

    /// Send a batch of RPC requests.
    pub async fn batch_request<D: DeserializeOwned + std::fmt::Debug>(
        &self,
        method: &str,
        params_list: &[impl ToRpcParams + Clone + Send],
        timeout: Duration,
    ) -> eyre::Result<Vec<Option<D>>> {
        if params_list.is_empty() {
            return Ok(vec![]);
        }

        let mut batch_builder = BatchRequestBuilder::new();
        for params in params_list {
            batch_builder
                .insert(method, params.clone())
                .map_err(|e| eyre!("Failed to insert request into batch: {e}"))?;
        }

        let batch_response =
            tokio::time::timeout(timeout, self.client.batch_request(batch_builder))
                .await
                .map_err(|_| eyre!("IPC batch request timed out after {timeout:?}"))?
                .wrap_err("IPC batch request failed")?;

        let mut results = Vec::with_capacity(params_list.len());
        for (idx, batch_entry) in batch_response.into_iter().enumerate() {
            match batch_entry {
                Ok(response) => {
                    results.push(Some(response));
                }
                Err(error) => {
                    debug!(idx=%idx, error=?error, "IPC batch request failed for entry");
                    results.push(None);
                }
            }
        }

        // Ensure we have the expected number of results
        if results.len() != params_list.len() {
            return Err(eyre!(
                "Batch response length mismatch: expected {}, got {}",
                params_list.len(),
                results.len()
            ));
        }

        Ok(results)
    }

    /// Get the socket path this client is connected to
    pub fn socket_path(&self) -> &str {
        &self.socket_path
    }
}

use crate::retry::NoRetry;

/// A builder for creating and sending an IPC request.
pub struct RpcRequestBuilder<'a, P, B: Backoff = NoRetry> {
    client: &'a Client,
    method: &'a str,
    params: P,
    retry_policy: B,
    timeout: Duration,
}

impl<'a, P> RpcRequestBuilder<'a, P> {
    /// Creates a new builder instance.
    pub fn new(client: &'a Client, method: &'a str, params: P) -> Self {
        Self {
            client,
            method,
            params,
            retry_policy: NoRetry,
            timeout: IPC_CLIENT_TIMEOUT,
        }
    }
}

impl<'a, P, B: Backoff> RpcRequestBuilder<'a, P, B> {
    /// Sets the per-call timeout for the request.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the retry policy for the request.
    /// This consumes the current builder and returns a new one with the specified policy.
    pub fn retry<NB: Backoff>(self, policy: NB) -> RpcRequestBuilder<'a, P, NB> {
        RpcRequestBuilder {
            client: self.client,
            method: self.method,
            params: self.params,
            retry_policy: policy,
            timeout: self.timeout,
        }
    }

    /// Builds and sends the JSON-RPC request.
    pub async fn send<D>(self) -> eyre::Result<D>
    where
        D: DeserializeOwned,
        P: ToRpcParams + Clone + Send,
    {
        let timeout = self.timeout;
        let method = self.method;

        // Closure that sends the request and processes the response.
        // This will be retried according to the retry policy.
        let send_once = || async {
            tokio::time::timeout(
                timeout,
                self.client
                    .request::<D, _>(self.method, self.params.clone()),
            )
            .await
            .map_err(|_| eyre!("IPC request {method} timed out after {timeout:?}"))?
            .map_err(|e| match e {
                RpcClientError::Call(err) => {
                    let engine_rpc_error = EngineApiRpcError::from(err);
                    eyre::Report::new(engine_rpc_error)
                        .wrap_err(format!("IPC request {} failed", self.method))
                }
                other => {
                    eyre::Report::new(other).wrap_err(format!("IPC request {} failed", self.method))
                }
            })
        };

        // Use `backon::Retryable` to execute the closure with the given retry policy.
        // If the policy is `NoRetry`, it runs exactly once.
        send_once
            .retry(self.retry_policy)
            .notify(|e, dur| {
                warn!("IPC request failed: {e}, retrying in {dur:?}");
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_new_with_invalid_socket() {
        let result = Ipc::new_with_timeout("/nonexistent/socket", Duration::from_secs(60)).await;
        assert!(result.is_err());
    }
}
