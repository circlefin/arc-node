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
use alloy_rpc_types::{Block, BlockNumberOrTag, TransactionRequest};
use alloy_rpc_types_engine::ExecutionPayloadV3;
use alloy_rpc_types_txpool::{TxpoolInspect, TxpoolStatus};
use alloy_sol_types::SolCall;
use async_trait::async_trait;
use backon::BackoffBuilder;
use eyre::Context;
use jsonrpsee::core::traits::ToRpcParams;
use jsonrpsee::rpc_params;
use serde::de::DeserializeOwned;
use tracing::{debug, trace};

use arc_consensus_types::{ConsensusParams, ValidatorSet};

use crate::abi_utils::{
    abi_decode_consensus_params, abi_decode_validator_set, consensusParamsCall,
    getActiveValidatorSetCall,
};
use crate::constants::{
    ETH_BATCH_REQUEST_TIMEOUT, ETH_CALL_RETRY, ETH_DEFAULT_TIMEOUT, IPC_CLIENT_TIMEOUT,
    PROTOCOL_CONFIG_ADDRESS, VALIDATOR_REGISTRY_ADDRESS,
};
use crate::engine::EthereumAPI;
use crate::ipc::ipc_builder::Ipc;
use crate::json_structures::*;
use crate::retry::NoRetry;

/// Generate ABI parameters for IPC calls to get active validator set
fn abi_get_active_validator_set_params_ipc(
    block_height: u64,
) -> eyre::Result<(TransactionRequest, BlockNumberOrTag)> {
    // Encode the 4‑byte selector + (no args)
    let calldata = getActiveValidatorSetCall {}.abi_encode();

    let tx_request = TransactionRequest {
        to: Some(alloy_primitives::TxKind::Call(VALIDATOR_REGISTRY_ADDRESS)),
        input: alloy_primitives::Bytes::from(calldata).into(),
        ..Default::default()
    };

    let block_number = BlockNumberOrTag::Number(block_height);

    Ok((tx_request, block_number))
}

/// Generate ABI parameters for IPC calls to get consensus params
fn abi_get_consensus_params_params_ipc(
    block_height: u64,
) -> eyre::Result<(TransactionRequest, BlockNumberOrTag)> {
    // Encode the 4‑byte selector + (no args)
    let calldata = consensusParamsCall {}.abi_encode();

    let tx_request = TransactionRequest {
        to: Some(alloy_primitives::TxKind::Call(PROTOCOL_CONFIG_ADDRESS)),
        input: alloy_primitives::Bytes::from(calldata).into(),
        ..Default::default()
    };

    let block_number = BlockNumberOrTag::Number(block_height);

    Ok((tx_request, block_number))
}

/// IPC client for Ethereum server.
pub struct EthereumIPC {
    ipc: Ipc,
}

impl std::fmt::Display for EthereumIPC {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EthereumIPC:{}", self.ipc.socket_path())
    }
}

impl EthereumIPC {
    /// Create a new `EthereumIPC` struct given the IPC socket path.
    pub async fn new(socket_path: &str) -> eyre::Result<Self> {
        Self::new_with_timeout(socket_path, IPC_CLIENT_TIMEOUT).await
    }

    /// Create a new `EthereumIPC` struct with custom timeout.
    pub async fn new_with_timeout(socket_path: &str, timeout: Duration) -> eyre::Result<Self> {
        let ipc = Ipc::new_with_timeout(socket_path, timeout).await?;
        Ok(Self { ipc })
    }

    /// Send an RPC request to the Ethereum RPC endpoint via IPC.
    pub async fn rpc_request<D: DeserializeOwned>(
        &self,
        method: &str,
        params: impl ToRpcParams + Clone + Send,
        timeout: Duration,
    ) -> eyre::Result<D> {
        self.ipc.rpc_request(method, params, timeout, NoRetry).await
    }

    /// Get the eth1 chain id of the given endpoint.
    pub async fn get_chain_id(&self) -> eyre::Result<String> {
        self.ipc
            .build_rpc_request("eth_chainId", rpc_params!())
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
                rpc_params!("0x0", false),
                ETH_DEFAULT_TIMEOUT,
            )
            .await?;

        block.ok_or_else(|| eyre::eyre!("Genesis block not found"))
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_active_validator_set(&self, block_height: u64) -> eyre::Result<ValidatorSet> {
        let params = abi_get_active_validator_set_params_ipc(block_height)?;
        debug!("eth_call params: {params:?}",);

        // Extract the transaction object and block number from the params tuple
        let (tx_request, block_number) = params;

        let result: String = self
            .ipc
            .build_rpc_request("eth_call", rpc_params!(tx_request, block_number))
            .timeout(ETH_DEFAULT_TIMEOUT)
            .retry(ETH_CALL_RETRY.build())
            .send()
            .await?;

        trace!("eth_call result: {result}");
        let result = hex::decode(result.trim_start_matches("0x"))?;

        abi_decode_validator_set(result)
    }

    #[tracing::instrument(skip(self))]
    pub async fn get_consensus_params(&self, block_height: u64) -> eyre::Result<ConsensusParams> {
        let params = abi_get_consensus_params_params_ipc(block_height)?;
        debug!("eth_call params: {params:?}",);

        // Extract the transaction object and block number from the params tuple
        let (tx_request, block_number) = params;

        let result: String = self
            .ipc
            .build_rpc_request("eth_call", rpc_params!(tx_request, block_number))
            .timeout(ETH_DEFAULT_TIMEOUT)
            .retry(ETH_CALL_RETRY.build())
            .send()
            .await?;

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
        self.rpc_request(
            "eth_getBlockByNumber",
            rpc_params!(block_number, return_full_transaction_objects),
            ETH_DEFAULT_TIMEOUT,
        )
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
        debug!("EthereumIPC: get_execution_payloads for block_numbers={block_numbers:?}");

        let return_full_transaction_objects = true;
        let params_list = block_numbers
            .iter()
            .map(|block_number| rpc_params![block_number, return_full_transaction_objects])
            .collect::<Vec<_>>();
        let batch_blocks: Vec<Option<Option<Block>>> = self
            .ipc
            .batch_request(
                "eth_getBlockByNumber",
                &params_list,
                ETH_BATCH_REQUEST_TIMEOUT,
            )
            .await
            .wrap_err("Failed to send IPC batch request")?;

        let mut results = Vec::with_capacity(block_numbers.len());
        for (idx, block) in batch_blocks.into_iter().enumerate() {
            match block {
                Some(Some(b)) => {
                    let block_hash = b.header.hash;
                    let consensus_block = b.into_consensus().convert_transactions::<TxEnvelope>();
                    let execution_payload =
                        ExecutionPayloadV3::from_block_unchecked(block_hash, &consensus_block);
                    results.push(Some(execution_payload));
                }
                Some(None) => {
                    debug!(idx=%idx, block_number=%block_numbers[idx], "No block found for request");
                    results.push(None);
                }
                None => {
                    debug!(idx=%idx, block_number=%block_numbers[idx], "Request failed for block");
                    results.push(None);
                }
            }
        }

        Ok(results)
    }

    /// Get the status of the transaction pool.
    pub async fn txpool_status(&self) -> eyre::Result<TxpoolStatus> {
        self.rpc_request("txpool_status", rpc_params!(), ETH_DEFAULT_TIMEOUT)
            .await
    }

    /// Get the contents of the transaction pool.
    pub async fn txpool_inspect(&self) -> eyre::Result<TxpoolInspect> {
        self.rpc_request("txpool_inspect", rpc_params!(), ETH_DEFAULT_TIMEOUT)
            .await
    }
}

#[async_trait]
impl EthereumAPI for EthereumIPC {
    /// Get the eth1 chain id of the given endpoint.
    async fn get_chain_id(&self) -> eyre::Result<String> {
        self.get_chain_id()
            .await
            .wrap_err("EthereumIPC get_chain_id call failed")
    }

    /// Get the genesis block.
    async fn get_genesis_block(&self) -> eyre::Result<ExecutionBlock> {
        self.get_genesis_block()
            .await
            .wrap_err("EthereumIPC get_genesis_block call failed")
    }

    /// Get the active validator set at a specific block height.
    async fn get_active_validator_set(&self, block_height: u64) -> eyre::Result<ValidatorSet> {
        self.get_active_validator_set(block_height)
            .await
            .wrap_err_with(|| {
                format!(
                    "EthereumIPC get_active_validator_set call failed for height={block_height}"
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
                    "EthereumIPC get_block_by_number call failed for block number={block_number}"
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
                format!("EthereumIPC get_execution_payloads call failed for block_numbers={block_numbers:?}")
            })
    }

    /// Get the status of the transaction pool.
    async fn txpool_status(&self) -> eyre::Result<TxpoolStatus> {
        self.txpool_status().await
    }

    /// Get the contents of the transaction pool.
    async fn txpool_inspect(&self) -> eyre::Result<TxpoolInspect> {
        self.txpool_inspect().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::Header as ConsensusHeader;
    use alloy_primitives::B256;
    use alloy_rpc_types_eth::{Block, Header};
    use std::{collections::HashMap, sync::Arc};
    use tempfile;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::task::JoinHandle;
    use tokio::time::{sleep, Duration};

    #[test]
    fn test_encode_get_valset_params_abi_ipc() {
        let block_number = 4567;
        let (tx_request, block_tag) =
            abi_get_active_validator_set_params_ipc(block_number).unwrap();

        // Check the transaction request has the expected address
        if let Some(alloy_primitives::TxKind::Call(address)) = tx_request.to {
            assert_eq!(address, VALIDATOR_REGISTRY_ADDRESS);
        } else {
            panic!("Expected Call transaction type");
        }

        // Check the block number
        assert_eq!(block_tag, BlockNumberOrTag::Number(block_number));
    }

    #[test]
    fn test_encode_get_consensus_params_abi_ipc() {
        let block_number = 4567;
        let (tx_request, block_tag) = abi_get_consensus_params_params_ipc(block_number).unwrap();

        // Check the transaction request has the expected address
        if let Some(alloy_primitives::TxKind::Call(address)) = tx_request.to {
            assert_eq!(address, PROTOCOL_CONFIG_ADDRESS);
        } else {
            panic!("Expected Call transaction type");
        }

        // Check the block number
        assert_eq!(block_tag, BlockNumberOrTag::Number(block_number));
    }

    fn create_test_block(
        number: u64,
        hash: &str,
        parent_hash: &str,
        timestamp: u64,
        gas_used: u64,
    ) -> Block {
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
        Block {
            header,
            ..Default::default()
        }
    }

    async fn start_mock_ipc_server(
        responses: HashMap<String, Option<Block>>,
    ) -> eyre::Result<(String, MockServerHandle)> {
        let temp_dir = tempfile::tempdir()?;
        let socket_path = temp_dir.path().join("mock_ipc.sock");
        let socket_path = socket_path.to_string_lossy().to_string();

        let _ = std::fs::remove_file(&socket_path); // Just in case

        let listener = UnixListener::bind(&socket_path)?;
        let responses = Arc::new(responses);
        let server_jh = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let responses = responses.clone();
                        tokio::spawn(async move { handle_ipc_connection(stream, responses).await });
                    }
                    Err(e) => {
                        eprintln!("Failed to accept connection: {}", e);
                        break;
                    }
                }
            }
        });

        let handle = MockServerHandle {
            _temp_dir_guard: temp_dir,
            _server_jh: server_jh,
        };

        // Give the server time to breathe
        sleep(Duration::from_millis(50)).await;

        Ok((socket_path, handle))
    }

    struct MockServerHandle {
        _temp_dir_guard: tempfile::TempDir,
        _server_jh: JoinHandle<()>,
    }

    async fn handle_ipc_connection(
        stream: UnixStream,
        responses: Arc<HashMap<String, Option<Block>>>,
    ) {
        // Big buffer not to complicate this test infra with framing
        let mut buffer = vec![0; 64 * 1024];
        let (mut reader, mut writer) = stream.into_split();

        while let Ok(n) = reader.read(&mut buffer).await {
            if n == 0 {
                break;
            }

            let request_str = String::from_utf8_lossy(&buffer[..n]);
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&request_str) else {
                continue;
            };

            let response = if value.is_array() {
                // Handle batch request
                let batch_request = value.as_array().unwrap();
                let mut batch_response = Vec::new();

                for request in batch_request {
                    let response = handle_single_request(request, &responses);
                    batch_response.push(response);
                }
                // Simulate out-of-order responses
                batch_response.reverse();

                serde_json::Value::Array(batch_response)
            } else {
                // Handle single request
                handle_single_request(&value, &responses)
            };

            let response_str = serde_json::to_string(&response).unwrap() + "\n";

            writer.write_all(response_str.as_bytes()).await.unwrap();
            writer.flush().await.unwrap();
        }
    }

    fn handle_single_request(
        request: &serde_json::Value,
        responses: &HashMap<String, Option<Block>>,
    ) -> serde_json::Value {
        let Some(method) = request.get("method").and_then(|m| m.as_str()) else {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {"code": -32600, "message": "Invalid Request"}
            });
        };
        if method != "eth_getBlockByNumber" {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": request.get("id").unwrap_or(&serde_json::Value::Null),
                "error": {"code": -32601, "message": "Method not found"}
            });
        };
        let Some(params) = request.get("params").and_then(|p| p.as_array()) else {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": request.get("id").unwrap_or(&serde_json::Value::Null),
                "error": {"code": -32602, "message": "Invalid params"}
            });
        };
        if params.is_empty() {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": request.get("id").unwrap_or(&serde_json::Value::Null),
                "error": {"code": -32602, "message": "Invalid params"}
            });
        };
        let Some(block_number) = params[0].as_str() else {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": request.get("id").unwrap_or(&serde_json::Value::Null),
                "error": {"code": -32602, "message": "Invalid params"}
            });
        };
        let Some(result) = responses.get(block_number) else {
            // Block number not found in our test data - return null
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": request.get("id").unwrap_or(&serde_json::Value::Null),
                "result": null
            });
        };
        let result = result.to_owned();
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": request.get("id").unwrap_or(&serde_json::Value::Null),
            "result": result
        })
    }

    #[tokio::test]
    async fn test_ethereum_ipc_get_genesis_block_success() {
        let genesis_hash = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let genesis_block = create_test_block(
            0,
            genesis_hash,
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0,
            0,
        );

        let mut responses = HashMap::new();
        responses.insert("0x0".to_string(), Some(genesis_block));

        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let result = ethereum_ipc.get_genesis_block().await.unwrap();
        assert_eq!(result.block_hash, genesis_hash.parse::<B256>().unwrap());
        assert_eq!(result.timestamp, 0);
    }

    #[tokio::test]
    async fn test_ethereum_ipc_get_genesis_block_not_found() {
        let responses = HashMap::new(); // No blocks available

        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let err = ethereum_ipc.get_genesis_block().await.unwrap_err();
        assert!(err.to_string().contains("Genesis block not found"));
    }

    #[tokio::test]
    async fn test_ethereum_ipc_batch_payloads_success() {
        let block1 = create_test_block(
            1,
            "0x1234567890123456789012345678901234567890123456789012345678901234",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0x64,
            0,
        );
        let block2 = create_test_block(
            2,
            "0x2345678901234567890123456789012345678901234567890123456789012345",
            "0x1234567890123456789012345678901234567890123456789012345678901234",
            0xc8,
            26000,
        );

        let mut responses = HashMap::new();
        responses.insert("0x1".to_string(), Some(block1));
        responses.insert("0x2".to_string(), Some(block2));

        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let block_numbers = vec!["0x1".to_string(), "0x2".to_string()];
        let result = ethereum_ipc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert!(result[0].is_some());
        assert!(result[1].is_some());

        let payload1 = result[0].as_ref().unwrap();
        let payload2 = result[1].as_ref().unwrap();

        assert_eq!(payload1.payload_inner.payload_inner.block_number, 1);
        assert_eq!(payload1.payload_inner.payload_inner.gas_used, 0);
        assert_eq!(payload2.payload_inner.payload_inner.block_number, 2);
        assert_eq!(payload2.payload_inner.payload_inner.gas_used, 26000);
    }

    #[tokio::test]
    async fn test_ethereum_ipc_batch_payloads_partial_failure() {
        let block1 = create_test_block(
            1,
            "0x1234567890123456789012345678901234567890123456789012345678901234",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0x64,
            33344,
        );

        let mut responses = HashMap::new();
        responses.insert("0x1".to_string(), Some(block1));
        responses.insert("0x999".to_string(), None); // Missing block

        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let block_numbers = vec!["0x1".to_string(), "0x999".to_string()];
        let result = ethereum_ipc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert!(result[0].is_some());
        assert!(result[1].is_none());

        let payload = result[0].as_ref().unwrap();
        assert_eq!(payload.payload_inner.payload_inner.block_number, 1);
        assert_eq!(payload.payload_inner.payload_inner.gas_used, 33344);
    }

    #[tokio::test]
    async fn test_ethereum_ipc_batch_payloads_empty_request() {
        let responses = HashMap::new();
        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let result = ethereum_ipc.get_execution_payloads(&[]).await.unwrap();
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn test_ethereum_ipc_batch_payloads_all_missing() {
        let responses = HashMap::new(); // No blocks available

        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let block_numbers = vec!["0x1".to_string(), "0x2".to_string()];
        let result = ethereum_ipc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 2);
        assert!(result[0].is_none());
        assert!(result[1].is_none());
    }

    #[tokio::test]
    async fn test_ethereum_ipc_batch_payloads_connection_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("non_existent_socket.sock");
        let socket_path = socket_path.to_string_lossy().to_string();

        let result = EthereumIPC::new(&socket_path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_ethereum_ipc_batch_payloads_single_block() {
        let block1 = create_test_block(
            1,
            "0x1234567890123456789012345678901234567890123456789012345678901234",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0x64,
            12345,
        );

        let mut responses = HashMap::new();
        responses.insert("0x1".to_string(), Some(block1));

        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let block_numbers = vec!["0x1".to_string()];
        let result = ethereum_ipc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].is_some());

        let payload = result[0].as_ref().unwrap();
        assert_eq!(payload.payload_inner.payload_inner.block_number, 1);
        assert_eq!(payload.payload_inner.payload_inner.gas_used, 12345);
    }

    #[tokio::test]
    async fn test_ethereum_ipc_batch_payloads_response_length_validation() {
        let block1 = create_test_block(
            1,
            "0x1234567890123456789012345678901234567890123456789012345678901234",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0x64,
            12345,
        );
        let mut responses = HashMap::new();
        responses.insert("0x1".to_string(), Some(block1));

        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let block_numbers = vec!["0x1".to_string()];
        let result = ethereum_ipc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].is_some());
    }

    #[tokio::test]
    async fn test_ethereum_ipc_batch_payloads_invalid_block_data() {
        let mut responses = HashMap::new();
        // None simulates invalid/malformed block data
        responses.insert("0x1".to_string(), None);

        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let block_numbers = vec!["0x1".to_string()];
        let result = ethereum_ipc
            .get_execution_payloads(&block_numbers)
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert!(result[0].is_none());
    }

    #[tokio::test]
    async fn test_ethereum_ipc_batch_payloads_mixed_success_and_errors() {
        let mut responses = HashMap::new();

        // Block 0x1: Success
        let block1 = create_test_block(
            1,
            "0x1111111111111111111111111111111111111111111111111111111111111111",
            "0x0000000000000000000000000000000000000000000000000000000000000000",
            0x64,
            12345,
        );
        responses.insert("0x1".to_string(), Some(block1));

        // Block 0x2: No response (simulates JSON-RPC error)

        // Block 0x3: Success
        let block3 = create_test_block(
            3,
            "0x3333333333333333333333333333333333333333333333333333333333333333",
            "0x2222222222222222222222222222222222222222222222222222222222222222",
            0xc8,
            26000,
        );
        responses.insert("0x3".to_string(), Some(block3));

        // Block 0x4: Block not found
        responses.insert("0x4".to_string(), None);

        let (socket_path, _handle) = start_mock_ipc_server(responses).await.unwrap();
        let ethereum_ipc = EthereumIPC::new(&socket_path).await.unwrap();

        let block_numbers = vec![
            "0x1".to_string(),
            "0x2".to_string(),
            "0x3".to_string(),
            "0x4".to_string(),
        ];
        let result = ethereum_ipc.get_execution_payloads(&block_numbers).await;

        assert!(
            result.is_ok(),
            "The batch should succeed even though some requests failed"
        );
        let payloads = result.unwrap();
        assert_eq!(payloads.len(), 4);

        assert!(payloads[0].is_some(), "Block 1 should succeed");
        assert_eq!(
            payloads[0]
                .as_ref()
                .unwrap()
                .payload_inner
                .payload_inner
                .block_number,
            1
        );

        assert!(
            payloads[1].is_none(),
            "Block 2 should be None (request failed/no response)"
        );

        assert!(payloads[2].is_some(), "Block 3 should succeed");
        assert_eq!(
            payloads[2]
                .as_ref()
                .unwrap()
                .payload_inner
                .payload_inner
                .block_number,
            3
        );

        assert!(
            payloads[3].is_none(),
            "Block 4 should be None (block not found)"
        );
    }
}
