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
use arc_execution_e2e::{
    actions::{AssertAccountAbsent, AssertTxIncluded, ProduceBlocks, SendTransaction, TxStatus},
    ArcSetup, ArcTestBuilder,
};
use eyre::Result;

/// Fresh address with no genesis allocation. A zero-value call touches it but
/// leaves it empty (nonce 0, balance 0, no code).
const FRESH_EOA: Address = address!("0x00000000000000000000000000000000e1610000");

/// A zero-value transaction to a fresh EOA touches the recipient but leaves it
/// empty. Under EIP-161 (Spurious Dragon, always active on Arc) the touched
/// empty account must be cleared, never persisted as an empty trie entry.
///
/// Trie membership is the only RPC/state-observable that distinguishes the two
/// outcomes: `balance`/`nonce`/`code` all read zero whether the account was
/// cleared or persisted empty, so the assertion checks `basic_account` directly.
#[tokio::test]
async fn zero_value_call_to_fresh_eoa_clears_empty_account() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("touch_fresh_eoa")
                .with_to(FRESH_EOA)
                .with_value(U256::ZERO),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("touch_fresh_eoa").expect(TxStatus::Success))
        .with_action(AssertAccountAbsent::new(FRESH_EOA))
        .run()
        .await
}
