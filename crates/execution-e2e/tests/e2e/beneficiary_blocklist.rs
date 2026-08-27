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

//! E2E test covering beneficiary blocklist enforcement during payload validation.

use alloy_primitives::address;
use alloy_rpc_types_engine::PayloadStatusEnum;
use alloy_rpc_types_eth::BlockNumberOrTag;
use arc_execution_e2e::{chainspec::localdev_with_storage_override, ArcSetup, ArcTestNode};
use eyre::Result;

use super::helpers::payload::{forkchoice_state, mutate_payload, next_payload_attributes};

/// Ensure proposer-selected beneficiaries are rejected when blocklisted.
///
/// - Header beneficiary is pre-blocklisted in NativeCoinControl
/// - Payload must be INVALID with blocked-address validation error
#[tokio::test]
async fn test_proposer_selected_blocklisted_beneficiary_is_invalid() -> Result<()> {
    reth_tracing::init_test_tracing();

    let blocklisted_beneficiary = address!("0xbad0000000000000000000000000000000000001");
    let chain_spec = localdev_with_storage_override(Some(blocklisted_beneficiary));

    let node = ArcTestNode::start(ArcSetup::new().with_chain_spec(chain_spec)).await?;

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
        payload.as_v1_mut().fee_recipient = blocklisted_beneficiary;
    })?;

    let status = node
        .new_payload(payload)
        .await
        .expect("new_payload should return Ok for blocklisted proposer-selected beneficiary");

    assert!(
        matches!(
            &status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.to_ascii_lowercase().contains("blocked address")
        ),
        "Expected INVALID with blocked-address validation error, got {:?}",
        status
    );

    Ok(())
}
