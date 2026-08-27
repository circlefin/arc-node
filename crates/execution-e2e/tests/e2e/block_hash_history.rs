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

//! EIP-2935 BlockHashHistory e2e tests for Arc Chain.
//!
//! Tests that the EIP-2935 system call persists parent block hashes in the
//! history storage contract at `0x0000F90827F1C53a10cb7A02335B175320002935`.
//!
//! The system call runs at the start of each block, storing `parent_hash` in a
//! ring buffer of size 8191.

use alloy_primitives::{address, Address, Bytes};
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionInput, TransactionRequest};
use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_e2e::{chainspec::localdev_with_hardforks, ArcSetup, ArcTestNode, TxKind};
use eyre::Result;
use reth_chainspec::ForkCondition;

/// EIP-2935 History Storage Contract address.
const HISTORY_STORAGE_ADDRESS: Address = address!("0000F90827F1C53a10cb7A02335B175320002935");

/// Helper: encode a block number as 32-byte big-endian calldata for the
/// history storage contract's `get(uint256)` interface.
fn block_number_calldata(block_number: u64) -> Bytes {
    let mut buf = [0u8; 32];
    buf[24..32].copy_from_slice(&block_number.to_be_bytes());
    Bytes::copy_from_slice(&buf)
}

/// After producing blocks, querying the history storage contract for a recent
/// block number should return that block's canonical hash.
#[tokio::test]
async fn test_block_hash_history_returns_canonical_hash_for_recent_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    // Produce block 3 so block 1 and block 2 hashes have been stored by their child blocks.
    node.produce_blocks(3).await?;

    let block_1_hash = node
        .get_block(BlockNumberOrTag::Number(1))
        .await?
        .header
        .hash;
    let history_block_1 = node
        .call(TransactionRequest {
            to: Some(TxKind::Call(HISTORY_STORAGE_ADDRESS)),
            input: TransactionInput::new(block_number_calldata(1)),
            ..Default::default()
        })
        .await?;
    assert_eq!(history_block_1.as_ref(), block_1_hash.as_slice());

    let block_2_hash = node
        .get_block(BlockNumberOrTag::Number(2))
        .await?
        .header
        .hash;
    let history_block_2 = node
        .call(TransactionRequest {
            to: Some(TxKind::Call(HISTORY_STORAGE_ADDRESS)),
            input: TransactionInput::new(block_number_calldata(2)),
            ..Default::default()
        })
        .await?;
    assert_eq!(history_block_2.as_ref(), block_2_hash.as_slice());
    Ok(())
}

/// Querying the history storage contract for a far-future block number should revert.
#[tokio::test]
async fn test_block_hash_history_reverts_for_future_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    node.produce_block().await?;

    let result = node
        .call(TransactionRequest {
            to: Some(TxKind::Call(HISTORY_STORAGE_ADDRESS)),
            input: TransactionInput::new(block_number_calldata(99999)),
            ..Default::default()
        })
        .await;
    assert!(result.is_err());
    Ok(())
}

/// Block-hash history writes use the baseline behavior even before Zero5
/// metadata activation.
///
/// Chain spec: Zero5 activates at block 3.
/// Produce blocks 1-4, then assert the contract returns exact canonical hashes
/// from before activation and at activation.
#[tokio::test]
async fn test_block_hash_history_writes_before_zero5_metadata_activation() -> Result<()> {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, ForkCondition::Block(0)),
        (ArcHardfork::Zero4, ForkCondition::Block(0)),
        (ArcHardfork::Zero5, ForkCondition::Block(3)),
        (ArcHardfork::Zero6, ForkCondition::Block(3)),
    ]);

    let mut node = ArcTestNode::start(ArcSetup::new().with_chain_spec(chain_spec)).await?;
    node.produce_blocks(4).await?;
    assert_eq!(
        node.get_block(BlockNumberOrTag::Latest)
            .await?
            .header
            .number,
        4
    );

    let block_1_hash = node
        .get_block(BlockNumberOrTag::Number(1))
        .await?
        .header
        .hash;
    let before_activation = node
        .call(TransactionRequest {
            to: Some(TxKind::Call(HISTORY_STORAGE_ADDRESS)),
            input: TransactionInput::new(block_number_calldata(1)),
            ..Default::default()
        })
        .await?;
    assert_eq!(before_activation.as_ref(), block_1_hash.as_slice());

    let block_3_hash = node
        .get_block(BlockNumberOrTag::Number(3))
        .await?
        .header
        .hash;
    let at_activation = node
        .call(TransactionRequest {
            to: Some(TxKind::Call(HISTORY_STORAGE_ADDRESS)),
            input: TransactionInput::new(block_number_calldata(3)),
            ..Default::default()
        })
        .await?;
    assert_eq!(at_activation.as_ref(), block_3_hash.as_slice());
    Ok(())
}
