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

use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::{TxKind, U256};
use alloy_signer::Signer;
use alloy_signer_local::{coins_bip39::English, MnemonicBuilder};
use color_eyre::eyre::{self, Context};
use indexmap::IndexMap;
use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
use tracing::{debug, info};

use super::{quake_test, CheckResult, RpcClientFactory, TestOutcome, TestParams, TestResult};
use crate::manifest;
use crate::testnet::Testnet;

/// Test mnemonic matching genesis pre-funded accounts.
const TEST_MNEMONIC: &str = "test test test test test test test test test test test junk";

/// Account index for the test signer (first extra-prefunded genesis account for load testing).
const TEST_ACCOUNT_INDEX: u32 = 0;

const CHAIN_ID: u64 = 1337;
const MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000; // 1 gwei
const MAX_FEE_PER_GAS: u128 = 40_000_000_000_000; // 40,000 gwei (2x headroom over the 20,000 gwei maxBaseFee ceiling)
const GAS_LIMIT: u64 = 30_000; // sufficient for a simple value transfer on Arc (~26k with blocklist check)

/// Default receipt polling timeout.
const DEFAULT_RECEIPT_TIMEOUT_SECS: u64 = 20;

/// Delay between receipt polling attempts.
const RECEIPT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Maximum time to wait for a late-joining target node to come online.
const TARGET_READINESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Delay between target-readiness polls.
const TARGET_READINESS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Submit a value transfer from a pre-funded genesis account to itself and
/// verify that the transaction is committed in a block with a success status.
#[quake_test(group = "tx", name = "transfer")]
fn transfer_test<'a>(
    testnet: &'a Testnet,
    factory: &'a RpcClientFactory,
    params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        let (target_node_name, target_node_url) = target_node(testnet, params)?;
        let receipt_node_name = params
            .get("receipt_node")
            .unwrap_or(target_node_name.as_str())
            .to_string();
        let receipt_node_url = named_node_url(
            &testnet.nodes_metadata.all_execution_urls(),
            "receipt_node",
            &receipt_node_name,
        )?;
        let account_index = param_u32(params, "account_index", TEST_ACCOUNT_INDEX)
            .wrap_err("invalid account_index")?;
        let receipt_timeout = std::time::Duration::from_secs(
            param_u64(params, "receipt_timeout_s", DEFAULT_RECEIPT_TIMEOUT_SECS)
                .wrap_err("invalid receipt_timeout_s")?,
        );

        info!(
            target_node = %target_node_name,
            target_url = %target_node_url,
            receipt_node = %receipt_node_name,
            receipt_url = %receipt_node_url,
            account_index,
            "Selected tx propagation probe nodes",
        );

        let target_client = factory.create(target_node_url);
        let receipt_client = factory.create(receipt_node_url);

        if target_starts_late(&testnet.manifest.nodes, &target_node_name) {
            wait_for_node_started(&target_client, &target_node_name).await?;
        }

        // Derive signer from test mnemonic
        let mut signer = MnemonicBuilder::<English>::default()
            .phrase(TEST_MNEMONIC)
            .derivation_path(format!("m/44'/60'/1'/0/{account_index}"))
            .wrap_err("invalid derivation path")?
            .build()
            .wrap_err("failed to build signer from mnemonic")?;

        signer.set_chain_id(Some(CHAIN_ID));
        let address = signer.address();
        debug!(%address, "Derived signer");

        // Query current nonce
        let nonce = target_client
            .get_transaction_count(&format!("{address:#x}"))
            .await
            .wrap_err("failed to query nonce")?;
        debug!(%nonce, "Current nonce");

        // Build self-transfer transaction
        let tx = TxEip1559 {
            chain_id: CHAIN_ID,
            nonce,
            max_priority_fee_per_gas: MAX_PRIORITY_FEE_PER_GAS,
            max_fee_per_gas: MAX_FEE_PER_GAS,
            gas_limit: GAS_LIMIT,
            to: TxKind::Call(address),
            value: U256::from(1),
            input: Default::default(),
            access_list: Default::default(),
        };

        // Sign
        let sig_hash = tx.signature_hash();
        let signature = signer
            .sign_hash(&sig_hash)
            .await
            .wrap_err("failed to sign transaction")?;
        let signed_tx = tx.into_signed(signature);

        // Encode to EIP-2718
        let mut buf = Vec::with_capacity(signed_tx.eip2718_encoded_length());
        signed_tx.eip2718_encode(&mut buf);
        let raw_tx = format!("0x{}", hex::encode(&buf));

        // Send
        let tx_hash = target_client
            .send_raw_transaction(&raw_tx)
            .await
            .wrap_err("failed to send transaction")?;
        info!(%tx_hash, "Transaction sent");

        // Poll for receipt
        let mut receipt = None;
        let mut last_receipt_error = None;
        let deadline = tokio::time::Instant::now() + receipt_timeout;
        let mut attempt = 0_u32;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::sleep(std::cmp::min(RECEIPT_POLL_INTERVAL, remaining)).await;
            attempt += 1;

            match receipt_client.get_transaction_receipt(&tx_hash).await {
                Ok(Some(r)) => {
                    debug!(attempt, "Receipt received");
                    receipt = Some(r);
                    break;
                }
                Ok(None) => {
                    debug!(attempt, "Receipt not yet available");
                }
                Err(e) => {
                    last_receipt_error = Some(e.to_string());
                    debug!(attempt, error = %e, "Failed to query receipt");
                }
            }
        }

        // Build blockscout URL if available
        let (_, _, blockscout_port) = testnet.infra_data.monitoring_ports();
        let blockscout_tx_url = format!("http://localhost:{blockscout_port}/tx/{tx_hash}");

        let mut outcome = TestOutcome::new();

        match receipt {
            Some(r) => {
                let status = hex_to_dec_str(r.get("status"));
                let block = hex_to_dec_str(r.get("blockNumber"));
                let index = hex_to_dec_str(r.get("transactionIndex"));
                let gas_used = hex_to_dec_str(r.get("gasUsed"));
                let gas_price = hex_to_dec_str(r.get("effectiveGasPrice"));

                let summary = serde_json::json!({
                    "tx": tx_hash,
                    "url": blockscout_tx_url,
                    "target_node": &target_node_name,
                    "receipt_node": &receipt_node_name,
                    "account_index": account_index,
                    "status": status,
                    "block": block,
                    "index": index,
                    "gas_used": gas_used,
                    "effective_gas_price": gas_price,
                });
                let pretty =
                    serde_json::to_string_pretty(&summary).unwrap_or_else(|_| summary.to_string());

                if status == "1" {
                    outcome.add_check(CheckResult::success(
                        target_node_name,
                        format!("\n{pretty}"),
                    ));
                } else {
                    outcome.add_check(CheckResult::failure(
                        target_node_name,
                        format!("status {status} (expected 1)\n{pretty}"),
                    ));
                }
            }
            None => {
                let receipt_error = last_receipt_error
                    .as_ref()
                    .map(|e| format!("\n  last receipt RPC error: {e}"))
                    .unwrap_or_default();
                outcome.add_check(CheckResult::failure(
                    target_node_name,
                    format!(
                        "tx not committed after {}s via receipt node {}{}\n  tx: {blockscout_tx_url}",
                        receipt_timeout.as_secs(),
                        receipt_node_name,
                        receipt_error,
                    ),
                ));
            }
        }

        outcome
            .auto_summary(
                "Transaction committed successfully",
                "Transaction failed: {}",
            )
            .into_result()
    })
}

fn target_node(testnet: &Testnet, params: &TestParams) -> eyre::Result<(String, reqwest::Url)> {
    let node_urls = testnet.nodes_metadata.all_execution_urls();
    if let Some(node_name) = params.get("target_node") {
        let node_url = named_node_url(&node_urls, "target_node", node_name)?;
        return Ok((node_name.to_string(), node_url));
    }

    let seed = testnet
        .seed
        .ok_or_else(|| eyre::eyre!("testnet seed is not set"))?;

    pick_random_node(&node_urls, seed)
}

/// Pick a node deterministically from `seed`.
fn pick_random_node(
    node_urls: &[(String, reqwest::Url)],
    seed: u64,
) -> eyre::Result<(String, reqwest::Url)> {
    let mut rng = StdRng::seed_from_u64(seed);
    node_urls
        .choose(&mut rng)
        .map(|(name, url)| (name.clone(), url.clone()))
        .ok_or_else(|| eyre::eyre!("no nodes available"))
}

/// `true` if the manifest configures `name` to start after genesis. Callers
/// gate RPC submission on the node coming online when this is set.
fn target_starts_late(manifest_nodes: &IndexMap<String, manifest::Node>, name: &str) -> bool {
    manifest_nodes
        .get(name)
        .and_then(|node| node.start_at)
        .unwrap_or(0)
        > 0
}

/// Poll `eth_blockNumber` on the target until it reports a non-zero height,
/// or [`TARGET_READINESS_TIMEOUT`] elapses.
async fn wait_for_node_started(
    client: &crate::rpc::RpcClient,
    node_name: &str,
) -> eyre::Result<()> {
    let deadline = tokio::time::Instant::now() + TARGET_READINESS_TIMEOUT;
    let mut last_err: Option<String> = None;
    let mut last_height: Option<u64> = None;
    loop {
        match client.get_latest_block_number_with_retries(0).await {
            Ok(height) if height > 0 => {
                debug!(node = %node_name, height, "Target node ready");
                return Ok(());
            }
            Ok(height) => last_height = Some(height),
            Err(e) => last_err = Some(e.to_string()),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(eyre::eyre!(
                "target node {node_name} not serving after {:?} (last height: {:?}, last error: {:?})",
                TARGET_READINESS_TIMEOUT,
                last_height,
                last_err,
            ));
        }
        tokio::time::sleep(TARGET_READINESS_POLL_INTERVAL).await;
    }
}

fn named_node_url(
    node_urls: &[(String, reqwest::Url)],
    param_name: &str,
    node_name: &str,
) -> eyre::Result<reqwest::Url> {
    node_urls
        .iter()
        .find(|(name, _)| name == node_name)
        .map(|(_, url)| url.clone())
        .ok_or_else(|| eyre::eyre!("{param_name} '{node_name}' not found"))
}

fn param_u32(params: &TestParams, key: &str, default: u32) -> eyre::Result<u32> {
    params
        .get(key)
        .map(str::parse)
        .transpose()
        .wrap_err_with(|| format!("failed to parse {key} as u32"))?
        .map_or(Ok(default), Ok)
}

fn param_u64(params: &TestParams, key: &str, default: u64) -> eyre::Result<u64> {
    params
        .get(key)
        .map(str::parse)
        .transpose()
        .wrap_err_with(|| format!("failed to parse {key} as u64"))?
        .map_or(Ok(default), Ok)
}

/// Convert a JSON hex string value (e.g. "0x42") to a decimal string.
/// Returns "n/a" if the value is missing or not parseable.
fn hex_to_dec_str(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|v| v.as_str())
        .and_then(|s| {
            let hex = s.strip_prefix("0x").unwrap_or(s);
            u128::from_str_radix(hex, 16).ok()
        })
        .map(|n| n.to_string())
        .unwrap_or_else(|| "n/a".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> TestParams {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<Vec<_>>()
            .into()
    }

    #[test]
    fn parses_optional_numeric_params() {
        let params = params(&[("account_index", "91"), ("receipt_timeout_s", "30")]);

        assert_eq!(
            param_u32(&params, "account_index", TEST_ACCOUNT_INDEX).unwrap(),
            91
        );
        assert_eq!(
            param_u64(&params, "receipt_timeout_s", DEFAULT_RECEIPT_TIMEOUT_SECS).unwrap(),
            30
        );
    }

    #[test]
    fn numeric_params_use_defaults_when_unset() {
        let params = TestParams::default();

        assert_eq!(
            param_u32(&params, "account_index", TEST_ACCOUNT_INDEX).unwrap(),
            TEST_ACCOUNT_INDEX
        );
        assert_eq!(
            param_u64(&params, "receipt_timeout_s", DEFAULT_RECEIPT_TIMEOUT_SECS).unwrap(),
            DEFAULT_RECEIPT_TIMEOUT_SECS
        );
    }

    #[test]
    fn numeric_params_reject_invalid_values() {
        let params = params(&[("account_index", "not-a-number")]);

        let err = param_u32(&params, "account_index", TEST_ACCOUNT_INDEX).unwrap_err();
        assert!(err.to_string().contains("failed to parse account_index"));
    }

    #[test]
    fn named_node_url_resolves_exact_node_names() {
        let node_urls = vec![
            (
                "full-blue".to_string(),
                reqwest::Url::parse("http://127.0.0.1:8545").unwrap(),
            ),
            (
                "full-green".to_string(),
                reqwest::Url::parse("http://127.0.0.1:8645").unwrap(),
            ),
        ];

        let url = named_node_url(&node_urls, "target_node", "full-green").unwrap();

        assert_eq!(url.as_str(), "http://127.0.0.1:8645/");
    }

    #[test]
    fn named_node_url_rejects_unknown_nodes() {
        let node_urls = vec![(
            "full-blue".to_string(),
            reqwest::Url::parse("http://127.0.0.1:8545").unwrap(),
        )];

        let err = named_node_url(&node_urls, "receipt_node", "missing").unwrap_err();

        assert!(err.to_string().contains("receipt_node 'missing' not found"));
    }

    fn urls(names: &[&str]) -> Vec<(String, reqwest::Url)> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let url = reqwest::Url::parse(&format!("http://127.0.0.1:{}/", 8545 + i)).unwrap();
                ((*name).to_string(), url)
            })
            .collect()
    }

    fn manifest_nodes(entries: &[(&str, Option<u64>)]) -> IndexMap<String, manifest::Node> {
        entries
            .iter()
            .map(|(name, start_at)| {
                let node = manifest::Node {
                    start_at: *start_at,
                    ..manifest::Node::default()
                };
                ((*name).to_string(), node)
            })
            .collect()
    }

    #[test]
    fn pick_random_node_is_deterministic_for_same_seed() {
        let node_urls = urls(&["val-0", "val-1", "val-2", "val-3"]);

        let first = pick_random_node(&node_urls, 42).unwrap();
        let second = pick_random_node(&node_urls, 42).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn pick_random_node_selects_per_seed() {
        let node_urls = urls(&["val-0", "val-1", "val-2", "val-3"]);

        let picks: std::collections::HashSet<String> = (0..32)
            .map(|seed| pick_random_node(&node_urls, seed).unwrap().0)
            .collect();

        assert!(
            picks.len() > 1,
            "expected different seeds to select different nodes, got {picks:?}",
        );
    }

    #[test]
    fn pick_random_node_errors_on_empty_set() {
        let err = pick_random_node(&[], 0).unwrap_err();
        assert!(err.to_string().contains("no nodes available"));
    }

    #[test]
    fn target_starts_late_is_true_for_positive_start_at() {
        let manifest = manifest_nodes(&[("val-0", Some(100))]);
        assert!(target_starts_late(&manifest, "val-0"));
    }

    #[test]
    fn target_starts_late_is_false_for_zero_start_at() {
        let manifest = manifest_nodes(&[("val-0", Some(0))]);
        assert!(!target_starts_late(&manifest, "val-0"));
    }

    #[test]
    fn target_starts_late_is_false_for_unset_start_at() {
        let manifest = manifest_nodes(&[("val-0", None)]);
        assert!(!target_starts_late(&manifest, "val-0"));
    }

    #[test]
    fn target_starts_late_is_false_for_unknown_node() {
        let manifest = manifest_nodes(&[("val-0", Some(100))]);
        assert!(!target_starts_late(&manifest, "val-1"));
    }
}
