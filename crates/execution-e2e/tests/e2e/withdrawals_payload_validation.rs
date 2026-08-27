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

//! Withdrawals payload validation e2e tests.
//!
//! Verifies that the Engine API rejects a block proposed by a bad validator
//! with a non-empty withdrawals list, since Arc never applies withdrawals to balances.

use alloy_rpc_types_engine::PayloadStatusEnum;
use alloy_rpc_types_eth::BlockNumberOrTag;
use arc_execution_e2e::{ArcSetup, ArcTestNode};
use eyre::Result;

use super::helpers::payload::{forkchoice_state, mutate_payload, next_payload_attributes};

/// A payload with a non-empty withdrawals list is rejected as INVALID via `engine_newPayload`.
#[tokio::test]
async fn test_payload_with_withdrawals_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();

    let node = ArcTestNode::start(ArcSetup::new()).await?;

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
        payload
            .as_v2_mut()
            .expect("expected ExecutionPayloadV2+")
            .withdrawals = vec![Default::default()];
    })?;

    let status = node.new_payload(payload).await?;

    match status {
        PayloadStatusEnum::Invalid { validation_error } => {
            assert_eq!(validation_error, "Withdrawals are not supported");
        }
        other => panic!("Expected INVALID status for withdrawals payload, got {other:?}"),
    }

    Ok(())
}
