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

//! Reusable RPC query helpers for historical blockchain data.

use color_eyre::eyre::{ensure, Result, WrapErr};
use reqwest::Url;
use serde_json::json;
use tracing::info;

use super::RpcClientFactory;

/// Fetch a block by number with full transactions and verify it exists.
pub(crate) async fn get_block_with_txs(
    factory: &RpcClientFactory,
    url: &Url,
    block: u64,
) -> Result<serde_json::Value> {
    let client = factory.create(url.clone());
    let hex_block = format!("0x{block:x}");
    let result: serde_json::Value = client
        .rpc_request("eth_getBlockByNumber", json!([&hex_block, true]), 0)
        .await
        .wrap_err_with(|| format!("eth_getBlockByNumber({hex_block}) failed"))?;
    ensure!(!result.is_null(), "Block at {hex_block} is null");
    info!("✅ eth_getBlockByNumber({hex_block}): OK");
    Ok(result)
}

/// Fetch an account balance at a given block height.
pub(crate) async fn get_balance(
    factory: &RpcClientFactory,
    url: &Url,
    address: &str,
    block: u64,
) -> Result<String> {
    let client = factory.create(url.clone());
    let hex_block = format!("0x{block:x}");
    let balance: String = client
        .rpc_request("eth_getBalance", json!([address, &hex_block]), 0)
        .await
        .wrap_err_with(|| format!("eth_getBalance({address}, {hex_block}) failed"))?;
    info!("✅ eth_getBalance({address}, {hex_block}): {balance}");
    Ok(balance)
}

/// Fetch an account balance at "latest".
pub(crate) async fn get_balance_latest(
    factory: &RpcClientFactory,
    url: &Url,
    address: &str,
) -> Result<String> {
    let client = factory.create(url.clone());
    let balance: String = client
        .rpc_request("eth_getBalance", json!([address, "latest"]), 0)
        .await
        .wrap_err_with(|| format!("eth_getBalance({address}, latest) failed"))?;
    info!("✅ eth_getBalance({address}, latest): {balance}");
    Ok(balance)
}

/// Fetch logs in a block range.
pub(crate) async fn get_logs(
    factory: &RpcClientFactory,
    url: &Url,
    from_block: u64,
    to_block: u64,
) -> Result<Vec<serde_json::Value>> {
    let client = factory.create(url.clone());
    let hex_from = format!("0x{from_block:x}");
    let hex_to = format!("0x{to_block:x}");
    let logs: Vec<serde_json::Value> = client
        .rpc_request(
            "eth_getLogs",
            json!([{"fromBlock": &hex_from, "toBlock": &hex_to}]),
            0,
        )
        .await
        .wrap_err_with(|| format!("eth_getLogs({hex_from}..{hex_to}) failed"))?;
    info!("✅ eth_getLogs({hex_from}..{hex_to}): {} logs", logs.len());
    Ok(logs)
}
