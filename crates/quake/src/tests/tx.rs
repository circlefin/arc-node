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

/// Maximum number of receipt polling attempts.
const MAX_RECEIPT_POLLS: u32 = 20;

/// Delay between receipt polling attempts.
const RECEIPT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Submit a value transfer from a pre-funded genesis account to itself and
/// verify that the transaction is committed in a block with a success status.
#[quake_test(group = "tx", name = "transfer")]
fn transfer_test<'a>(
    testnet: &'a Testnet,
    factory: &'a RpcClientFactory,
    _params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        let node_urls = testnet.nodes_metadata.all_execution_urls();
        let (node_name, node_url) = node_urls
            .choose(&mut thread_rng())
            .ok_or_else(|| eyre::eyre!("no nodes available"))?;

        info!(node = %node_name, url = %node_url, "Selected target node");

        let client = factory.create(node_url.clone());

        // Derive signer from test mnemonic
        let mut signer = MnemonicBuilder::<English>::default()
            .phrase(TEST_MNEMONIC)
            .derivation_path(format!("m/44'/60'/1'/0/{TEST_ACCOUNT_INDEX}"))
            .wrap_err("invalid derivation path")?
            .build()
            .wrap_err("failed to build signer from mnemonic")?;

        signer.set_chain_id(Some(CHAIN_ID));
        let address = signer.address();
        debug!(%address, "Derived signer");

        // Query current nonce
        let nonce = client
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
        let tx_hash = client
            .send_raw_transaction(&raw_tx)
            .await
            .wrap_err("failed to send transaction")?;
        info!(%tx_hash, "Transaction sent");

        // Poll for receipt
        let mut receipt = None;
        for attempt in 1..=MAX_RECEIPT_POLLS {
            tokio::time::sleep(RECEIPT_POLL_INTERVAL).await;
            match client.get_transaction_receipt(&tx_hash).await {
                Ok(Some(r)) => {
                    debug!(attempt, "Receipt received");
                    receipt = Some(r);
                    break;
                }
                Ok(None) => {
                    debug!(attempt, "Receipt not yet available");
                }
                Err(e) => {
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
                    "node": node_name,
                    "status": status,
                    "block": block,
                    "index": index,
                    "gas_used": gas_used,
                    "effective_gas_price": gas_price,
                });
                let pretty =
                    serde_json::to_string_pretty(&summary).unwrap_or_else(|_| summary.to_string());

                if status == "1" {
                    outcome.add_check(CheckResult::success(node_name, format!("\n{pretty}")));
                } else {
                    outcome.add_check(CheckResult::failure(
                        node_name,
                        format!("status {status} (expected 1)\n{pretty}"),
                    ));
                }
            }
            None => {
                outcome.add_check(CheckResult::failure(
                    node_name,
                    format!(
                        "tx not committed after {}s\n  tx: {blockscout_tx_url}",
                        MAX_RECEIPT_POLLS as u64 * RECEIPT_POLL_INTERVAL.as_millis() as u64 / 1000
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
