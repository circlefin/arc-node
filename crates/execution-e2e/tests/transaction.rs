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

//! Transaction sending e2e tests for Arc Chain.

use alloy_primitives::{address, bytes};
use arc_execution_e2e::{
    actions::{AssertTxIncluded, ProduceBlocks, SendTransaction, TxStatus},
    ArcSetup, ArcTestBuilder,
};
use eyre::Result;

/// Test sending multiple transactions in a single block.
#[tokio::test]
async fn test_multiple_transactions() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(SendTransaction::new("tx1"))
        .with_action(SendTransaction::new("tx2"))
        .with_action(ProduceBlocks::new(1))
        .with_action(SendTransaction::new("tx3"))
        .with_action(ProduceBlocks::new(1))
        .with_action(
            AssertTxIncluded::new("tx1")
                .in_block(1)
                .expect(TxStatus::Success),
        )
        .with_action(
            AssertTxIncluded::new("tx2")
                .in_block(1)
                .expect(TxStatus::Success),
        )
        .with_action(
            AssertTxIncluded::new("tx3")
                .in_block(2)
                .expect(TxStatus::Success),
        )
        .run()
        .await
}

/// Test that a contract call that reverts is detected.
#[tokio::test]
async fn test_reverted_transaction() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(
            SendTransaction::new("tx1")
                .with_to(address!("0x3600000000000000000000000000000000000000"))
                .with_data(bytes!("0x1234abcd"))
                .with_gas_limit(100_000),
        )
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertTxIncluded::new("tx1").expect(TxStatus::Reverted))
        .run()
        .await
}
