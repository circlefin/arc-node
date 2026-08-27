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

//! Verifies `--arc.tx.relays` failover against a real network: a full node
//! relays raw tx submission to validators with failover, and keeps accepting
//! and mining transactions (and answering tx-lifecycle queries) after its
//! primary relay target is killed. Also verifies the relay list wraps
//! around: after failing over to the last entry, killing that entry too
//! (with the first entry back up) must fail over back to the start of the
//! list, not dead-end. Pairs with the `tx-relay-failover.toml` scenario,
//! which is the only manifest that configures `full1`'s relay list.

use std::time::Duration;

use color_eyre::eyre::{self, Context};
use tracing::{info, warn};

use super::tx::sign_self_transfer;
use super::{quake_test, CheckResult, RpcClientFactory, TestOutcome, TestParams, TestResult};
use crate::testnet::Testnet;

/// Distinct from `tx::transfer_test`'s default account (index 0) so the two
/// tests never race on the same nonce if run in the same session.
const RELAY_TEST_ACCOUNT_INDEX: u32 = 1;

/// Must match `tx-relay-failover.toml`'s `full1.el.config.arc.tx.relays` order:
/// validator1 primary, validator2 backup.
const PRIMARY_RELAY_TARGET: &str = "validator1";
const SECONDARY_RELAY_TARGET: &str = "validator2";
const RELAY_NODE: &str = "full1";

const RECEIPT_TIMEOUT_SECS: u64 = 20;
const RECEIPT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Sends a tx to a full node that relays to validators with failover, kills
/// the primary relay target, sends a second tx, and confirms the full node
/// keeps accepting and mining transactions and answering tx-lifecycle queries
/// for both. Then restarts the primary and kills the secondary, and sends a
/// third tx to confirm the relay list wraps back around to the primary
/// instead of dead-ending on the last entry.
#[quake_test(group = "tx", name = "relay_failover")]
fn relay_failover_test<'a>(
    testnet: &'a Testnet,
    factory: &'a RpcClientFactory,
    _params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        // This test is tied to `tx-relay-failover.toml`'s specific topology
        // (a `full1` relaying to `validator1`/`validator2`). `tx` isn't a
        // default-excluded group, so an unscoped `quake test` run against any
        // other manifest (e.g. `localdev-remote-signer.toml`, which has no
        // `full1`) would otherwise hit this test — skip gracefully instead of
        // failing, matching `mev::pending_state_test`'s convention.
        let Some(relay_node_url) = testnet
            .nodes_metadata
            .all_execution_urls()
            .into_iter()
            .find(|(name, _)| name == RELAY_NODE)
            .map(|(_, url)| url)
        else {
            warn!("Skipping: no '{RELAY_NODE}' node in this manifest");
            return Ok(());
        };
        let relay_client = factory.create(relay_node_url);

        let mut outcome = TestOutcome::new();

        // tx1: primary relay target (validator1) is up, sticky selection starts there.
        send_and_check_mined(&relay_client, &mut outcome, "tx1_before_kill").await?;

        // Kill the primary relay target. An explicit stop/start (not a timed
        // `Perturbation::Kill`, whose single call blocks for a fixed duration
        // before auto-restarting) keeps the outage exactly as long as needed,
        // with no duration to guess and no background race to manage.
        testnet.stop(vec![PRIMARY_RELAY_TARGET.to_string()]).await?;

        let result =
            submit_and_check_during_outage(&relay_client, &mut outcome, "tx2", "tx2_after_kill")
                .await;

        // Always restart, even on failure, so a failed run doesn't leave the
        // testnet at degraded quorum for later tests.
        testnet
            .start(vec![PRIMARY_RELAY_TARGET.to_string()], false)
            .await?;
        result?;

        // Confirm consensus has actually resumed with the primary back in the
        // validator set before killing the secondary — a restarted node's
        // gossipsub mesh can take a few seconds to re-form, and proceeding too
        // early can stall the whole network with only 2/4 validators live.
        testnet
            .wait_rounds(1, Duration::from_secs(RECEIPT_TIMEOUT_SECS))
            .await
            .wrap_err("consensus did not resume after restarting the primary relay target")?;

        // tx3: primary is back up, secondary (where sticky selection now
        // points, after tx2 failed over) is killed. Failover must wrap
        // cyclically back to the primary instead of dead-ending.
        testnet
            .stop(vec![SECONDARY_RELAY_TARGET.to_string()])
            .await?;

        let result = submit_and_check_during_outage(
            &relay_client,
            &mut outcome,
            "tx3",
            "tx3_after_wraparound",
        )
        .await;

        testnet
            .start(vec![SECONDARY_RELAY_TARGET.to_string()], false)
            .await?;
        result?;

        outcome
            .auto_summary(
                "Tx relay failover verified: full node kept accepting and mining transactions \
                 and answering tx-lifecycle queries after the primary relay target was killed, \
                 and failover wrapped back around to the primary after the secondary was also killed",
                "Tx relay failover check(s) failed: {}",
            )
            .into_result()
    })
}

/// Submits a tx while a relay target is down, checks it's locally visible
/// before it's mined, then waits for it to be mined via failover.
async fn submit_and_check_during_outage(
    relay_client: &crate::rpc::RpcClient,
    outcome: &mut TestOutcome,
    tx_label: &str,
    mined_check_name: &str,
) -> eyre::Result<()> {
    let raw_tx = sign_self_transfer(relay_client, RELAY_TEST_ACCOUNT_INDEX).await?;
    let tx_hash = relay_client
        .send_raw_transaction(&raw_tx)
        .await
        .wrap_err(format!("failed to send {tx_label} via relay during outage"))?;
    info!(%tx_hash, tx_label, "tx sent while a relay target is down");

    // The full node retains an accepted relay in its own local pool, so it
    // must answer this before the tx is mined — independent of which
    // upstream (down) or failover target ends up mining it. See
    // docs/tx-forwarding.md.
    match relay_client.get_transaction_by_hash(&tx_hash).await {
        Ok(Some(_)) => outcome.add_check(CheckResult::success(
            format!("{tx_label}_visible_locally_while_pending"),
            "full node answered eth_getTransactionByHash for the pending relayed tx",
        )),
        Ok(None) => outcome.add_check(CheckResult::failure(
            format!("{tx_label}_visible_locally_while_pending"),
            "full node has no local record of the relayed tx before it was mined",
        )),
        Err(e) => outcome.add_check(CheckResult::failure(
            format!("{tx_label}_visible_locally_while_pending"),
            format!("failed to query eth_getTransactionByHash: {e}"),
        )),
    }

    check_mined(relay_client, outcome, &tx_hash, mined_check_name).await
}

/// Builds, signs, and sends a self-transfer via `relay_client`, then checks it's mined.
async fn send_and_check_mined(
    relay_client: &crate::rpc::RpcClient,
    outcome: &mut TestOutcome,
    check_name: &str,
) -> eyre::Result<()> {
    let raw_tx = sign_self_transfer(relay_client, RELAY_TEST_ACCOUNT_INDEX).await?;
    let tx_hash = relay_client
        .send_raw_transaction(&raw_tx)
        .await
        .wrap_err("failed to send transaction via relay")?;
    info!(%tx_hash, check_name, "Transaction sent via relay");

    check_mined(relay_client, outcome, &tx_hash, check_name).await
}

/// Polls for a mined receipt and records a success/failure check — never
/// bails on a missing or failed receipt, so the caller's outcome always
/// reflects every check performed, matching `tx::transfer_test`'s pattern.
async fn check_mined(
    relay_client: &crate::rpc::RpcClient,
    outcome: &mut TestOutcome,
    tx_hash: &str,
    check_name: &str,
) -> eyre::Result<()> {
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(RECEIPT_TIMEOUT_SECS);
    let mut receipt = None;
    let mut last_error = None;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(std::cmp::min(RECEIPT_POLL_INTERVAL, remaining)).await;
        match relay_client.get_transaction_receipt(tx_hash).await {
            Ok(Some(r)) => {
                receipt = Some(r);
                break;
            }
            Ok(None) => {}
            Err(e) => last_error = Some(e.to_string()),
        }
    }

    match receipt {
        Some(r) => {
            let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "0x1" {
                outcome.add_check(CheckResult::success(
                    check_name,
                    format!("tx {tx_hash} mined successfully"),
                ));
            } else {
                outcome.add_check(CheckResult::failure(
                    check_name,
                    format!("tx {tx_hash} mined with status {status} (expected 0x1)"),
                ));
            }
        }
        None => {
            let err_suffix = last_error
                .map(|e| format!(" (last receipt RPC error: {e})"))
                .unwrap_or_default();
            outcome.add_check(CheckResult::failure(
                check_name,
                format!("tx {tx_hash} not committed after {RECEIPT_TIMEOUT_SECS}s{err_suffix}"),
            ));
        }
    }

    Ok(())
}
