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

//! Basic block production e2e tests for Arc Chain.

use arc_execution_e2e::{
    actions::{AssertBlockNumber, ProduceBlocks, ProduceInvalidBlock},
    ArcSetup, ArcTestBuilder,
};
use eyre::Result;

/// Test produce a single block.
#[tokio::test]
async fn test_produce_single_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertBlockNumber::new(1))
        .run()
        .await
}

/// Test produce multiple blocks.
#[tokio::test]
async fn test_incremental_block_production() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(ProduceBlocks::new(3))
        .with_action(AssertBlockNumber::new(3))
        .with_action(ProduceBlocks::new(2))
        .with_action(AssertBlockNumber::new(5))
        .run()
        .await
}

/// Test that blocks with corrupted state root are rejected.
#[tokio::test]
async fn test_block_with_corrupted_state_root_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Produce a valid block first
        .with_action(ProduceBlocks::new(1))
        .with_action(AssertBlockNumber::new(1))
        // Try to produce a block with corrupted state root - should be rejected
        .with_action(ProduceInvalidBlock::new())
        .with_action(AssertBlockNumber::new(1))
        .run()
        .await
}
