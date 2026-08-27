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

//! EIP-7708 payload validation e2e tests.
//!
//! Verifies that Engine API accepts payloads containing EIP-7708 Transfer logs
//! and rejects payloads with corrupted state roots.

use alloy_primitives::{address, B256, U256};
use alloy_rpc_types_engine::PayloadStatusEnum;
use alloy_rpc_types_eth::{BlockNumberOrTag, TransactionRequest};
use arc_execution_e2e::{ArcSetup, ArcTestNode, TxKind};
use eyre::Result;

use super::helpers::{
    payload::{forkchoice_state, mutate_payload, next_payload_attributes},
    utils::send_and_mine,
};

/// Test #42: Payload with EIP-7708 Transfer log is accepted as VALID.
#[tokio::test]
async fn test_payload_with_eip7708_log_accepted() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;

    // Produce block 1 with a value transfer (triggers EIP-7708 log)
    let receipt = send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(address!(
                "0x000000000000000000000000000000000000bEEF"
            ))),
            value: Some(U256::from(1_000_000)),
            ..Default::default()
        },
    )
    .await?;
    assert!(receipt.status());

    // Now build the next payload and submit via Engine API.
    let parent = node.get_block(BlockNumberOrTag::Latest).await?;
    let fork_choice_state = forkchoice_state(parent.header.hash);
    let payload_attributes = next_payload_attributes(parent.header.timestamp);
    let fcu_result = node
        .fork_choice_updated(fork_choice_state, Some(payload_attributes))
        .await?;
    let payload_id = fcu_result
        .payload_id
        .ok_or_else(|| eyre::eyre!("forkChoiceUpdated did not return a payload ID"))?;
    let payload = node.get_payload(payload_id).await?;

    let status = node.new_payload(payload).await?;
    assert!(matches!(status, PayloadStatusEnum::Valid));

    Ok(())
}

/// Test #43: Payload with corrupted stateRoot after EIP-7708 tx is rejected as INVALID.
#[tokio::test]
async fn test_payload_with_corrupted_state_root_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;
    let signer = node.wallet_signer(0)?;
    // Produce block 1 with a value transfer
    send_and_mine(
        &mut node,
        signer.clone(),
        TransactionRequest {
            from: Some(signer.address()),
            to: Some(TxKind::Call(address!(
                "0x000000000000000000000000000000000000bEEF"
            ))),
            value: Some(U256::from(1_000_000)),
            ..Default::default()
        },
    )
    .await?;

    // Build next payload.
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

    // Corrupt the state root
    mutate_payload(&mut payload, |payload| {
        payload.as_v1_mut().state_root = B256::repeat_byte(0xDE);
    })?;

    let status = node.new_payload(payload).await?;

    assert!(
        matches!(status, PayloadStatusEnum::Invalid { .. }),
        "Expected INVALID status for corrupted state root, got {status:?}"
    );

    Ok(())
}
