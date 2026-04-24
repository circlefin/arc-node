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

//! EIP-7708 zero address e2e tests.
//!
//! Arc custom behavior: value transfers to Address::ZERO are rejected under Zero5.

mod helpers;

use alloy_primitives::{Address, U256};
use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_e2e::{
    actions::{
        AssertTxIncluded, AssertTxLogs, AssertTxTrace, ProduceBlocks, SendTransaction, TxStatus,
    },
    chainspec::localdev_with_hardforks,
    ArcSetup, ArcTestBuilder,
};

/// Test #24: Send value to Address::ZERO under Zero5 — tx reverts.
#[tokio::test]
async fn test_zero_address_value_transfer_reverts() {
    reth_tracing::init_test_tracing();

    let value = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("zero_addr")
                .with_to(Address::ZERO)
                .with_value(value)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("zero_addr").expect(TxStatus::Reverted))
        .with_action(AssertTxLogs::new("zero_addr").expect_no_logs())
        .with_action(AssertTxTrace::new("zero_addr"))
        .run()
        .await
        .expect("test_zero_address_value_transfer_reverts failed");
}

/// Test #25: Send zero value to Address::ZERO — should succeed (no transfer, no log).
#[tokio::test]
async fn test_zero_address_zero_value_succeeds() {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("zero_addr")
                .with_to(Address::ZERO)
                .with_value(U256::ZERO)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("zero_addr").expect(TxStatus::Success))
        .with_action(AssertTxLogs::new("zero_addr").expect_no_logs())
        .with_action(AssertTxTrace::new("zero_addr"))
        .run()
        .await
        .expect("test_zero_address_zero_value_succeeds failed");
}

/// Test #26: Pre-Zero5 — value transfer to Address::ZERO is not rejected.
#[tokio::test]
async fn test_pre_zero5_zero_address_allowed() {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 0),
        (ArcHardfork::Zero4, 0),
        (ArcHardfork::Zero5, 100),
        (ArcHardfork::Zero6, 100),
    ]);

    let value = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(chain_spec))
        .with_action(
            SendTransaction::new("zero_addr")
                .with_to(Address::ZERO)
                .with_value(value)
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        // Pre-Zero5: zero-address value transfer should succeed
        .with_action(AssertTxIncluded::new("zero_addr").expect(TxStatus::Success))
        .with_action(AssertTxTrace::new("zero_addr"))
        .run()
        .await
        .expect("test_pre_zero5_zero_address_allowed failed");
}
