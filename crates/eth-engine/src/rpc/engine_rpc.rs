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

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use alloy_rpc_types_engine::{
    ExecutionPayloadEnvelopeV4, ExecutionPayloadEnvelopeV5, ExecutionPayloadV3, ForkchoiceState,
    ForkchoiceUpdated, PayloadAttributes, PayloadId as AlloyPayloadId, PayloadStatus,
};
use async_trait::async_trait;
use backon::{Backoff, BackoffBuilder};
use eyre::Context;
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde_json::json;
use tracing::debug;

use arc_consensus_types::{BlockHash, Bytes, B256};

use crate::capabilities::EngineCapabilities;
use crate::constants::*;
use crate::engine::EngineAPI;
use crate::retry::NoRetry;
use crate::rpc::auth::Auth;
use crate::rpc::request_builder::RpcRequestBuilder;

/// RPC client for connecting to Engine RPC endpoint with JWT authentication.
pub struct EngineRpc {
    client: Client,
    url: Url,
    auth: Auth,
}

impl std::fmt::Display for EngineRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.url)
    }
}

impl EngineRpc {
    /// Create a new `EngineRpc` struct given the URL and JWT path.
    pub fn new(url: Url, jwt_path: &Path) -> eyre::Result<Self> {
        Ok(Self {
            client: Client::builder().build()?,
            url,
            auth: Auth::new_from_path(jwt_path).wrap_err("Failed to load configuration file")?,
        })
    }

    /// Building an RPC request to the Ethereum server.
    /// - method: The method to call.
    fn build_rpc_request<'a, 'b>(&'a self, method: &'b str) -> RpcRequestBuilder<'a>
    where
        'b: 'a,
    {
        RpcRequestBuilder::new(&self.client, &self.url, method)
    }

    /// Send an RPC request to the Ethereum server.
    /// - method: The method to call.
    /// - params: The parameters to pass to the method.
    /// - timeout: The timeout for the request.
    /// - retry_policy: The retry policy for the request.
    pub async fn rpc_request<D: DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
        retry_policy: impl Backoff,
    ) -> eyre::Result<D> {
        self.build_rpc_request(method)
            .params(params)
            .timeout(timeout)
            .retry(retry_policy)
            .bearer_auth(self.auth.generate_token()?)
            .send()
            .await
    }

    /// Exchange capabilities with the Engine RPC endpoint.
    pub async fn exchange_capabilities(&self) -> eyre::Result<EngineCapabilities> {
        let capabilities: HashSet<String> = self
            .rpc_request(
                ENGINE_EXCHANGE_CAPABILITIES,
                json!([NODE_CAPABILITIES]),
                ENGINE_EXCHANGE_CAPABILITIES_TIMEOUT,
                ENGINE_EXCHANGE_CAPABILITIES_RETRY_RPC.build(),
            )
            .await?;
        debug!("🟠 EngineAPI: exchange_capabilities: {:?}", capabilities);
        Ok(EngineCapabilities::from_capabilities(&capabilities))
    }

    /// Notify that a fork choice has been updated, to set the head of the chain
    /// - head_block_hash: The block hash of the head of the chain
    /// - maybe_payload_attributes: Optional payload attributes for the next block
    pub async fn forkchoice_updated(
        &self,
        head_block_hash: BlockHash,
        maybe_payload_attributes: Option<PayloadAttributes>,
    ) -> eyre::Result<ForkchoiceUpdated> {
        let forkchoice_state = ForkchoiceState {
            head_block_hash,
            safe_block_hash: head_block_hash,
            finalized_block_hash: head_block_hash,
        };
        let res = self
            .rpc_request(
                ENGINE_FORKCHOICE_UPDATED_V3,
                json!([forkchoice_state, maybe_payload_attributes]),
                ENGINE_FORKCHOICE_UPDATED_TIMEOUT,
                NoRetry,
            )
            .await;
        debug!(
            "🟠 EngineAPI: forkchoice_updated{}: {:?}",
            maybe_payload_attributes
                .map(|_| " with payload attributes")
                .unwrap_or(""),
            head_block_hash
        );
        res
    }

    /// Get a payload by its ID.
    /// Uses V5 (Osaka) when `use_v5` is true, otherwise V4.
    pub async fn get_payload(
        &self,
        payload_id: AlloyPayloadId,
        use_v5: bool,
    ) -> eyre::Result<ExecutionPayloadV3> {
        let execution_payload = if use_v5 {
            let ExecutionPayloadEnvelopeV5 {
                execution_payload, ..
            } = self
                .rpc_request(
                    ENGINE_GET_PAYLOAD_V5,
                    json!([payload_id]),
                    ENGINE_GET_PAYLOAD_TIMEOUT,
                    NoRetry,
                )
                .await?;
            execution_payload
        } else {
            let ExecutionPayloadEnvelopeV4 { envelope_inner, .. } = self
                .rpc_request(
                    ENGINE_GET_PAYLOAD_V4,
                    json!([payload_id]),
                    ENGINE_GET_PAYLOAD_TIMEOUT,
                    NoRetry,
                )
                .await?;
            envelope_inner.execution_payload
        };
        debug!(
            "🟠 EngineAPI: get_payload: {:?}",
            execution_payload.payload_inner.payload_inner.block_hash,
        );
        Ok(execution_payload)
    }

    /// Notify that a new payload has been created.
    /// - execution_payload: The execution payload to be included in the next block.
    /// - versioned_hashes: The versioned hashes of the blobs in the execution payload.
    /// - parent_block_hash: The hash of the parent block.
    pub async fn new_payload(
        &self,
        execution_payload: &ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_block_hash: BlockHash,
    ) -> eyre::Result<PayloadStatus> {
        let empty_execution_requests: Vec<Bytes> = Vec::new();
        let params = json!([
            execution_payload,
            versioned_hashes,
            parent_block_hash,
            empty_execution_requests
        ]);
        let res = self
            .rpc_request(
                ENGINE_NEW_PAYLOAD_V4,
                params,
                ENGINE_NEW_PAYLOAD_TIMEOUT,
                NoRetry,
            )
            .await;

        let block_hash = execution_payload.payload_inner.payload_inner.block_hash;
        debug!("🟠 EngineAPI: new_payload: {:?}", block_hash);

        res
    }
}

#[async_trait]
impl EngineAPI for EngineRpc {
    /// Exchange capabilities with the engine.
    async fn exchange_capabilities(&self) -> eyre::Result<EngineCapabilities> {
        EngineRpc::exchange_capabilities(self)
            .await
            .wrap_err("EngineRpc exchange_capabilities call failed")
    }

    /// Set the latest forkchoice state.
    async fn forkchoice_updated(
        &self,
        head_block_hash: BlockHash,
        maybe_payload_attributes: Option<PayloadAttributes>,
    ) -> eyre::Result<ForkchoiceUpdated> {
        EngineRpc::forkchoice_updated(self, head_block_hash, maybe_payload_attributes.clone())
            .await
            .wrap_err_with(|| {
                format!(
                    "EngineRpc forkchoice_updated call failed; block_hash {:?}, payload_attributes {:?}",
                    head_block_hash,
                    maybe_payload_attributes,
                )
            })
    }

    /// Get a payload by its ID.
    async fn get_payload(
        &self,
        payload_id: AlloyPayloadId,
        use_v5: bool,
    ) -> eyre::Result<ExecutionPayloadV3> {
        EngineRpc::get_payload(self, payload_id, use_v5)
            .await
            .wrap_err_with(|| format!("EngineRpc get_payload call failed; payload_id {payload_id}"))
    }

    /// Notify that a new payload has been created.
    async fn new_payload(
        &self,
        execution_payload: &ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_block_hash: BlockHash,
    ) -> eyre::Result<PayloadStatus> {
        let payload_hash = execution_payload.payload_inner.payload_inner.block_hash;
        EngineRpc::new_payload(self, execution_payload, versioned_hashes, parent_block_hash)
            .await
            .wrap_err_with(|| format!("EngineRpc new_payload call failed; block_hash {payload_hash}, parent_block_hash {parent_block_hash}"))
    }
}
