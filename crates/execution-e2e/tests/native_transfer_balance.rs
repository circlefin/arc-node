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

//! E2E tests verifying that native value transfers produce correct balance changes.

use alloy_primitives::{address, Address, U256};
use arc_execution_e2e::{
    actions::{AssertBalance, AssertTxIncluded, ProduceBlocks, SendTransaction, TxStatus},
    ArcSetup, ArcTestBuilder,
};
use eyre::Result;

/// Genesis-funded sender address (hardhat account #0).
const SENDER: Address = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");

/// Recipient with zero balance in genesis.
const RECIPIENT: Address = address!("0x000000000000000000000000000000000000bEEF");

/// Sender's genesis balance: 1,000,000 USDC (1_000_000e18 wei).
/// 0xd3c21bcecceda1000000 = limbs [0x1bcecceda1000000, 0xd3c2, 0, 0]
const SENDER_GENESIS_BALANCE: U256 = U256::from_limbs([0x1bce_cced_a100_0000, 0xd3c2, 0, 0]);

/// Transfer value used in tests: 100 USDC (100e18 wei).
fn transfer_value() -> U256 {
    U256::from(100u64) * U256::from(10u64).pow(U256::from(18u64))
}

/// Max gas cost per tx: gas_limit(26_000) * max_fee_per_gas(1000e9).
fn max_gas_cost() -> U256 {
    U256::from(26_000u64) * U256::from(1_000_000_000_000u64)
}

/// Recipient balance goes from 0 to the transferred value.
#[tokio::test]
async fn test_value_transfer_credits_recipient() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = transfer_value();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer")
                .with_to(RECIPIENT)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(AssertBalance::new(RECIPIENT, value))
        .run()
        .await
}

/// Sender balance decreases by at least the transferred value (plus gas).
#[tokio::test]
async fn test_value_transfer_debits_sender() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = transfer_value();
    let min_remaining = SENDER_GENESIS_BALANCE - value - max_gas_cost();
    let max_remaining = SENDER_GENESIS_BALANCE - value;

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer")
                .with_to(RECIPIENT)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(
            AssertBalance::new(SENDER, min_remaining)
                .at_least()
                .at_most(max_remaining),
        )
        .run()
        .await
}

/// Zero-value transfer leaves recipient balance unchanged.
#[tokio::test]
async fn test_zero_value_transfer_no_balance_change() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("zero_transfer")
                .with_to(RECIPIENT)
                .with_value(U256::ZERO),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("zero_transfer").expect(TxStatus::Success))
        .with_action(AssertBalance::new(RECIPIENT, U256::ZERO))
        .run()
        .await
}

/// Two transfers to the same recipient accumulate.
#[tokio::test]
async fn test_multiple_transfers_accumulate() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = transfer_value();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("tx1")
                .with_to(RECIPIENT)
                .with_value(value),
        )
        .with_action(
            SendTransaction::new("tx2")
                .with_to(RECIPIENT)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("tx1").expect(TxStatus::Success))
        .with_action(AssertTxIncluded::new("tx2").expect(TxStatus::Success))
        .with_action(AssertBalance::new(RECIPIENT, value + value))
        .run()
        .await
}
