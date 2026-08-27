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
//! hardfork boundaries for Zero4, Zero5, Zero6, Zero7, and Zero8 hardforks.

use alloy_rpc_types_eth::BlockNumberOrTag;
use arc_execution_config::hardforks::{is_arc_fork_active, ArcHardfork};
use arc_execution_e2e::{chainspec::localdev_with_hardforks, ArcSetup, ArcTestNode};
use eyre::Result;
use reth_chainspec::{ChainSpecProvider, EthereumHardfork, EthereumHardforks, ForkCondition};

#[tokio::test]
async fn test_hardfork_active_at_genesis() -> Result<()> {
    reth_tracing::init_test_tracing();

    let node = ArcTestNode::start(ArcSetup::new()).await?;
    let block = node.get_block(BlockNumberOrTag::Latest).await?;
    let chain_spec = node.node.inner.provider().chain_spec();

    for hardfork in [
        ArcHardfork::Zero3,
        ArcHardfork::Zero4,
        ArcHardfork::Zero5,
        ArcHardfork::Zero6,
        ArcHardfork::Zero7,
        ArcHardfork::Zero8,
    ] {
        assert!(is_arc_fork_active(
            chain_spec.as_ref(),
            hardfork,
            block.header.number,
            block.header.timestamp
        ));
    }
    Ok(())
}

/// Test multiple hardfork transitions in sequence.
#[tokio::test]
async fn test_sequential_hardfork_transitions() -> Result<()> {
    reth_tracing::init_test_tracing();

    let hardforks = [
        (ArcHardfork::Zero3, 2),
        (ArcHardfork::Zero4, 4),
        (ArcHardfork::Zero5, 6),
        (ArcHardfork::Zero6, 8),
        (ArcHardfork::Zero7, 10),
        (ArcHardfork::Zero8, 12),
    ];
    let hardfork_conditions =
        hardforks.map(|(hardfork, block_number)| (hardfork, ForkCondition::Block(block_number)));
    let chain_spec = localdev_with_hardforks(&hardfork_conditions);

    let mut node = ArcTestNode::start(ArcSetup::new().with_chain_spec(chain_spec)).await?;
    let chain_spec = node.node.inner.provider().chain_spec();

    let block = node.get_block(BlockNumberOrTag::Latest).await?;
    for (hardfork, _) in hardforks {
        assert!(!is_arc_fork_active(
            chain_spec.as_ref(),
            hardfork,
            block.header.number,
            block.header.timestamp
        ));
    }

    for (hardfork, block_number) in hardforks {
        node.produce_blocks(2).await?;
        let block = node.get_block(BlockNumberOrTag::Latest).await?;
        assert_eq!(block.header.number, block_number);
        assert!(is_arc_fork_active(
            chain_spec.as_ref(),
            hardfork,
            block.header.number,
            block.header.timestamp
        ));
    }
    Ok(())
}

/// Test that Osaka (Fusaka) hardfork is active on localdev and blocks produce correctly.
///
/// Osaka is a timestamp-based Ethereum hardfork that enables EIP-7212 (P256 precompile),
/// EIP-7934 (RLP block size limit), and other Fusaka EIPs.
#[tokio::test]
async fn test_osaka_active_on_localdev() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let block = node.get_block(BlockNumberOrTag::Latest).await?;
    let chain_spec = node.node.inner.provider().chain_spec();
    assert!(chain_spec
        .is_ethereum_fork_active_at_timestamp(EthereumHardfork::Osaka, block.header.timestamp));
    node.produce_blocks(3).await?;
    let block = node.get_block(BlockNumberOrTag::Latest).await?;
    assert_eq!(block.header.number, 3);
    assert!(chain_spec
        .is_ethereum_fork_active_at_timestamp(EthereumHardfork::Osaka, block.header.timestamp));
    Ok(())
}
