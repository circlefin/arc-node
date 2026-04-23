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

//! EIP-7708 gas accounting e2e tests.
//!
//! Verifies that gasUsed in receipts correctly accounts for the EIP-7708
//! log emission cost. Tests isolate transactions in separate blocks to get
//! clean per-transaction gas measurements (cumulative_gas_used == per-tx gas
//! when the tx is alone in its block).

mod helpers;

use alloy_primitives::{address, U256};
use arc_execution_e2e::{
    actions::{AssertTxIncluded, AssertTxTrace, ProduceBlocks, SendTransaction, TxStatus},
    ArcSetup, ArcTestBuilder,
};
use reth_provider::ReceiptProvider;

/// Mirrors `arc_precompiles::helpers::PRECOMPILE_SLOAD_GAS_COST`.
/// Under Zero6, each blocklist check costs one SLOAD at this price.
const PRECOMPILE_SLOAD_GAS_COST: u64 = 2100;

/// Test #44: Value transfer gasUsed > zero-value transfer gasUsed.
///
/// Isolates each tx in its own block so cumulative_gas_used == per-tx gas.
/// The value transfer incurs the EIP-7708 log emission cost (375 base + 375*3 topics
/// + 8*32 data = 1,756 gas overhead), so it must use strictly more gas.
#[tokio::test]
async fn test_value_transfer_gas_includes_log_cost() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000009999");
    let value = U256::from(1_000_000);

    let mut env = arc_execution_e2e::ArcEnvironment::new();
    arc_execution_e2e::ArcSetup::new()
        .apply(&mut env)
        .await
        .expect("setup failed");

    // Block 1: value transfer (isolated)
    let mut tx_with_value = SendTransaction::new("with_value")
        .with_to(recipient)
        .with_value(value);
    arc_execution_e2e::Action::execute(&mut tx_with_value, &mut env)
        .await
        .expect("send with value");

    let mut produce = arc_execution_e2e::actions::ProduceBlocks::new(1);
    arc_execution_e2e::Action::execute(&mut produce, &mut env)
        .await
        .expect("produce block 1");

    // Block 2: zero-value transfer (isolated)
    let mut tx_zero_value = SendTransaction::new("zero_value")
        .with_to(recipient)
        .with_value(U256::ZERO);
    arc_execution_e2e::Action::execute(&mut tx_zero_value, &mut env)
        .await
        .expect("send zero value");

    let mut produce2 = arc_execution_e2e::actions::ProduceBlocks::new(1);
    arc_execution_e2e::Action::execute(&mut produce2, &mut env)
        .await
        .expect("produce block 2");

    // Get receipts — each tx is alone in its block, so cumulative_gas_used == per-tx gas
    let with_value_hash = *env.get_tx_hash("with_value").expect("with_value hash");
    let zero_value_hash = *env.get_tx_hash("zero_value").expect("zero_value hash");

    let receipt_with = env
        .node()
        .inner
        .provider()
        .receipt_by_hash(with_value_hash)
        .expect("receipt query")
        .expect("receipt not found");

    let receipt_zero = env
        .node()
        .inner
        .provider()
        .receipt_by_hash(zero_value_hash)
        .expect("receipt query")
        .expect("receipt not found");

    let gas_with_value = receipt_with.cumulative_gas_used;
    let gas_zero_value = receipt_zero.cumulative_gas_used;

    // Value transfer must use strictly more gas due to EIP-7708 log emission
    assert!(
        gas_with_value > gas_zero_value,
        "Value transfer gas ({}) should be greater than zero-value transfer gas ({}). \
         The difference should be ~1756 gas for the EIP-7708 Transfer log.",
        gas_with_value,
        gas_zero_value
    );

    // The overhead should be approximately 1,756 gas (LOG3 cost for Transfer event)
    let overhead = gas_with_value
        .checked_sub(gas_zero_value)
        .expect("gas_with_value < gas_zero_value");
    assert!(
        overhead > 1000,
        "Gas overhead ({}) is suspiciously low — expected ~1756 for LOG3",
        overhead
    );
}

/// Test #45: Value transfer succeeds within default gas limit.
///
/// The default gas limit (26,000) should be sufficient for a simple value transfer
/// with EIP-7708 log emission overhead.
#[tokio::test]
async fn test_value_transfer_within_gas_limit() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000008888");
    let value = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("transfer")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_value_transfer_within_gas_limit failed");
}

/// Test #46: Value transfer with explicit low gas succeeds.
///
/// The intrinsic gas for a value transfer is 21,000 + EIP-7708 overhead (~1,756).
/// A gas limit of 26,000 should be sufficient.
#[tokio::test]
async fn test_value_transfer_explicit_gas_succeeds() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000007777");
    let value = U256::from(100);

    let mut env = arc_execution_e2e::ArcEnvironment::new();
    arc_execution_e2e::ArcSetup::new()
        .apply(&mut env)
        .await
        .expect("setup failed");

    let mut send = SendTransaction::new("transfer")
        .with_to(recipient)
        .with_value(value)
        .with_gas_limit(26_000);
    arc_execution_e2e::Action::execute(&mut send, &mut env)
        .await
        .expect("send tx");

    let mut produce = arc_execution_e2e::actions::ProduceBlocks::new(1);
    arc_execution_e2e::Action::execute(&mut produce, &mut env)
        .await
        .expect("produce");

    let tx_hash = *env.get_tx_hash("transfer").expect("tx hash");
    let receipt = env
        .node()
        .inner
        .provider()
        .receipt_by_hash(tx_hash)
        .expect("receipt query")
        .expect("receipt not found");

    assert!(
        receipt.success,
        "Value transfer with 26,000 gas should succeed; got reverted. Gas used: {}",
        receipt.cumulative_gas_used,
    );

    // Verify the gas used is reasonable (21,000 intrinsic + log overhead + value transfer cost)
    assert!(
        receipt.cumulative_gas_used > 21_000,
        "Gas used ({}) should exceed intrinsic gas (21,000)",
        receipt.cumulative_gas_used,
    );
}

/// Test #47: Zero value transfer uses baseline gas (no log emission overhead).
///
/// Isolates a zero-value transfer in its own block and verifies it uses exactly
/// the baseline gas (21,000 intrinsic, no EIP-7708 log overhead).
#[tokio::test]
async fn test_zero_value_transfer_baseline_gas() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000006666");

    let mut env = arc_execution_e2e::ArcEnvironment::new();
    arc_execution_e2e::ArcSetup::new()
        .apply(&mut env)
        .await
        .expect("setup failed");

    let mut send = SendTransaction::new("transfer")
        .with_to(recipient)
        .with_value(U256::ZERO);
    arc_execution_e2e::Action::execute(&mut send, &mut env)
        .await
        .expect("send tx");

    let mut produce = arc_execution_e2e::actions::ProduceBlocks::new(1);
    arc_execution_e2e::Action::execute(&mut produce, &mut env)
        .await
        .expect("produce");

    let tx_hash = *env.get_tx_hash("transfer").expect("tx hash");
    let receipt = env
        .node()
        .inner
        .provider()
        .receipt_by_hash(tx_hash)
        .expect("receipt query")
        .expect("receipt not found");

    assert!(receipt.success, "Zero-value transfer should succeed");

    // Under Zero6 (active in localdev), a zero-value call pays intrinsic gas + 1 SLOAD
    // for the caller blocklist check (recipient check is skipped when value is zero).
    let expected_gas = 21_000 + PRECOMPILE_SLOAD_GAS_COST;
    assert_eq!(
        receipt.cumulative_gas_used, expected_gas,
        "Zero-value transfer gas mismatch: expected {} (21k intrinsic + {} blocklist SLOAD), got {}",
        expected_gas, PRECOMPILE_SLOAD_GAS_COST, receipt.cumulative_gas_used,
    );
}
