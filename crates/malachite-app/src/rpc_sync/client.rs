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

//! RPC Client for fetching blocks from remote endpoints
//!
//! This client fetches commit certificates and execution payloads
//! from trusted RPC endpoints. It is used by the network actor to
//! fulfill block requests from malachite's sync actor.
//!
//! Note: Validator sets are not fetched here, consensus uses its own
//! validator set state to verify signatures.

use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use alloy_rpc_types_engine::ExecutionPayloadV3;
use alloy_rpc_types_eth::Block;
use bytes::Bytes;
use eyre::Context;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use serde_with::{base64::Base64, serde_as};
use tracing::debug;
use url::Url;

use malachitebft_core_types::utils::height::HeightRangeExt;
use malachitebft_core_types::{CommitCertificate, CommitSignature, Round};

use arc_consensus_types::signing::Signature;
use arc_consensus_types::{Address, ArcContext, Height, ValueId};

use crate::utils::pretty::Pretty;

/// RPC representation of a commit certificate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcCommitCertificate {
    pub height: u64,
    pub round: i64,
    pub block_hash: ValueId,
    pub signatures: Vec<RpcCommitSignature>,
}

/// RPC representation of a commit signature
#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcCommitSignature {
    pub address: Address,
    #[serde_as(as = "Base64")]
    pub signature: Vec<u8>,
}

impl RpcCommitCertificate {
    /// Convert to internal CommitCertificate type
    pub fn into_commit_certificate(self) -> eyre::Result<CommitCertificate<ArcContext>> {
        let signatures: eyre::Result<Vec<CommitSignature<ArcContext>>> =
            self.signatures
                .into_iter()
                .map(|sig| {
                    let sig_bytes: [u8; 64] = sig.signature.try_into().map_err(|v: Vec<u8>| {
                        eyre::eyre!("Invalid signature length: {}", v.len())
                    })?;
                    Ok(CommitSignature {
                        address: sig.address,
                        signature: Signature::from_bytes(sig_bytes),
                    })
                })
                .collect();

        let round_u32: u32 = self.round.try_into().map_err(|e| {
            eyre::eyre!(
                "Invalid round value {}, cannot convert to u32: {}",
                self.round,
                e
            )
        })?;

        Ok(CommitCertificate {
            height: Height::new(self.height),
            round: Round::new(round_u32),
            value_id: self.block_hash,
            commit_signatures: signatures?,
        })
    }
}

/// A block with its associated data (certificate + payload)
#[derive(Debug, Clone)]
pub struct SyncedBlock {
    pub height: Height,
    pub certificate: CommitCertificate<ArcContext>,
    pub payload: ExecutionPayloadV3,
    pub value_bytes: Bytes,
}

/// RPC client for fetching blocks
#[derive(Clone)]
pub struct RpcSyncClient {
    client: Client,
}

impl Default for RpcSyncClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcSyncClient {
    /// Create a new RPC sync client with connection pooling
    pub fn new() -> Self {
        let client = Client::builder()
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(60))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self { client }
    }

    /// Fetch a batch of blocks from a specific endpoint.
    /// The caller, malachite sync actor, handles peer selection and retry logic.
    pub async fn fetch_blocks_batch(
        &self,
        endpoint: &Url,
        range: &RangeInclusive<Height>,
    ) -> eyre::Result<Vec<SyncedBlock>> {
        self.fetch_blocks_batch_impl(endpoint, range).await
    }

    async fn fetch_blocks_batch_impl(
        &self,
        endpoint: &Url,
        range: &RangeInclusive<Height>,
    ) -> eyre::Result<Vec<SyncedBlock>> {
        let mut blocks = Vec::with_capacity(range.len());

        // Build batch requests for certificates and payloads only
        // Note: Validator sets are NOT needed - consensus has its own validator set state
        let mut cert_requests = Vec::with_capacity(range.len());
        let mut block_requests = Vec::with_capacity(range.len());

        for height in range.clone().iter_heights() {
            let height_hex = format!("0x{:x}", height.as_u64());

            cert_requests.push(json!({
                "jsonrpc": "2.0",
                "method": "arc_getCertificate",
                "params": [height.as_u64()],
                "id": height.as_u64()
            }));

            block_requests.push(json!({
                "jsonrpc": "2.0",
                "method": "eth_getBlockByNumber",
                "params": [height_hex, true],
                "id": height.as_u64()
            }));
        }

        // Send both batch requests in parallel
        let start = Instant::now();

        let (cert_result, payload_result) = tokio::join!(
            self.send_batch_request::<RpcCommitCertificate>(endpoint, &cert_requests),
            self.send_batch_request::<Block>(endpoint, &block_requests),
        );

        let elapsed = start.elapsed();

        let cert_responses = cert_result.wrap_err("Failed to fetch certificates")?;
        let block_responses = payload_result.wrap_err("Failed to fetch blocks")?;

        debug!(
            %endpoint,
            range = %Pretty(range),
            ?elapsed,
            certs = cert_responses.len(),
            blocks = block_responses.len(),
            "Fetched batch",
        );

        // Verify we got exactly what we asked for (zip silently drops extras)
        let expected_count = range.len();
        let (cert_count, block_count) = (cert_responses.len(), block_responses.len());
        if cert_count != expected_count || block_count != expected_count {
            return Err(eyre::eyre!(
                "Response count mismatch: expected {expected_count}, got {cert_count} certificates and {block_count} blocks",
            ));
        }

        // Parse responses and build blocks, verifying heights match exactly what we requested
        for ((height, rpc_cert), block) in range
            .clone()
            .iter_heights()
            .zip(cert_responses)
            .zip(block_responses)
        {
            let cert_height = Height::new(rpc_cert.height);
            let block_height = Height::new(block.number());

            // Verify both certificate and block match the expected height
            if cert_height != height {
                return Err(eyre::eyre!(
                    "Certificate height mismatch: expected {}, got {}",
                    height,
                    cert_height
                ));
            }

            if block_height != height {
                return Err(eyre::eyre!(
                    "Block height mismatch: expected {}, got {}",
                    height,
                    block_height
                ));
            }

            let certificate = rpc_cert
                .into_commit_certificate()
                .wrap_err_with(|| format!("Failed to convert certificate at height {height}"))?;

            // Convert Block to ExecutionPayloadV3, verifying hash consistency
            let payload = block_to_execution_payload(block)
                .wrap_err_with(|| format!("Invalid block at height {height}"))?;

            // Encode payload to SSZ bytes for consensus
            let value_bytes = Bytes::from(ssz::Encode::as_ssz_bytes(&payload));

            blocks.push(SyncedBlock {
                height,
                certificate,
                payload,
                value_bytes,
            });
        }

        Ok(blocks)
    }

    async fn send_batch_request<V>(
        &self,
        endpoint: &Url,
        requests: &[Value],
    ) -> eyre::Result<Vec<V>>
    where
        V: for<'de> Deserialize<'de>,
    {
        let response = self
            .client
            .post(endpoint.clone())
            .json(requests)
            .send()
            .await
            .wrap_err("Failed to send batch request")?
            .error_for_status()
            .wrap_err("RPC endpoint returned an error status")?;

        let mut responses: Vec<json_rpc::Response<V>> = response
            .json()
            .await
            .wrap_err("Failed to parse batch response")?;

        // Sort by id to restore original request order, since the
        // JSON-RPC spec does not guarantee batch response ordering.
        responses.sort_by_key(|r| r.id);

        responses
            .into_iter()
            .map(|r| match (r.result, r.error) {
                (Some(result), _) => Ok(result),
                (_, Some(error)) => Err(eyre::eyre!("JSON-RPC error: {error}")),
                (None, None) => Err(eyre::eyre!(
                    "JSON-RPC response (id={}) has neither result nor error",
                    r.id
                )),
            })
            .collect()
    }
}

mod json_rpc {
    use serde::Deserialize;
    use serde_json::Value;

    /// A JSON-RPC error object returned when the server reports a failure.
    #[derive(Debug, Deserialize)]
    pub struct Error {
        pub code: i64,
        pub message: String,
        #[serde(default)]
        pub data: Option<Value>,
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "code {}: {}", self.code, self.message)?;
            if let Some(data) = &self.data {
                write!(f, " (data: {data})")?;
            }
            Ok(())
        }
    }

    /// Raw JSON-RPC response with the `id` field preserved so that
    /// batch responses can be re-ordered to match the original request
    /// order (the JSON-RPC spec does not guarantee ordering).
    #[derive(Deserialize)]
    #[serde(bound(deserialize = "V: Deserialize<'de>"))]
    pub struct Response<V> {
        pub id: u64,
        pub result: Option<V>,
        pub error: Option<Error>,
    }
}

/// Convert an Ethereum block (from eth_getBlockByNumber) to ExecutionPayloadV3
///
/// Verifies the block hash by recomputing it from header fields. This ensures the RPC
/// response is internally consistent (claimed hash matches actual header content).
/// Any header field tampering will be detected and rejected early.
///
/// CL (malachite) will verify the block hash matches the certificate's value_id (signed by validators).
/// EL (reth) will additionally validate body-header consistency (transactions_root, withdrawals_root)
/// and execution correctness (state_root, receipts_root, gas_used) when processing the block.
fn block_to_execution_payload(block: Block) -> eyre::Result<ExecutionPayloadV3> {
    use alloy_consensus::TxEnvelope;

    let claimed_hash = block.header.hash;
    let computed_hash = block.header.inner.hash_slow();

    if claimed_hash != computed_hash {
        return Err(eyre::eyre!(
            "Block hash mismatch: RPC claimed {claimed_hash}, computed {computed_hash}"
        ));
    }

    let consensus_block = block.into_consensus().convert_transactions::<TxEnvelope>();
    Ok(ExecutionPayloadV3::from_block_unchecked(
        claimed_hash,
        &consensus_block,
    ))
}
