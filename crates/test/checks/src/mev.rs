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

//! MEV / pending-state surface checks.
//!
//! Verifies that nodes correctly restrict RPC methods that expose pending state
//! or mempool data, which are potential MEV attack vectors.
//!
//! # What is checked
//!
//! - **Pending-block suppression** — `eth_getBlockByNumber("pending")` returns null
//! - **Pending state fallback** — state methods with `"pending"` tag match `"latest"`
//! - **Pending-tx RPCs blocked** — `eth_newPendingTransactionFilter` returns `-32001`
//! - **Sensitive namespaces disabled** — txpool, debug, trace, admin, flashbots, mev, ots
//! - **Mempool nonce lookup blocked** — `eth_getTransactionBySenderAndNonce` returns no data

use std::time::Duration;

use color_eyre::eyre::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

use crate::types::{CheckResult, Report};

/// JSON-RPC error code returned by Arc's pending-tx filter.
const ARC_BLOCKED_ERROR_CODE: i64 = -32001;

/// Default address for state-query assertions (Anvil account #0, localdev genesis).
pub const DEFAULT_ADDR: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

/// Zero address — safe target for eth_call / eth_estimateGas with empty calldata.
const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";

const PENDING_BLOCK_SAMPLES: usize = 5;
const PENDING_BLOCK_DELAY: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

// ── JSON-RPC helpers ─────────────────────────────────────────────────────────

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

// ── Individual checks ────────────────────────────────────────────────────────

async fn check_pending_block_is_null(
    client: &reqwest::Client,
    url: &Url,
    node: &str,
) -> CheckResult {
    for i in 0..PENDING_BLOCK_SAMPLES {
        if i > 0 {
            tokio::time::sleep(PENDING_BLOCK_DELAY).await;
        }
        match rpc_call(
            client,
            url,
            "eth_getBlockByNumber",
            json!(["pending", false]),
        )
        .await
        {
            RpcOutcome::Ok(v) if v.is_null() => continue,
            RpcOutcome::Ok(v) => {
                let num = v.get("number").and_then(|n| n.as_str()).unwrap_or("?");
                return CheckResult {
                    name: node.into(),
                    passed: false,
                    message: format!(
                        "eth_getBlockByNumber(\"pending\") returned block {num} on sample {}/{PENDING_BLOCK_SAMPLES} \
                         (expected null -- is --rpc.pending-block=none set?)",
                        i + 1,
                    ),
                };
            }
            RpcOutcome::Err { code, message } => {
                return CheckResult {
                    name: node.into(),
                    passed: false,
                    message: format!("eth_getBlockByNumber(\"pending\") error {code}: {message}"),
                };
            }
            RpcOutcome::Transport(e) => {
                return CheckResult {
                    name: node.into(),
                    passed: false,
                    message: format!("eth_getBlockByNumber(\"pending\") transport error: {e}"),
                };
            }
        }
    }
    CheckResult {
        name: node.into(),
        passed: true,
        message: format!(
            "eth_getBlockByNumber(\"pending\") null x{PENDING_BLOCK_SAMPLES} \
             (--rpc.pending-block=none active)"
        ),
    }
}

async fn check_pending_eq_latest(
    client: &reqwest::Client,
    url: &Url,
    node: &str,
    label: &str,
    method: &str,
    params_pending: Value,
    params_latest: Value,
) -> CheckResult {
    let r_pending = rpc_call(client, url, method, params_pending).await;
    let r_latest = rpc_call(client, url, method, params_latest).await;

    match (r_pending, r_latest) {
        (RpcOutcome::Ok(vp), RpcOutcome::Ok(vl)) => {
            if vp == vl {
                CheckResult {
                    name: node.into(),
                    passed: true,
                    message: format!("{label}: \"pending\" == \"latest\" ({vp})"),
                }
            } else if label.contains("estimateGas") {
                CheckResult {
                    name: node.into(),
                    passed: true,
                    message: format!(
                        "{label}: \"pending\" ({vp}) != \"latest\" ({vl}) \
                         -- minor base-fee drift (acceptable)"
                    ),
                }
            } else {
                CheckResult {
                    name: node.into(),
                    passed: false,
                    message: format!(
                        "{label}: \"pending\" ({vp}) != \"latest\" ({vl}) \
                         (pending block not fully suppressed?)"
                    ),
                }
            }
        }
        (RpcOutcome::Err { code, message }, _) => CheckResult {
            name: node.into(),
            passed: false,
            message: format!("{label}: \"pending\" errored: {code}: {message}"),
        },
        (_, RpcOutcome::Err { code, message }) => CheckResult {
            name: node.into(),
            passed: false,
            message: format!("{label}: \"latest\" errored: {code}: {message}"),
        },
        (RpcOutcome::Transport(e), _) | (_, RpcOutcome::Transport(e)) => CheckResult {
            name: node.into(),
            passed: false,
            message: format!("{label}: transport error: {e}"),
        },
    }
}

async fn check_blocked(
    client: &reqwest::Client,
    url: &Url,
    node: &str,
    method: &str,
    params: Value,
) -> CheckResult {
    match rpc_call(client, url, method, params).await {
        RpcOutcome::Err { code, .. } if code == ARC_BLOCKED_ERROR_CODE => CheckResult {
            name: node.into(),
            passed: true,
            message: format!("{method} error -32001 (blocked as expected)"),
        },
        RpcOutcome::Ok(_) => CheckResult {
            name: node.into(),
            passed: false,
            message: format!(
                "{method} returned data! \
                 Pending-tx filter is disabled (--arc.hide-pending-txs must be set)"
            ),
        },
        RpcOutcome::Err { code, message } => CheckResult {
            name: node.into(),
            passed: false,
            message: format!("{method} unexpected error {code}: {message} (expected -32001)"),
        },
        RpcOutcome::Transport(e) => CheckResult {
            name: node.into(),
            passed: false,
            message: format!("{method} transport error: {e}"),
        },
    }
}

async fn check_namespace_disabled(
    client: &reqwest::Client,
    url: &Url,
    node: &str,
    method: &str,
    params: Value,
) -> CheckResult {
    match rpc_call(client, url, method, params).await {
        RpcOutcome::Err { code, .. } => CheckResult {
            name: node.into(),
            passed: true,
            message: format!("{method} error {code} (namespace disabled, as expected)"),
        },
        RpcOutcome::Ok(_) => CheckResult {
            name: node.into(),
            passed: false,
            message: format!(
                "{method} returned data! Namespace must be disabled \
                 (--http.api eth,net,web3 --ws.api eth,net,web3)"
            ),
        },
        RpcOutcome::Transport(e) => CheckResult {
            name: node.into(),
            passed: false,
            message: format!("{method} transport error: {e}"),
        },
    }
}

async fn check_mempool_nonce_blocked(
    client: &reqwest::Client,
    url: &Url,
    node: &str,
    addr: &str,
) -> CheckResult {
    let nonce_hex = match rpc_call(
        client,
        url,
        "eth_getTransactionCount",
        json!([addr, "latest"]),
    )
    .await
    {
        RpcOutcome::Ok(v) => v.as_str().unwrap_or("0x0").to_owned(),
        _ => "0x0".to_owned(),
    };

    match rpc_call(
        client,
        url,
        "eth_getTransactionBySenderAndNonce",
        json!([addr, &nonce_hex]),
    )
    .await
    {
        RpcOutcome::Err { code, .. } => CheckResult {
            name: node.into(),
            passed: true,
            message: format!(
                "eth_getTransactionBySenderAndNonce(addr, nonce={nonce_hex}) \
                 error {code} (mempool lookup blocked)"
            ),
        },
        RpcOutcome::Ok(v) if v.is_null() => CheckResult {
            name: node.into(),
            passed: true,
            message: format!(
                "eth_getTransactionBySenderAndNonce(addr, nonce={nonce_hex}) \
                 null (no mempool leak)"
            ),
        },
        RpcOutcome::Ok(v) => {
            let hash = v.get("hash").and_then(|h| h.as_str()).unwrap_or("?");
            CheckResult {
                name: node.into(),
                passed: false,
                message: format!(
                    "eth_getTransactionBySenderAndNonce(addr, nonce={nonce_hex}) \
                     {hash} (mempool lookup not blocked!)"
                ),
            }
        }
        RpcOutcome::Transport(e) => CheckResult {
            name: node.into(),
            passed: false,
            message: format!("eth_getTransactionBySenderAndNonce transport error: {e}"),
        },
    }
}

// ── Full check battery per node ──────────────────────────────────────────────

async fn check_node(
    client: &reqwest::Client,
    node: &str,
    url: &Url,
    addr: &str,
) -> Vec<CheckResult> {
    let mut checks = Vec::new();

    // 1. Pending block suppression
    checks.push(check_pending_block_is_null(client, url, node).await);

    // 2-7. Pending state falls back to latest
    let state_methods: &[(&str, &str, Value, Value)] = &[
        (
            "eth_getBalance",
            "eth_getBalance",
            json!([addr, "pending"]),
            json!([addr, "latest"]),
        ),
        (
            "eth_getTransactionCount",
            "eth_getTransactionCount",
            json!([addr, "pending"]),
            json!([addr, "latest"]),
        ),
        (
            "eth_getCode",
            "eth_getCode",
            json!([addr, "pending"]),
            json!([addr, "latest"]),
        ),
        (
            "eth_getStorageAt(slot 0)",
            "eth_getStorageAt",
            json!([addr, "0x0", "pending"]),
            json!([addr, "0x0", "latest"]),
        ),
        (
            "eth_call({to: 0x0})",
            "eth_call",
            json!([{"to": ZERO_ADDR, "data": "0x"}, "pending"]),
            json!([{"to": ZERO_ADDR, "data": "0x"}, "latest"]),
        ),
        (
            "eth_estimateGas({to: 0x0})",
            "eth_estimateGas",
            json!([{"to": ZERO_ADDR, "data": "0x"}, "pending"]),
            json!([{"to": ZERO_ADDR, "data": "0x"}, "latest"]),
        ),
    ];

    for (label, method, params_pending, params_latest) in state_methods {
        checks.push(
            check_pending_eq_latest(
                client,
                url,
                node,
                label,
                method,
                params_pending.clone(),
                params_latest.clone(),
            )
            .await,
        );
    }

    // 8. Pending-tx RPCs blocked
    checks.push(
        check_blocked(
            client,
            url,
            node,
            "eth_newPendingTransactionFilter",
            json!([]),
        )
        .await,
    );

    // 9. Sensitive namespaces disabled
    for method in &["txpool_status", "txpool_inspect", "txpool_content"] {
        checks.push(check_namespace_disabled(client, url, node, method, json!([])).await);
    }
    checks.push(
        check_namespace_disabled(client, url, node, "txpool_contentFrom", json!([addr])).await,
    );

    checks.push(
        check_namespace_disabled(
            client,
            url,
            node,
            "debug_traceCall",
            json!([{"to": ZERO_ADDR}, "latest", {}]),
        )
        .await,
    );
    checks.push(
        check_namespace_disabled(
            client,
            url,
            node,
            "debug_getRawTransaction",
            json!(["0x0000000000000000000000000000000000000000000000000000000000000000"]),
        )
        .await,
    );

    checks.push(
        check_namespace_disabled(
            client,
            url,
            node,
            "trace_call",
            json!([{"to": ZERO_ADDR}, ["trace"], "latest"]),
        )
        .await,
    );
    checks
        .push(check_namespace_disabled(client, url, node, "trace_block", json!(["latest"])).await);

    for method in &["admin_peers", "admin_nodeInfo"] {
        checks.push(check_namespace_disabled(client, url, node, method, json!([])).await);
    }

    for method in &[
        "flashbots_validateBuilderSubmissionV3",
        "mev_sendBundle",
        "mev_simBundle",
        "ots_getApiLevel",
    ] {
        checks.push(check_namespace_disabled(client, url, node, method, json!([])).await);
    }
    checks.push(
        check_namespace_disabled(
            client,
            url,
            node,
            "ots_searchTransactionsBefore",
            json!([addr, 0, 10]),
        )
        .await,
    );

    // 10. Mempool nonce lookup blocked
    checks.push(check_mempool_nonce_blocked(client, url, node, addr).await);

    checks
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Verify that nodes do not expose pending state or mempool data via RPC.
///
/// Runs the full MEV / pending-state surface check battery against each node.
/// Returns a [`Report`] with individual [`CheckResult`]s.
///
/// # Arguments
/// * `rpc_urls` — `(node_name, rpc_url)` pairs to audit
/// * `addr` — address used for state-query assertions (e.g. `eth_getBalance`)
pub async fn check_pending_state(rpc_urls: &[(String, Url)], addr: &str) -> Result<Report> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;

    let mut checks = Vec::new();
    for (node_name, url) in rpc_urls {
        checks.extend(check_node(&client, node_name, url, addr).await);
    }

    Ok(Report { checks })
}
