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

//! Regression test for the shared sparse-trie payload-builder state root.
//!
//! With `--engine.share-sparse-trie-with-payload-builder` enabled, the payload
//! builder seals each block with the state root precomputed by the engine's
//! shared sparse-trie task. That root is only correct if every state change the
//! block makes, including Arc's unconditional end-of-block SystemAccounting
//! write performed in `ArcBlockExecutor::finish`, is streamed to the task
//! before the root is fixed.
//!
//! The earlier ordering detached the state hook and resolved the root *before*
//! `executor.finish()` ran, so the SystemAccounting storage delta never reached
//! the task and the sealed header carried an incomplete root. `newPayload`
//! re-executes with the hook attached, recomputes the complete root, and
//! rejects the block as `mismatched block state root`, which crash-looped
//! every proposer at height 1.
//!
//! `ArcTestNode::produce_block` drives FCU -> getPayload -> newPayload -> FCU
//! and asserts `newPayload` is valid. So producing blocks with the flag ON is itself the
//! equality check between the build-time root and the re-execution root: the
//! action only succeeds when they match. This test exercises that path on an
//! empty block (the exact `./quake start` failure shape), on a block carrying a
//! transaction, and across several heights (anchored-trie reuse).

use alloy_primitives::{address, Address, U256};
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionRequest};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;

const RECIPIENT: Address = address!("0x000000000000000000000000000000000000bEEF");

#[tokio::test]
async fn sparse_trie_payload_builds_blocks_with_matching_state_root() -> Result<()> {
    reth_tracing::init_test_tracing();

    let value = U256::from(100u64) * U256::from(10u64).pow(U256::from(18u64));

    let mut node = ArcTestNode::start(ArcSetup::new().with_share_sparse_trie(true)).await?;
    let signer = node.wallet_signer(0)?;

    // Empty block 1: the exact shape that crash-looped `./quake start`.
    // Even with no transactions, `ArcBlockExecutor::finish` writes the
    // per-block SystemAccounting gas-accounting slot, so the build-time
    // root must include it for `newPayload` to accept the block.
    node.produce_block().await?;
    let block = node.get_block(BlockNumberOrTag::Latest).await?;
    assert_eq!(block.header.number, 1);

    // A block carrying a transfer: both the transfer and the per-block
    // SystemAccounting write must be reflected in the shared-trie root.
    let tx_hash = node
        .send_tx(
            signer.clone(),
            TransactionRequest {
                from: Some(signer.address()),
                to: Some(TxKind::Call(RECIPIENT)),
                value: Some(value),
                ..Default::default()
            },
        )
        .await?;
    node.produce_block().await?;
    let receipt = node.get_receipt(tx_hash).await?;
    assert!(receipt.status());
    assert_eq!(node.balance(RECIPIENT, None).await?, value);

    // Several more heights to exercise anchored-trie reuse across blocks.
    node.produce_blocks(3).await?;
    let block = node.get_block(BlockNumberOrTag::Latest).await?;
    assert_eq!(block.header.number, 5);

    Ok(())
}
