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

use alloy_primitives::B256;
use alloy_rpc_types_engine::PayloadStatusEnum;
use alloy_rpc_types_eth::BlockNumberOrTag;
use arc_execution_e2e::{ArcSetup, ArcTestNode};
use eyre::Result;

use super::helpers::payload::{forkchoice_state, mutate_payload, next_payload_attributes};

/// Test produce a single block.
#[tokio::test]
async fn test_produce_single_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    node.produce_block().await?;
    assert_eq!(
        node.get_block(BlockNumberOrTag::Latest)
            .await?
            .header
            .number,
        1
    );
    Ok(())
}

/// Test produce multiple blocks.
#[tokio::test]
async fn test_incremental_block_production() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    node.produce_blocks(3).await?;
    assert_eq!(
        node.get_block(BlockNumberOrTag::Latest)
            .await?
            .header
            .number,
        3
    );
    node.produce_blocks(2).await?;
    assert_eq!(
        node.get_block(BlockNumberOrTag::Latest)
            .await?
            .header
            .number,
        5
    );
    Ok(())
}

/// Test that blocks with corrupted state root are rejected.
#[tokio::test]
async fn test_block_with_corrupted_state_root_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    node.produce_block().await?;
    assert_eq!(
        node.get_block(BlockNumberOrTag::Latest)
            .await?
            .header
            .number,
        1
    );

    let parent = node.get_block(BlockNumberOrTag::Latest).await?;
    let fork_choice_state = forkchoice_state(parent.header.hash);
    let payload_attributes = next_payload_attributes(parent.header.timestamp);
    let fcu_result = node
        .fork_choice_updated(fork_choice_state, Some(payload_attributes))
        .await?;
    let payload_id = fcu_result
        .payload_id
        .ok_or_else(|| eyre::eyre!("forkChoiceUpdated did not return a payload ID"))?;
    let mut payload = node.get_payload(payload_id).await?;
    mutate_payload(&mut payload, |payload| {
        payload.as_v1_mut().state_root = B256::random();
    })?;
    let status = node.new_payload(payload).await?;
    assert!(matches!(status, PayloadStatusEnum::Invalid { .. }));
    assert_eq!(
        node.get_block(BlockNumberOrTag::Latest)
            .await?
            .header
            .number,
        1
    );
    Ok(())
}
