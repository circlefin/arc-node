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

//! E2E tests for NativeCoinControl blocklist 2×SLOAD gas (Zero6 hardfork).
//!
//! The Zero6 hardfork adds extra intrinsic gas for blocklist SLOAD checks:
//! - Value transfer (tx.value > 0): +4,200 gas (2 SLOADs: caller + recipient)
//! - Zero-value call (tx.value == 0): +2,100 gas (1 SLOAD: caller only)
//!
//! These costs are enforced during EVM execution in `validate_initial_tx_gas`.
//! The standard pool validator does not know about Arc-specific SLOAD costs,
//! so transactions with insufficient gas enter the pool but are skipped by the
//! payload builder during block construction.

use alloy_primitives::U256;
use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_e2e::{
    actions::{AssertTxIncluded, AssertTxNotIncluded, ProduceBlocks, SendTransaction, TxStatus},
    chainspec::localdev_with_hardforks,
    ArcSetup, ArcTestBuilder,
};
use eyre::Result;
use rstest::rstest;

/// Tests Zero6 blocklist SLOAD gas accounting through the full node stack.
///
/// With Zero6 active (default localdev):
/// - Value transfer requires: base (21,000) + 2 SLOADs (4,200) = 25,200 gas
/// - Zero-value call requires: base (21,000) + 1 SLOAD (2,100) = 23,100 gas
///
/// Sufficient gas → tx included in block.
/// Insufficient gas → tx enters pool but payload builder skips it.
#[rstest]
#[case::value_transfer_sufficient_gas(25_200, 1, true)]
#[case::value_transfer_insufficient_gas(25_199, 1, false)]
#[case::zero_value_call_sufficient_gas(23_100, 0, true)]
#[case::zero_value_call_insufficient_gas(23_099, 0, false)]
#[tokio::test]
async fn test_zero6_blocklist_sload_gas(
    #[case] gas_limit: u64,
    #[case] value: u64,
    #[case] should_be_included: bool,
) -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut builder = ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("tx")
                .with_gas_limit(gas_limit)
                .with_value(U256::from(value)),
        )
        .with_action(ProduceBlocks::new(1));

    builder = if should_be_included {
        builder.with_action(AssertTxIncluded::new("tx").expect(TxStatus::Success))
    } else {
        builder.with_action(AssertTxNotIncluded::new("tx"))
    };

    builder.run().await
}

/// Pre-Zero6: no extra SLOAD gas is charged, so 21,000 is enough for a value transfer.
///
/// With Zero6 at block 100 (not yet active), standard intrinsic gas (21,000) suffices.
#[tokio::test]
async fn test_pre_zero6_no_extra_gas() -> Result<()> {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 0),
        (ArcHardfork::Zero4, 0),
        (ArcHardfork::Zero5, 0),
        (ArcHardfork::Zero6, 100),
    ]);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(chain_spec))
        .with_action(
            SendTransaction::new("tx")
                .with_gas_limit(21_000)
                .with_value(U256::from(1)),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("tx").expect(TxStatus::Success))
        .run()
        .await
}
