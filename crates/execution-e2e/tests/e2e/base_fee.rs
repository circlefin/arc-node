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

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use alloy_primitives::{Bytes, U256};
use alloy_rpc_types_engine::PayloadStatusEnum;
use alloy_rpc_types_eth::BlockNumberOrTag;
use arc_execution_config::{gas_fee::decode_base_fee_from_bytes, hardforks::ArcHardfork};
use arc_execution_e2e::{chainspec::localdev_with_hardforks, ArcSetup, ArcTestNode};
use eyre::Result;
use reth_chainspec::ForkCondition;

use super::helpers::payload::{forkchoice_state, mutate_payload, next_payload_attributes};

// ADR-0004 encodes the next block's required base fee in parent's `extra_data` (8 bytes).
// Two independent checks enforce this on every new block:
//
//   Check A — consensus layer (consensus.rs: arc_validate_against_parent_base_fee), fires first:
//     parent.extra_data (decoded) == child.base_fee_per_gas
//     "the header's base_fee_per_gas must match what the parent promised"
//
//   Check B — execution layer (executor.rs: validate_extra_data_base_fee), fires during execution:
//     child.extra_data (decoded) == freshly_computed_nextBaseFee
//     "the extra_data you encoded for your child must match what I compute"
//
// The tests below isolate each check by corrupting a different field:
//   test_parent_child_base_fee_continuity_rejected                     → corrupts base_fee_per_gas  → trips Check A
//   test_incorrect_extra_data_base_fee_rejected_as_invalid_payload     → corrupts extra_data        → trips Check B

/// Check A: arc_validate_against_parent_base_fee (consensus layer).
///
/// Corrupts `base_fee_per_gas` on block 2 so it no longer matches the `nextBaseFee`
/// stored in block 1's `extra_data`. Rejected before execution with "block base fee mismatch".
///
/// Unlike the absolute bounds check (which is Zero5-gated), this check fires from Zero4
/// onwards whenever the parent's extra_data decodes as a valid 8-byte base fee.
/// It skips only when the parent is genesis (block 0), so block 1 must be produced first.
#[tokio::test]
async fn test_parent_child_base_fee_continuity_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();

    // Wrong base_fee_per_gas must be rejected with the continuity error.
    let status = submit_with_wrong_parent_base_fee(ArcSetup::new()).await?;
    assert!(
        matches!(
            &status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.contains("block base fee mismatch")
        ),
        "Expected INVALID with 'block base fee mismatch', got {status:?}"
    );

    Ok(())
}

/// Check B: validate_extra_data_base_fee (execution layer).
///
/// Corrupts `extra_data` on block 2 so it encodes a wrong `nextBaseFee` (for block 3).
/// `base_fee_per_gas` is left correct, so Check A passes. Rejected during execution
/// with "extra_data base fee mismatch".
#[tokio::test]
async fn test_incorrect_extra_data_base_fee_rejected_as_invalid_payload() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut node = ArcTestNode::start(ArcSetup::new()).await?;

    // Produce block 1 so block 2 has a valid parent.
    node.produce_block().await?;

    // Build block 2 payload and then corrupt extra_data with a wrong 8-byte base fee value.
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

    let correct_extra_data = &payload.payload.as_v1().extra_data;
    let correct_base_fee = decode_base_fee_from_bytes(correct_extra_data)
        .ok_or_else(|| eyre::eyre!("block 2 extra_data does not contain a valid base fee"))?;

    let wrong_base_fee = correct_base_fee.wrapping_add(1);
    let wrong_extra_data: Bytes = wrong_base_fee.to_be_bytes().to_vec().into();

    mutate_payload(&mut payload, |payload| {
        payload.as_v1_mut().extra_data = wrong_extra_data;
    })?;

    let status = node.new_payload(payload).await?;

    assert!(
        matches!(
            &status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.contains("extra_data base fee mismatch")
        ),
        "Expected INVALID with 'extra_data base fee mismatch' error, got {status:?}"
    );

    Ok(())
}

/// arc_validate_header_base_fee enforces absolute bounds on base_fee_per_gas.
///
/// Even when chain metadata schedules Zero5 later, base_fee_per_gas = 0 is
/// below absolute_min = 1 and must be INVALID with "block base fee mismatch"
/// (ConsensusError::BaseFeeDiff).
#[tokio::test]
async fn test_base_fee_absolute_bounds_enforced_before_zero5_activation() -> Result<()> {
    reth_tracing::init_test_tracing();

    // Default localdev: base_fee_per_gas=0 must be rejected with the bounds error.
    let status = submit_with_base_fee(ArcSetup::new(), U256::ZERO).await?;
    assert!(
        matches!(
            &status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.contains("block base fee mismatch")
        ),
        "default localdev: expected INVALID with 'block base fee mismatch', got {status:?}"
    );

    // Delayed Zero5 metadata: the baseline still enforces the bounds error.
    let delayed_zero5_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, ForkCondition::Block(0)),
        (ArcHardfork::Zero4, ForkCondition::Block(0)),
        (ArcHardfork::Zero5, ForkCondition::Block(10)),
    ]);
    let status = submit_with_base_fee(
        ArcSetup::new().with_chain_spec(delayed_zero5_spec),
        U256::ZERO,
    )
    .await?;
    assert!(
        matches!(
            &status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.contains("block base fee mismatch")
        ),
        "delayed Zero5 metadata: expected INVALID with 'block base fee mismatch', got {status:?}"
    );

    Ok(())
}

/// Produces block 1, then builds block 2 with a base_fee_per_gas that does not match
/// the nextBaseFee encoded in block 1's extra_data, and submits it.
///
/// arc_validate_against_parent_base_fee skips genesis parents (block 0), so block 1
/// must exist before the continuity check can fire.
async fn submit_with_wrong_parent_base_fee(setup: ArcSetup) -> Result<PayloadStatusEnum> {
    let mut node = ArcTestNode::start(setup).await?;

    // Produce block 1 — this gives block 2 a non-genesis parent with valid extra_data.
    node.produce_block().await?;

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

    // The builder sets base_fee_per_gas to match parent's nextBaseFee. Adding 1 breaks continuity.
    let correct = payload.payload.as_v1().base_fee_per_gas;
    mutate_payload(&mut payload, |payload| {
        payload.as_v1_mut().base_fee_per_gas = correct + U256::from(1u64);
    })?;

    node.new_payload(payload).await
}

/// Builds a block with `base_fee_per_gas` overridden to the given value and submits it.
async fn submit_with_base_fee(setup: ArcSetup, base_fee: U256) -> Result<PayloadStatusEnum> {
    let node = ArcTestNode::start(setup).await?;

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
        payload.as_v1_mut().base_fee_per_gas = base_fee;
    })?;

    node.new_payload(payload).await
}
