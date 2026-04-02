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

use alloy_rpc_types_admin::PeerInfo;
use alloy_rpc_types_txpool::TxpoolStatus;
use backon::{ExponentialBuilder, Retryable};
use color_eyre::eyre::{self, Context, Result};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::Duration;
use tracing::{debug, trace};

pub(crate) mod valset_manager;
use crate::node::NodeName;
use crate::rpc::valset_manager::{public_key_to_hex, ContractValidator};
use crate::util;

pub(crate) use valset_manager::{ControllerInfo, Controllers};

#[derive(Clone)]
pub(crate) struct RpcClient {
    client: Client,
    url: Url,
    timeout: Duration,
}

impl RpcClient {
    pub(crate) fn new(url: Url, timeout: Duration) -> Self {
        let client = Client::new();
        Self {
            client,
            url,
            timeout,
        }
    }

    /// Send an HTTP JSON-RPC request with optional retries and return the raw response body.
    /// HTTP-level failures are propagated as `Err`; JSON-RPC errors are left in the response body.
    async fn send_raw(
        &self,
        method: &str,
        params: &serde_json::Value,
        max_retries: u32,
    ) -> Result<JsonResponseBody> {
        trace!(url=%self.url, %method, %params, %max_retries, "Sending RPC request");

        let request_body = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        }))?;

        let send_request = || {
            self.client
                .post(self.url.clone())
                .timeout(self.timeout)
                .header(CONTENT_TYPE, "application/json")
                .body(request_body.clone())
                .send()
        };

        let response = send_request
            .retry(ExponentialBuilder::default().with_max_times(max_retries as usize))
            .notify(|_, dur| {
                trace!(url=%self.url, %method, %params, "RPC request failed, retrying after {dur:?}...");
            })
            .await
            .wrap_err("Failed to send RPC request")?;

        let body: JsonResponseBody = response.error_for_status()?.json().await?;
        Ok(body)
    }

    /// Send an RPC request with retries, deserializing the result or returning a JSON-RPC error.
    pub(crate) async fn rpc_request<D: DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
        max_retries: u32,
    ) -> Result<D> {
        let body = self.send_raw(method, &params, max_retries).await?;

        if let Some(JsonError { code, message }) = body.error {
            Err(eyre::eyre!("Server Error {}: {}", code, message))
        } else {
            serde_json::from_value(body.result).map_err(Into::into)
        }
    }

    pub async fn get_latest_block_number_with_retries(&self, max_retries: u32) -> Result<u64> {
        let response = self
            .rpc_request::<String>("eth_blockNumber", json!([]), max_retries)
            .await?;
        let hex_str = response.strip_prefix("0x").unwrap_or(&response);
        let block_number = u64::from_str_radix(hex_str, 16)?;
        Ok(block_number)
    }

    pub async fn get_txpool_status(&self) -> Result<TxpoolStatus> {
        let response = self
            .rpc_request::<TxpoolStatus>("txpool_status", json!([]), 0)
            .await?;
        Ok(response)
    }

    /// Check if the node is syncing.
    /// Returns true if syncing, false if not syncing.
    pub async fn is_syncing(&self) -> Result<bool> {
        let response = self
            .rpc_request::<serde_json::Value>("eth_syncing", json!([]), 0)
            .await?;
        if response.is_object() {
            // A JSON object indicates the node is syncing.
            Ok(true)
        } else if let Some(is_syncing_bool) = response.as_bool() {
            // A boolean `false` means not syncing. The spec does not mention `true`.
            Ok(is_syncing_bool)
        } else {
            // Any other type is unexpected, but we can conservatively assume it means syncing.
            Ok(true)
        }
    }

    /// Get the list of connected peers from the node via admin_peers RPC.
    pub async fn get_peers(&self) -> Result<Vec<PeerInfo>> {
        let response = self
            .rpc_request::<Vec<PeerInfo>>("admin_peers", json!([]), 0)
            .await?;
        Ok(response)
    }

    /// Sends a transaction to the PermissionedValidatorManager smart contract to
    /// update the voting power of the specified validator to the specified value.
    /// controllers_config_dir is the directory containing the controllers
    /// configuration file.
    pub(crate) async fn update_validator_voting_power(
        &self,
        controller: &mut ControllerInfo,
        voting_power: u64,
    ) -> Result<()> {
        let raw_tx = valset_manager::build_validator_update_tx(controller, voting_power)
            .await
            .wrap_err("failed to build validator update transaction")?;

        controller.nonce += 1;

        let response = self
            .rpc_request::<String>("eth_sendRawTransaction", json!([raw_tx]), 0)
            .await
            .wrap_err("failed to broadcast transaction")?;

        debug!(
            "Broadcasted transaction to update validator {} voting power to {}. Tx Hash: {}",
            controller.index, voting_power, response
        );

        Ok(())
    }

    /// Get the transaction count (nonce) for the given address.
    pub async fn get_transaction_count(&self, address: &str) -> Result<u64> {
        let response = self
            .rpc_request::<String>("eth_getTransactionCount", json!([address, "pending"]), 0)
            .await?;
        let hex_str = response.strip_prefix("0x").unwrap_or(&response);
        let nonce = u64::from_str_radix(hex_str, 16)?;
        Ok(nonce)
    }

    /// Send a raw signed transaction and return the transaction hash.
    pub async fn send_raw_transaction(&self, raw_tx: &str) -> Result<String> {
        self.rpc_request::<String>("eth_sendRawTransaction", json!([raw_tx]), 0)
            .await
    }

    /// Get the transaction receipt for the given transaction hash.
    /// Returns `None` if the transaction has not been mined yet.
    pub async fn get_transaction_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<Option<serde_json::Value>> {
        let response = self
            .rpc_request::<serde_json::Value>("eth_getTransactionReceipt", json!([tx_hash]), 0)
            .await?;
        if response.is_null() {
            Ok(None)
        } else {
            Ok(Some(response))
        }
    }

    // queries the getValidator() function in the validator registry smart contract.
    pub(crate) async fn get_validator(
        &self,
        controller: ControllerInfo,
    ) -> Result<ContractValidator> {
        let params = valset_manager::get_validator_call_params(&controller.address);
        let result = self
            .rpc_request::<String>("eth_call", params, 0)
            .await
            .wrap_err("failed to call getValidator() to validator registry")?;

        let validator = valset_manager::get_validator_response_decode(&result)?;

        Ok(validator)
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JsonResponseBody {
    pub jsonrpc: String,
    #[serde(default)]
    pub error: Option<JsonError>,
    #[serde(default)]
    pub result: serde_json::Value,
    pub id: serde_json::Value,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct JsonError {
    pub code: i64,
    pub message: String,
}

/// Fetch in parallel the latest height of each node
pub async fn fetch_latest_heights(node_urls: &[(NodeName, Url)]) -> Vec<(NodeName, Result<u64>)> {
    util::in_parallel_tuples(node_urls, |name, url| async move {
        let client = RpcClient::new(url, Duration::from_secs(1));
        let result = client.get_latest_block_number_with_retries(0).await;
        (name, result)
    })
    .await
}

/// Fetch in parallel the mempool status (number of pending and queued transactions) of each node
pub(crate) async fn fetch_mempool_status(
    node_urls: &[(NodeName, Url)],
) -> Vec<(NodeName, (i64, i64))> {
    util::in_parallel_tuples(node_urls, |name, url| async move {
        let client = RpcClient::new(url, Duration::from_secs(1));
        let status: (i64, i64) = client
            .get_txpool_status()
            .await
            .map(|s| (s.pending as i64, s.queued as i64))
            .unwrap_or((-1, -1));
        (name, status)
    })
    .await
}

/// Fetch in parallel the keys, addresses, and controller addresses of each node
pub(crate) async fn fetch_node_keys(
    node_urls: &[(String, Url)],
    controllers: &Controllers,
) -> Vec<(NodeName, (Result<String>, Result<String>, Result<String>))> {
    let controllers = controllers.clone();
    util::in_parallel_tuples(node_urls, move |node: NodeName, url: Url| {
        let controllers = controllers.clone();
        async move {
            let client = RpcClient::new(url, Duration::from_secs(2));
            let info = match controllers.load_controller(&node) {
                Ok(controller) => {
                    let ctrl_addr = controller.eth_address().to_string();
                    match client.get_validator(controller).await {
                        Ok(val) => {
                            let address = val.address().map(|a| a.to_string());
                            let public_key = val.public_key().map(|key| public_key_to_hex(&key));
                            (Ok(ctrl_addr), public_key, address)
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            (Ok(ctrl_addr), Err(e), Err(eyre::eyre!(msg)))
                        }
                    }
                }
                Err(_) => (
                    Err(eyre::eyre!("n/a")),
                    Err(eyre::eyre!("n/a")),
                    Err(eyre::eyre!("n/a")),
                ),
            };
            (node, info)
        }
    })
    .await
}

/// Fetch in parallel the peers of each node
pub(crate) async fn fetch_peers_info(
    node_urls: &[(NodeName, Url)],
) -> Vec<(NodeName, Vec<PeerInfo>)> {
    util::in_parallel_tuples(node_urls, |name, url| async move {
        let client = RpcClient::new(url, Duration::from_secs(5));
        let peers = client.get_peers().await.unwrap_or_default();
        (name, peers)
    })
    .await
}

/// Fetch in parallel the latest data of each node (height, peers, contract validator)
pub(crate) async fn fetch_latest_data(
    node_urls: &[(NodeName, Url)],
    controllers: &Controllers,
) -> Vec<(
    NodeName,
    (
        Result<String>,
        Result<Vec<PeerInfo>>,
        Result<ContractValidator>,
    ),
)> {
    let controllers = controllers.clone();
    util::in_parallel_tuples(node_urls, move |node, url| {
        let controllers = controllers.clone();
        async move {
            let client = RpcClient::new(url, Duration::from_secs(1));

            let height = client
                .get_latest_block_number_with_retries(0)
                .await
                .map(|h| h.to_string());

            let peers = client.get_peers().await;

            let contract_validator = match controllers.load_controller(&node) {
                Ok(controller) => client.get_validator(controller).await,
                Err(_) => Err(eyre::eyre!("n/a")),
            };

            (node.to_string(), (height, peers, contract_validator))
        }
    })
    .await
}
