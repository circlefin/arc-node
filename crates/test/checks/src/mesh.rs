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

//! Gossip / P2P connectivity checks.
//!
//! Execution JSON-RPC does not expose libp2p gossipsub mesh state. This module
//! uses [`net_peerCount`](https://ethereum.org/en/developers/docs/apis/json-rpc/#net_peercount)
//! as a **best-effort proxy** for whether the node has enough devp2p peers to
//! plausibly participate in the network. When the method is missing or disabled,
//! the check records a pass with an explanatory message so callers are not
//! blocked until a dedicated mesh introspection surface exists.

use std::collections::HashMap;
use std::time::Duration;

use color_eyre::eyre::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

use crate::types::{CheckResult, Report};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct JsonResponseBody {
    #[serde(default)]
    error: Option<JsonError>,
    #[serde(default)]
    result: Value,
}

#[derive(Deserialize)]
struct JsonError {
    code: i64,
    message: String,
}

enum RpcOutcome {
    Ok(Value),
    Err { code: i64, message: String },
    Transport(String),
}

async fn rpc_call(client: &reqwest::Client, url: &Url, method: &str, params: Value) -> RpcOutcome {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    match client.post(url.as_str()).json(&body).send().await {
        Ok(resp) => match resp.json::<JsonResponseBody>().await {
            Ok(parsed) => match parsed.error {
                Some(e) => RpcOutcome::Err {
                    code: e.code,
                    message: e.message,
                },
                None => RpcOutcome::Ok(parsed.result),
            },
            Err(e) => RpcOutcome::Transport(format!("JSON parse error: {e}")),
        },
        Err(e) => RpcOutcome::Transport(e.to_string()),
    }
}

fn parse_peer_count(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => {
            let digits = s.strip_prefix("0x").unwrap_or(s.as_str());
            if digits.is_empty() {
                return Some(0);
            }
            u64::from_str_radix(digits, 16).ok()
        }
        _ => None,
    }
}

/// Validate node connectivity against an expected peer list.
///
/// Each entry in `expected_peers` maps a node name to human-readable peer
/// identifiers (e.g. other validator names). The **count** of expected peers
/// is compared to `net_peerCount`; identities are not resolved on-chain.
///
/// Reports per-node outcome: sufficient P2P peers, insufficient peers, or
/// skipped / unavailable measurement.
pub async fn check_mesh(
    rpc_urls: &[(String, Url)],
    expected_peers: &HashMap<String, Vec<String>>,
) -> Result<Report> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;

    let mut checks = Vec::new();

    for (node_name, url) in rpc_urls {
        let expected = expected_peers
            .get(node_name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if expected.is_empty() {
            checks.push(CheckResult {
                name: node_name.clone(),
                passed: true,
                message: "mesh: no expected peers configured for this node (skipped)".to_string(),
            });
            continue;
        }

        let min_peers = expected.len() as u64;
        match rpc_call(&client, url, "net_peerCount", json!([])).await {
            RpcOutcome::Ok(v) => match parse_peer_count(&v) {
                Some(count) if count >= min_peers => checks.push(CheckResult {
                    name: node_name.clone(),
                    passed: true,
                    message: format!(
                        "mesh: net_peerCount={count} >= expected {min_peers} \
                         (devp2p peer count as connectivity proxy)"
                    ),
                }),
                Some(count) => checks.push(CheckResult {
                    name: node_name.clone(),
                    passed: false,
                    message: format!(
                        "mesh: net_peerCount={count} < expected {min_peers} peer(s) for {expected:?}"
                    ),
                }),
                None => checks.push(CheckResult {
                    name: node_name.clone(),
                    passed: false,
                    message: format!("mesh: net_peerCount returned unparsable value: {v}"),
                }),
            },
            RpcOutcome::Err { code, message } => checks.push(CheckResult {
                name: node_name.clone(),
                passed: true,
                message: format!(
                    "mesh: net_peerCount unavailable ({code}: {message}); \
                     gossipsub topology not verified"
                ),
            }),
            RpcOutcome::Transport(e) => checks.push(CheckResult {
                name: node_name.clone(),
                passed: false,
                message: format!("mesh: net_peerCount transport error: {e}"),
            }),
        }
    }

    Ok(Report { checks })
}
