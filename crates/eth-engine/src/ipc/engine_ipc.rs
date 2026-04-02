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

use alloy_rpc_types_engine::{
    ExecutionPayloadEnvelopeV4, ExecutionPayloadEnvelopeV5, ExecutionPayloadV3, ForkchoiceState,
    ForkchoiceUpdated, PayloadAttributes, PayloadId as AlloyPayloadId, PayloadStatus,
};
use async_trait::async_trait;
use backon::{Backoff, BackoffBuilder};
use jsonrpsee::core::traits::ToRpcParams;
use jsonrpsee::rpc_params;
use serde::de::DeserializeOwned;
use std::time::Duration;

use arc_consensus_types::{BlockHash, Bytes, B256};

use crate::capabilities::EngineCapabilities;
use crate::constants::*;
use crate::engine::EngineAPI;
use crate::ipc::ipc_builder::Ipc;
use crate::retry::NoRetry;

/// Engine API client for connecting to Engine IPC via Unix Domain Socket.
pub struct EngineIPC {
    ipc: Ipc,
}

impl std::fmt::Display for EngineIPC {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EngineIPC:{}", self.ipc.socket_path())
    }
}

impl EngineIPC {
    /// Create a new `EngineIPC` struct given the IPC socket path.
    pub async fn new(socket_path: &str) -> eyre::Result<Self> {
        Self::new_with_timeout(socket_path, IPC_CLIENT_TIMEOUT).await
    }

    /// Create a new `EngineIPC` struct with custom timeout.
    pub async fn new_with_timeout(socket_path: &str, timeout: Duration) -> eyre::Result<Self> {
        let ipc = Ipc::new_with_timeout(socket_path, timeout).await?;
        Ok(Self { ipc })
    }

    /// Send an RPC request to the Engine RPC endpoint via IPC.
    pub async fn rpc_request<D: DeserializeOwned>(
        &self,
        method: &str,
        params: impl ToRpcParams + Clone + Send,
        timeout: Duration,
        retry_policy: impl Backoff,
    ) -> eyre::Result<D> {
        self.ipc
            .rpc_request(method, params, timeout, retry_policy)
            .await
    }
}

#[async_trait]
impl EngineAPI for EngineIPC {
    /// Exchange capabilities with the Engine RPC endpoint.
    async fn exchange_capabilities(&self) -> eyre::Result<EngineCapabilities> {
        let capabilities: std::collections::HashSet<String> = self
            .rpc_request(
                ENGINE_EXCHANGE_CAPABILITIES,
                rpc_params![NODE_CAPABILITIES],
                ENGINE_EXCHANGE_CAPABILITIES_TIMEOUT,
                ENGINE_EXCHANGE_CAPABILITIES_RETRY_IPC.build(),
            )
            .await?;

        Ok(EngineCapabilities::from_capabilities(&capabilities))
    }

    /// Notify that a fork choice has been updated, to set the head of the chain
    /// - head_block_hash: The block hash of the head of the chain
    /// - maybe_payload_attributes: Optional payload attributes for the next block
    async fn forkchoice_updated(
        &self,
        head_block_hash: BlockHash,
        maybe_payload_attributes: Option<PayloadAttributes>,
    ) -> eyre::Result<ForkchoiceUpdated> {
        let forkchoice_state = ForkchoiceState {
            head_block_hash,
            safe_block_hash: head_block_hash,
            finalized_block_hash: head_block_hash,
        };

        let params = if let Some(attrs) = maybe_payload_attributes {
            rpc_params![forkchoice_state, attrs]
        } else {
            rpc_params![forkchoice_state]
        };

        self.rpc_request(
            ENGINE_FORKCHOICE_UPDATED_V3,
            params,
            ENGINE_FORKCHOICE_UPDATED_TIMEOUT,
            NoRetry,
        )
        .await
    }

    /// Get a payload by its ID.
    /// Uses V5 (Osaka) when `use_v5` is true, otherwise V4.
    async fn get_payload(
        &self,
        payload_id: AlloyPayloadId,
        use_v5: bool,
    ) -> eyre::Result<ExecutionPayloadV3> {
        if use_v5 {
            let ExecutionPayloadEnvelopeV5 {
                execution_payload, ..
            } = self
                .rpc_request(
                    ENGINE_GET_PAYLOAD_V5,
                    rpc_params![payload_id],
                    ENGINE_GET_PAYLOAD_TIMEOUT,
                    NoRetry,
                )
                .await?;
            Ok(execution_payload)
        } else {
            let ExecutionPayloadEnvelopeV4 { envelope_inner, .. } = self
                .rpc_request(
                    ENGINE_GET_PAYLOAD_V4,
                    rpc_params![payload_id],
                    ENGINE_GET_PAYLOAD_TIMEOUT,
                    NoRetry,
                )
                .await?;
            Ok(envelope_inner.execution_payload)
        }
    }

    /// Notify that a new payload has been created.
    /// - execution_payload: The execution payload to be included in the next block.
    /// - versioned_hashes: The versioned hashes of the blobs in the execution payload.
    /// - parent_block_hash: The hash of the parent block.
    async fn new_payload(
        &self,
        execution_payload: &ExecutionPayloadV3,
        versioned_hashes: Vec<B256>,
        parent_block_hash: BlockHash,
    ) -> eyre::Result<PayloadStatus> {
        let empty_execution_requests: Vec<Bytes> = Vec::new();
        let params = rpc_params![
            execution_payload,
            versioned_hashes,
            parent_block_hash,
            empty_execution_requests
        ];

        self.rpc_request(
            ENGINE_NEW_PAYLOAD_V4,
            params,
            ENGINE_NEW_PAYLOAD_TIMEOUT,
            NoRetry,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_new_with_invalid_socket() {
        let result = EngineIPC::new("/nonexistent/socket").await;
        assert!(result.is_err());
    }
}
