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

//! EIP-7708 log format compliance e2e tests.
//!
//! Verifies the byte-level ERC-20 Transfer log format: emitter address,
//! topic[0] (event signature), topic[1] (from), topic[2] (to), data (value).

mod helpers;

use alloy_primitives::{address, Bytes, B256, U256};
use arc_execution_e2e::{
    actions::{
        AssertTxIncluded, AssertTxLogs, AssertTxTrace, ProduceBlocks, SendTransaction, TxStatus,
    },
    ArcSetup, ArcTestBuilder,
};
use helpers::constants::{SYSTEM_ADDRESS, TRANSFER_EVENT_SIGNATURE, WALLET_FIRST_ADDRESS};

/// Test #37: topic[0] matches ERC-20 Transfer(address,address,uint256) signature.
#[tokio::test]
async fn test_transfer_log_topic0_matches_erc20_signature() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000001111");
    let value = U256::from(1_000_000);

    // Build expected topics and data
    let expected_topics = vec![
        TRANSFER_EVENT_SIGNATURE,
        B256::left_padding_from(WALLET_FIRST_ADDRESS.as_slice()),
        B256::left_padding_from(recipient.as_slice()),
    ];
    let expected_data = Bytes::from(value.to_be_bytes::<32>().to_vec());

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
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
                .expect_log_at(0, SYSTEM_ADDRESS, expected_topics, expected_data),
        )
        .with_action(AssertTxTrace::new("transfer"))
        .run()
        .await
        .expect("test_transfer_log_topic0_matches_erc20_signature failed");
}

/// Test #38: topic[1] encodes sender address as left-padded bytes32.
#[tokio::test]
async fn test_transfer_log_topic1_encodes_sender() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000002222");
    let value = U256::from(42);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
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
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .run()
        .await
        .expect("test_transfer_log_topic1_encodes_sender failed");
}

/// Test #39: topic[2] encodes recipient address as left-padded bytes32.
#[tokio::test]
async fn test_transfer_log_topic2_encodes_recipient() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000003333");
    let value = U256::from(999);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
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
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .run()
        .await
        .expect("test_transfer_log_topic2_encodes_recipient failed");
}

/// Test #40: data encodes value as big-endian uint256.
#[tokio::test]
async fn test_transfer_log_data_encodes_value() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000004444");
    // Use a distinctive value to verify encoding
    let value = U256::from(0xDEADBEEFu64);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
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
                .expect_transfer_event(0, WALLET_FIRST_ADDRESS, recipient, value),
        )
        .run()
        .await
        .expect("test_transfer_log_data_encodes_value failed");
}

/// Test #41: emitter address is SYSTEM_ADDRESS, not the sender or NativeCoinAuthority.
#[tokio::test]
async fn test_transfer_log_emitter_is_system_address() {
    reth_tracing::init_test_tracing();

    let recipient = address!("0x0000000000000000000000000000000000005555");
    let value = U256::from(1);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
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
                .expect_emitter_at(0, SYSTEM_ADDRESS),
        )
        .run()
        .await
        .expect("test_transfer_log_emitter_is_system_address failed");
}
