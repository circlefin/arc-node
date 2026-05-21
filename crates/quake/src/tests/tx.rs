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
use rand::{seq::SliceRandom, thread_rng};
use tracing::{debug, info};

use super::{quake_test, CheckResult, RpcClientFactory, TestOutcome, TestParams, TestResult};
use crate::testnet::Testnet;

/// Test mnemonic matching genesis pre-funded accounts.
const TEST_MNEMONIC: &str = "test test test test test test test test test test test junk";

/// Account index for the test signer (first extra-prefunded genesis account for load testing).
const TEST_ACCOUNT_INDEX: u32 = 0;

const CHAIN_ID: u64 = 1337;
const MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000; // 1 gwei
const MAX_FEE_PER_GAS: u128 = 2_000_000_000; // 2 gwei
const GAS_LIMIT: u64 = 30_000; // sufficient for a simple value transfer on Arc (~26k with blocklist check)

/// Default receipt polling timeout.
const DEFAULT_RECEIPT_TIMEOUT_SECS: u64 = 10;

/// Delay between receipt polling attempts.
const RECEIPT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

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

    node_urls
        .choose(&mut thread_rng())
        .cloned()
        .ok_or_else(|| eyre::eyre!("no nodes available"))
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
}
