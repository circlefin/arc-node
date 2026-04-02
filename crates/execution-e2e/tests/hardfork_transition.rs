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

//! Hardfork transition e2e tests for Arc Chain.
//!
//! These tests verify that block production works correctly across
//! hardfork boundaries for Zero4, Zero5, and Zero6 hardforks.

use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_e2e::{
    actions::{AssertBlockNumber, AssertEthereumHardfork, AssertHardfork, ProduceBlocks},
    chainspec::localdev_with_hardforks,
    ArcSetup, ArcTestBuilder,
};
use eyre::Result;
use reth_chainspec::EthereumHardfork;

#[tokio::test]
async fn test_hardfork_active_at_genesis() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero3))
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero4))
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero5))
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero6))
        .run()
        .await
}

/// Test multiple hardfork transitions in sequence.
#[tokio::test]
async fn test_sequential_hardfork_transitions() -> Result<()> {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 2),
        (ArcHardfork::Zero4, 4),
        (ArcHardfork::Zero5, 6),
        (ArcHardfork::Zero6, 8),
    ]);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(chain_spec))
        // At genesis (block 0)
        .with_action(AssertHardfork::is_not_active(ArcHardfork::Zero3))
        .with_action(AssertHardfork::is_not_active(ArcHardfork::Zero4))
        .with_action(AssertHardfork::is_not_active(ArcHardfork::Zero5))
        .with_action(AssertHardfork::is_not_active(ArcHardfork::Zero6))
        // Produce block 1-2 - Zero3 activates
        .with_action(ProduceBlocks::new(2))
        .with_action(AssertBlockNumber::new(2))
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero3))
        // Produce block 3-4 - Zero4 activates
        .with_action(ProduceBlocks::new(2))
        .with_action(AssertBlockNumber::new(4))
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero4))
        // Produce block 5-6 - Zero5 activates
        .with_action(ProduceBlocks::new(2))
        .with_action(AssertBlockNumber::new(6))
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero5))
        // Produce block 7-8 - Zero6 activates
        .with_action(ProduceBlocks::new(2))
        .with_action(AssertBlockNumber::new(8))
        .with_action(AssertHardfork::is_active(ArcHardfork::Zero6))
        .run()
        .await
}

/// Test that Osaka (Fusaka) hardfork is active on localdev and blocks produce correctly.
///
/// Osaka is a timestamp-based Ethereum hardfork that enables EIP-7212 (P256 precompile),
/// EIP-7934 (RLP block size limit), and other Fusaka EIPs.
#[tokio::test]
async fn test_osaka_active_on_localdev() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Verify Osaka is active at genesis on localdev
        .with_action(AssertEthereumHardfork::is_active(EthereumHardfork::Osaka))
        // Produce multiple blocks to confirm block production works with Osaka rules
        .with_action(ProduceBlocks::new(3))
        .with_action(AssertBlockNumber::new(3))
        // Osaka should still be active after producing blocks
        .with_action(AssertEthereumHardfork::is_active(EthereumHardfork::Osaka))
        .run()
        .await
}
