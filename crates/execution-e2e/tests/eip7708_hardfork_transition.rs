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

//! EIP-7708 hardfork transition e2e tests.
//!
//! Verifies that EIP-7708 Transfer logs activate correctly at the Zero5 boundary
//! and that pre-Zero5 blocks emit NativeCoinTransferred logs from
//! NATIVE_COIN_AUTHORITY_ADDRESS instead.

mod helpers;

use alloy_primitives::{address, U256};
use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_e2e::{
    actions::{
        AssertBlockNumber, AssertHardfork, AssertTxIncluded, AssertTxLogs, AssertTxTrace,
        ProduceBlocks, SendTransaction, TxStatus,
    },
    chainspec::localdev_with_hardforks,
    ArcSetup, ArcTestBuilder,
};
use helpers::constants::{NATIVE_COIN_AUTHORITY_ADDRESS, SYSTEM_ADDRESS, WALLET_FIRST_ADDRESS};

/// Test #20: Pre-Zero5 value transfer emits NativeCoinTransferred from NativeCoinAuthority.
///
/// Verifies exact event format: topic[0] = NativeCoinTransferred signature,
/// topic[1] = from, topic[2] = to, data = amount.
#[tokio::test]
async fn test_pre_zero5_emits_native_coin_transferred() {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 0),
        (ArcHardfork::Zero4, 0),
        (ArcHardfork::Zero5, 100), // far in the future
        (ArcHardfork::Zero6, 100),
    ]);

    let recipient = address!("0x000000000000000000000000000000000000bEEF");
    let value = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(chain_spec))
        .with_action(AssertHardfork::is_not_active(ArcHardfork::Zero5))
        .with_action(
            SendTransaction::new("transfer")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("transfer")
                .expect_log_count(1)
                .expect_emitter_at(0, NATIVE_COIN_AUTHORITY_ADDRESS)
                // Verify exact NativeCoinTransferred event topics and data
                .expect_native_coin_transferred_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_pre_zero5_emits_native_coin_transferred failed");
}

/// Test #21: Zero5 activation boundary — tx before activation uses old log format,
/// tx after activation uses EIP-7708 format.
///
/// Pre-Zero5 tx emits NativeCoinTransferred with exact topics/data;
/// post-Zero5 tx emits ERC-20 Transfer with exact topics/data.
#[tokio::test]
async fn test_zero5_activation_boundary() {
    reth_tracing::init_test_tracing();

    // Zero5 activates at block 3
    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 0),
        (ArcHardfork::Zero4, 0),
        (ArcHardfork::Zero5, 3),
        (ArcHardfork::Zero6, 100),
    ]);

    let recipient = address!("0x000000000000000000000000000000000000bEEF");
    let value = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(chain_spec))
        .with_action(AssertHardfork::is_not_active(ArcHardfork::Zero5))
        // Send tx before Zero5 (block 1)
        .with_action(
            SendTransaction::new("pre_zero5")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertBlockNumber::new(1))
        .with_action(AssertTxIncluded::new("pre_zero5").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("pre_zero5")
                .expect_log_count(1)
                .expect_emitter_at(0, NATIVE_COIN_AUTHORITY_ADDRESS)
                // Exact pre-Zero5 event format
                .expect_native_coin_transferred_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        // Produce blocks 2-3 to reach Zero5 activation
        .with_action(ProduceBlocks::new(2))
        .with_action(AssertBlockNumber::new(3))
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero5))
        // Send tx after Zero5 (block 4)
        .with_action(
            SendTransaction::new("post_zero5")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("post_zero5").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("post_zero5")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .with_action(AssertTxTrace::new("pre_zero5"))
        .with_action(AssertTxTrace::new("post_zero5"))
        .run()
        .await
        .expect("test_zero5_activation_boundary failed");
}

/// Test #22: Post-Zero5 value transfer emits Transfer from SYSTEM_ADDRESS.
#[tokio::test]
async fn test_post_zero5_emits_eip7708_transfer() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x000000000000000000000000000000000000bEEF");
    let value = U256::from(1_000_000);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero5))
        .with_action(
            SendTransaction::new("transfer")
                .with_to(recipient)
                .with_value(value),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("transfer").expect(TxStatus::Success))
        .with_action(
            AssertTxLogs::new("transfer")
                .expect_log_count(1)
                .expect_emitter_at(0, SYSTEM_ADDRESS)
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_post_zero5_emits_eip7708_transfer failed");
}

/// Test #23: Verify Zero5 hardfork is active at genesis on default localdev.
#[tokio::test]
async fn test_zero5_active_at_genesis() {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero5))
        .run()
        .await
        .expect("test_zero5_active_at_genesis failed");
}
