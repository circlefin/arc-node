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

//! Regression guard for EIP-161 empty-account clearing across the reth 2.2 upgrade.
//!
//! reth 1.11 enabled state clearing via an explicit `set_state_clear_flag(true)`
//! call in `apply_pre_execution_changes`. reth 2.2 removed that hook and relies on
//! revm's Journal to clear touched-but-empty accounts automatically. This test
//! pins the resulting behavior so a future bump that disables clearing — leaving
//! empty accounts in the trie — fails here with a state-root divergence rather
//! than silently forking the network.

use alloy_primitives::{address, Address, U256};
use alloy_rpc_types_eth::TransactionRequest;
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;
use reth_provider::{AccountReader, StateProviderFactory};

/// Fresh address with no genesis allocation. A zero-value call touches it but
/// leaves it empty (nonce 0, balance 0, no code).
const FRESH_EOA: Address = address!("0x00000000000000000000000000000000e1610000");

/// A zero-value transaction to a fresh EOA touches the recipient but leaves it
/// empty. The touched empty account is cleared, never persisted as an empty trie
/// entry — the behavior is spec-fixed (Spurious Dragon), identical before and
/// after the reth 2.2 upgrade.
///
/// Trie membership is the only RPC/state-observable that reflects clearing:
/// `balance`/`nonce`/`code` all read zero whether the account was cleared or
/// persisted empty, so the assertion checks `basic_account` directly.
#[tokio::test]
async fn zero_value_call_to_fresh_eoa_clears_empty_account() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    let tx_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(FRESH_EOA)),
                value: Some(U256::ZERO),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let receipt = node.get_receipt(tx_hash).await?;
    assert!(receipt.status());

    let state = node.node.inner.provider().latest()?;
    let account = state.basic_account(&FRESH_EOA)?;
    assert!(account.is_none());
    Ok(())
}
