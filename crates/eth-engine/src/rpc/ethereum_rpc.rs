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

use alloy_consensus::TxEnvelope;
use alloy_rpc_types::Block;
use alloy_rpc_types_engine::ExecutionPayloadV3;
use alloy_rpc_types_txpool::{TxpoolInspect, TxpoolStatus};
use alloy_sol_types::SolCall;
use async_trait::async_trait;
use backon::BackoffBuilder;
use eyre::Context;
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde_json::{from_value, json, Value};
use tracing::{debug, trace};

use arc_consensus_types::{ConsensusParams, ValidatorSet};

use crate::abi_utils::{
    abi_decode_consensus_params, abi_decode_validator_set, consensusParamsCall,
    getActiveValidatorSetCall,
};
use crate::constants::{
    ETH_BATCH_REQUEST_TIMEOUT, ETH_CALL_RETRY, ETH_DEFAULT_TIMEOUT, PROTOCOL_CONFIG_ADDRESS,
    VALIDATOR_REGISTRY_ADDRESS,
};
use crate::engine::EthereumAPI;
use crate::json_structures::ExecutionBlock;
use crate::rpc::request_builder::RpcRequestBuilder;

/// Generate ABI parameters for RPC calls to get active validator set
fn abi_get_active_validator_set_params_rpc(block_height: u64) -> eyre::Result<Value> {
    // Encode the 4‑byte selector + (no args)
    let calldata = getActiveValidatorSetCall {}.abi_encode();
    let data = format!("0x{}", hex::encode(calldata));

    Ok(serde_json::json!([{
        "to": format!("{:#x}", VALIDATOR_REGISTRY_ADDRESS),
        "data": data
    }, format!("0x{:x}", block_height)]))
}

/// Generate ABI parameters for RPC calls to get consensus params
fn abi_get_consensus_params_params_rpc(block_height: u64) -> eyre::Result<Value> {
    let calldata = consensusParamsCall {}.abi_encode();
    let data = format!("0x{}", hex::encode(calldata));

    Ok(serde_json::json!([{
        "to": format!("{:#x}", PROTOCOL_CONFIG_ADDRESS),
        "data": data
    }, format!("0x{:x}", block_height)]))
}

/// RPC client for Ethereum server.
pub struct EthereumRPC {
    client: Client,
    url: Url,
}

impl EthereumRPC {
    /// Create a new `EthereumRPC` struct given the URL.
    pub fn new(url: Url) -> eyre::Result<Self> {
        Ok(Self {
            client: Client::builder().build()?,
            url,
        })
    }

    /// Building an RPC request to the Ethereum server.
    /// - method: The method to call.
    pub fn build_rpc_request<'a, 'b>(&'a self, method: &'b str) -> RpcRequestBuilder<'a>
    where
        'b: 'a,
    {
        RpcRequestBuilder::new(&self.client, &self.url, method)
    }

    /// Send an RPC request to the Ethereum server.
    /// - method: The method to call.
    /// - params: The parameters to pass to the method.
    /// - timeout: The timeout for the request.
    pub async fn rpc_request<D: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> eyre::Result<D> {
        self.build_rpc_request(method)
            .params(params)
            .timeout(timeout)
            .send()
            .await
    }

    /// Probe the RPC server with `net_listening` to confirm
    /// it is accepting requests.
    pub async fn check_connectivity(&self) -> eyre::Result<bool> {
        self.rpc_request("net_listening", json!([]), ETH_DEFAULT_TIMEOUT)
            .await
    }

    /// Get the eth1 chain id of the given endpoint.
    pub async fn get_chain_id(&self) -> eyre::Result<String> {
        self.build_rpc_request("eth_chainId")
            .params(json!([]))
            .timeout(ETH_DEFAULT_TIMEOUT)
            .retry(ETH_CALL_RETRY.build())
            .send()
            .await
    }

    /// Get the genesis block.
    pub async fn get_genesis_block(&self) -> eyre::Result<ExecutionBlock> {
        let block: Option<ExecutionBlock> = self
            .rpc_request(
                "eth_getBlockByNumber",
                json!(["0x0", false]),
                ETH_DEFAULT_TIMEOUT,
            )
            .await?;

        block.ok_or_else(|| eyre::eyre!("Genesis block not found"))
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_active_validator_set(&self, block_height: u64) -> eyre::Result<ValidatorSet> {
        let params = abi_get_active_validator_set_params_rpc(block_height)?;
        debug!("eth_call params: {params}");

        let result: String = self
            .build_rpc_request("eth_call")
            .params(params)
            .timeout(ETH_DEFAULT_TIMEOUT)
            .retry(ETH_CALL_RETRY.build())
            .send()
            .await
            .wrap_err_with(|| {
                format!(
                    "eth_call request for active validator set failed for height={block_height}"
                )
            })?;

        trace!("eth_call result: {result}");

        let result = hex::decode(result.trim_start_matches("0x"))?;
        abi_decode_validator_set(result)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_consensus_params(&self, block_height: u64) -> eyre::Result<ConsensusParams> {
        let params = abi_get_consensus_params_params_rpc(block_height)?;
        debug!("eth_call params: {params}");

        let result: String = self
            .build_rpc_request("eth_call")
            .params(params)
            .timeout(ETH_DEFAULT_TIMEOUT)
            .retry(ETH_CALL_RETRY.build())
            .send()
            .await
            .wrap_err_with(|| {
                format!("eth_call request for consensus params failed for height={block_height}")
            })?;

        trace!("eth_call result: {result}");

        let result = hex::decode(result.trim_start_matches("0x"))?;
        abi_decode_consensus_params(result)
    }

    /// Get a block by its number.
    /// - block_number: The number of the block to get.
    pub async fn get_block_by_number(
        &self,
        block_number: &str,
    ) -> eyre::Result<Option<ExecutionBlock>> {
        let return_full_transaction_objects = false;
        let params = json!([block_number, return_full_transaction_objects]);
        self.rpc_request("eth_getBlockByNumber", params, ETH_DEFAULT_TIMEOUT)
            .await
    }

    /// Get a batch of full execution payloads.
    async fn get_execution_payloads(
        &self,
        block_numbers: &[String],
    ) -> eyre::Result<Vec<Option<ExecutionPayloadV3>>> {
        if block_numbers.is_empty() {
            return Ok(vec![]);
        }
        debug!("EthereumRPC: get_execution_payloads for block_numbers={block_numbers:?}");

        // Build JSON-RPC batch request for full blocks with transactions
        let return_full_transaction_objects = true;
        let batch_requests = block_numbers
            .iter()
            .enumerate()
            .map(|(id, block_number)| {
                json!({
                    "jsonrpc": "2.0",
                    "method": "eth_getBlockByNumber",
                    "params": [block_number, return_full_transaction_objects],
                    "id": id
                })
            })
            .collect::<Vec<_>>();

        // Send batch request
        let response = self
            .client
            .post(self.url.clone())
            .json(&batch_requests)
            .timeout(ETH_BATCH_REQUEST_TIMEOUT)
            .send()
            .await
            .wrap_err("Failed to send RPC batch request")?;

        let batch_responses: Vec<Value> = response
            .json()
            .await
            .wrap_err("Failed to parse batch response")?;

        // Parse results maintaining order and convert to execution payloads
        let mut results = vec![None; block_numbers.len()];
        let mut processed_ids = std::collections::HashSet::new();

        for response in batch_responses {
            let (Some(id), Some(result)) = (
                response.get("id").and_then(|v| v.as_u64()),
                response.get("result"),
            ) else {
                if let Some(error) = response.get("error") {
                    debug!("RPC batch response error: {}", error);
                }
                continue;
            };

            let id = id as usize;
            if id >= block_numbers.len() {
                debug!(
                    "Invalid response ID {} for batch of size {}",
                    id,
                    block_numbers.len()
                );
                continue;
            }

            processed_ids.insert(id);

            if result.is_null() {
                debug!(id=%id, block_number=%block_numbers[id], "No block found for request");
                continue;
            }

            // Parse as full block with transactions
            match from_value::<Block>(result.clone()) {
                Ok(block) => {
                    let block_hash = block.header.hash;
                    let consensus_block =
                        block.into_consensus().convert_transactions::<TxEnvelope>();
                    let execution_payload =
                        ExecutionPayloadV3::from_block_unchecked(block_hash, &consensus_block);

                    if let Some(entry) = results.get_mut(id) {
                        *entry = Some(execution_payload);
                    }
                }
                Err(e) => {
                    debug!(id=%id, error=%e, "Failed to parse block for request");
                }
            }
        }

        for (idx, block_number) in block_numbers.iter().enumerate() {
            if !processed_ids.contains(&idx) {
                debug!(idx=%idx, block_number=%block_number, "No response received for request");
            }
        }

        Ok(results)
    }

    /// Get the status of the transaction pool.
    pub async fn txpool_status(&self) -> eyre::Result<TxpoolStatus> {
        self.rpc_request("txpool_status", json!([]), ETH_DEFAULT_TIMEOUT)
            .await
    }

    /// Get the contents of the transaction pool.
    pub async fn txpool_inspect(&self) -> eyre::Result<TxpoolInspect> {
        self.rpc_request("txpool_inspect", json!([]), ETH_DEFAULT_TIMEOUT)
            .await
    }
}

#[async_trait]
impl EthereumAPI for EthereumRPC {
    /// Get the eth1 chain id of the given endpoint.
    async fn get_chain_id(&self) -> eyre::Result<String> {
        self.get_chain_id()
            .await
            .wrap_err("EthereumRPC get_chain_id call failed")
    }

    /// Get the genesis block.
    async fn get_genesis_block(&self) -> eyre::Result<ExecutionBlock> {
        self.get_genesis_block()
            .await
            .wrap_err("EthereumRPC get_genesis_block call failed")
    }

    /// Get the active validator set at a specific block height.
    async fn get_active_validator_set(&self, block_height: u64) -> eyre::Result<ValidatorSet> {
        self.get_active_validator_set(block_height)
            .await
            .wrap_err_with(|| {
                format!(
                    "EthereumRPC get_active_validator_set call failed for height={block_height}"
                )
            })
    }

    /// Get the consensus parameters at a specific block height.
    async fn get_consensus_params(&self, block_height: u64) -> eyre::Result<ConsensusParams> {
        self.get_consensus_params(block_height).await
    }

    /// Get a block by its number.
    async fn get_block_by_number(
        &self,
        block_number: &str,
    ) -> eyre::Result<Option<ExecutionBlock>> {
        self.get_block_by_number(block_number)
            .await
            .wrap_err_with(|| {
                format!(
                    "EthereumRPC get_block_by_number call failed for block number={block_number}"
                )
            })
    }

    /// Get a batch of full execution payloads.
    async fn get_execution_payloads(
        &self,
        block_numbers: &[String],
    ) -> eyre::Result<Vec<Option<ExecutionPayloadV3>>> {
        self.get_execution_payloads(block_numbers)
            .await
            .wrap_err_with(|| {
                format!(
                    "EthereumRPC get_execution_payloads call failed for block numbers={block_numbers:?}"
                )
            })
    }

    /// Get the status of the transaction pool.
    async fn txpool_status(&self) -> eyre::Result<TxpoolStatus> {
        self.txpool_status()
            .await
            .wrap_err("EthereumRPC txpool_status call failed")
    }

    /// Get the contents of the transaction pool.
    async fn txpool_inspect(&self) -> eyre::Result<TxpoolInspect> {
        self.txpool_inspect()
            .await
            .wrap_err("EthereumRPC txpool_inspect call failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::Header as ConsensusHeader;
    use alloy_primitives::B256;
    use alloy_rpc_types_eth::{Block, Header};
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn create_test_block(
        number: u64,
        hash: &str,
        parent_hash: &str,
        timestamp: u64,
        gas_used: u64,
    ) -> serde_json::Value {
        let inner = ConsensusHeader {
            number,
            timestamp,
            parent_hash: parent_hash.parse::<B256>().unwrap(),
            gas_used,
            ..Default::default()
        };
        let header = Header {
            hash: hash.parse::<B256>().unwrap(),
            inner,
            ..Default::default()
        };
        let block: Block<()> = Block {
            header,
            ..Default::default()
        };

        serde_json::to_value(block).unwrap()
    }

    #[test]
    fn test_encode_get_valset_params_abi_rpc() {
        let block_number = 4567;
        let params = abi_get_active_validator_set_params_rpc(block_number).unwrap();

        let expected = serde_json::json!([
            {
                "to": VALIDATOR_REGISTRY_ADDRESS,
                "data": "0x24408a68"
            },
            "0x11d7"
        ]);
        assert_eq!(params, expected);
    }

    #[test]
    fn test_encode_get_consensus_params_abi_rpc() {
        let block_number = 4567;
        let params = abi_get_consensus_params_params_rpc(block_number).unwrap();

        let expected = serde_json::json!([
            {
                "to": PROTOCOL_CONFIG_ADDRESS,
                "data": "0x9fd02a36"
            },
            "0x11d7"
        ]);
        assert_eq!(params, expected);
    }

    #[tokio::test]
    async fn test_ethereum_rpc_get_genesis_block_success() {
        let server = MockServer::start().await;

        let genesis_hash = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let genesis_block = create_test_block(
            0,
            genesis_hash,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0,
            0,
        );

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": "1",
                "result": genesis_block
            })))
            .mount(&server)
            .await;

        let url = Url::parse(&server.uri()).unwrap();
        let ethereum_rpc = EthereumRPC {
            url,
            client: Client::new(),
        };

        let result = ethereum_rpc.get_genesis_block().await.unwrap();
        assert_eq!(result.block_hash, genesis_hash.parse::<B256>().unwrap());
        assert_eq!(result.timestamp, 0);
    }

    #[tokio::test]
    async fn test_ethereum_rpc_get_genesis_block_not_found() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": "1",
                "result": null
            })))
            .mount(&server)
            .await;

        let url = Url::parse(&server.uri()).unwrap();
        let ethereum_rpc = EthereumRPC {
            url,
            client: Client::new(),
        };

        let err = ethereum_rpc.get_genesis_block().await.unwrap_err();
        assert!(err.to_string().contains("Genesis block not found"));
    }

    #[tokio::test]
    async fn test_ethereum_rpc_batch_payloads_success() {
        let server = MockServer::start().await;

        let block1 = create_test_block(
            1,
            "0x1234567890123456789012345678901234567890123456789012345678901234",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0x64,
            12345,
        );
        let block2 = create_test_block(
            2,
            "0x2345678901234567890123456789012345678901234567890123456789012345",
            "0x1234567890123456789012345678901234567890123456789012345678901234",
            0xc8,
            26000,
        );

        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_json(json!([
                {
                    "jsonrpc": "2.0",
                    "method": "eth_getBlockByNumber",
                    "params": ["0x1", true],
                    "id": 0
                },
                {
                    "jsonrpc": "2.0",
                    "method": "eth_getBlockByNumber",
                    "params": ["0x2", true],
                    "id": 1
                }
            ])))
            // Reverse order to test matching by ID
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": block2
                },
                {
                    "jsonrpc": "2.0",
                    "id": 0,
                    "result": block1
                }
            ])))
            .mount(&server)
            .await;

        let url = Url::parse(&server.uri()).unwrap();
        let ethereum_rpc = EthereumRPC {
            url,
            client: Client::new(),
        };

        let block_numbers = vec!["0x1".to_string(), "0x2".to_string()];
        let result = ethereum_rpc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert!(result[0].is_some());
        assert!(result[1].is_some());

        let payload1 = result[0].as_ref().unwrap();
        let payload2 = result[1].as_ref().unwrap();

        assert_eq!(payload1.payload_inner.payload_inner.block_number, 1);
        assert_eq!(payload1.payload_inner.payload_inner.gas_used, 12345);
        assert_eq!(payload2.payload_inner.payload_inner.block_number, 2);
        assert_eq!(payload2.payload_inner.payload_inner.gas_used, 26000);
    }

    #[tokio::test]
    async fn test_ethereum_rpc_batch_payloads_partial_failure() {
        let server = MockServer::start().await;

        let block1 = create_test_block(
            5,
            "0x1234567890123456789012345678901234567890123456789012345678901234",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0x64,
            33344,
        );

        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_json(json!([
                {
                    "jsonrpc": "2.0",
                    "method": "eth_getBlockByNumber",
                    "params": ["0x1", true],
                    "id": 0
                },
                {
                    "jsonrpc": "2.0",
                    "method": "eth_getBlockByNumber",
                    "params": ["0x999", true],
                    "id": 1
                }
            ])))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "jsonrpc": "2.0",
                    "id": 0,
                    "result": block1
                },
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": null
                }
            ])))
            .mount(&server)
            .await;

        let url = Url::parse(&server.uri()).unwrap();
        let ethereum_rpc = EthereumRPC {
            url,
            client: Client::new(),
        };

        let block_numbers = vec!["0x1".to_string(), "0x999".to_string()];
        let result = ethereum_rpc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert!(result[0].is_some());

        let payload = result[0].as_ref().unwrap();

        assert_eq!(payload.payload_inner.payload_inner.block_number, 5);
        assert_eq!(payload.payload_inner.payload_inner.gas_used, 33344);

        assert!(result[1].is_none());
    }

    #[tokio::test]
    async fn test_ethereum_rpc_batch_payloads_response_length_validation() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_json(json!([
                {
                    "jsonrpc": "2.0",
                    "method": "eth_getBlockByNumber",
                    "params": ["0x1", true],
                    "id": 0
                }
            ])))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "jsonrpc": "2.0",
                    "id": 10, // Invalid ID - too large
                    "result": {
                        "number": "0x1",
                        "hash": "0x1234567890123456789012345678901234567890123456789012345678901234"
                    }
                }
            ])))
            .mount(&server)
            .await;

        let url = Url::parse(&server.uri()).unwrap();
        let ethereum_rpc = EthereumRPC {
            url,
            client: Client::new(),
        };

        let block_numbers = vec!["0x1".to_string()];
        let result = ethereum_rpc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].is_none());
    }

    #[tokio::test]
    async fn test_ethereum_rpc_batch_payloads_invalid_block_data() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_json(json!([
                {
                    "jsonrpc": "2.0",
                    "method": "eth_getBlockByNumber",
                    "params": ["0x1", true],
                    "id": 0
                }
            ])))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "jsonrpc": "2.0",
                    "id": 0,
                    "result": {
                        "number": "0x1",
                        "hash": "invalid_hash",
                        "parentHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
                        "timestamp": "0x64"
                        // Missing many required fields
                    }
                }
            ])))
            .mount(&server)
            .await;

        let url = Url::parse(&server.uri()).unwrap();
        let ethereum_rpc = EthereumRPC {
            url,
            client: Client::new(),
        };

        let block_numbers = vec!["0x1".to_string()];
        let result = ethereum_rpc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].is_none());
    }

    #[tokio::test]
    async fn test_ethereum_rpc_batch_payloads_network_error() {
        // Use invalid URL to trigger network error
        let url = Url::parse("http://invalid-host:8545").unwrap();
        let ethereum_rpc = EthereumRPC {
            url,
            client: Client::new(),
        };

        let block_numbers = vec!["0x1".to_string()];
        let result = ethereum_rpc.get_execution_payloads(&block_numbers).await;

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Failed to send RPC batch request"));
    }

    #[tokio::test]
    async fn test_ethereum_rpc_batch_payloads_out_of_range_id() {
        let server = MockServer::start().await;

        let valid_block = create_test_block(
            1,
            "0x1234567890123456789012345678901234567890123456789012345678901234",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0x64,
            0,
        );

        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_json(json!([
                {
                    "jsonrpc": "2.0",
                    "method": "eth_getBlockByNumber",
                    "params": ["0x1", true],
                    "id": 0
                }
            ])))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "jsonrpc": "2.0",
                "id": 10, // Out of range
                "result": valid_block
            }
            ])))
            .mount(&server)
            .await;

        let url = Url::parse(&server.uri()).unwrap();
        let ethereum_rpc = EthereumRPC {
            url,
            client: Client::new(),
        };

        let block_numbers = vec!["0x1".to_string()];
        let result = ethereum_rpc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].is_none()); // id was out of range
    }
}
