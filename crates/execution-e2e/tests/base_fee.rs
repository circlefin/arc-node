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

use alloy_primitives::{Bytes, U256};
use alloy_rpc_types_engine::PayloadStatusEnum;
use arc_execution_config::{gas_fee::decode_base_fee_from_bytes, hardforks::ArcHardfork};
use arc_execution_e2e::{
    actions::{
        build_payload_for_next_block, set_payload_override_and_rehash, submit_payload,
        ProduceBlocks,
    },
    chainspec::localdev_with_hardforks,
    Action, ArcEnvironment, ArcSetup,
};
use eyre::Result;

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

    let mut env = ArcEnvironment::new();
    ArcSetup::new().apply(&mut env).await?;

    // Produce block 1 so block 2 has a valid parent.
    let mut produce = ProduceBlocks::new(1);
    produce.execute(&mut env).await?;

    // Build block 2 payload and then corrupt extra_data with a wrong 8-byte base fee value.
    let (mut payload, execution_requests, parent_beacon_block_root) =
        build_payload_for_next_block(&env).await?;

    let correct_extra_data = &payload.payload_inner.payload_inner.extra_data;
    let correct_base_fee = decode_base_fee_from_bytes(correct_extra_data)
        .ok_or_else(|| eyre::eyre!("block 2 extra_data does not contain a valid base fee"))?;

    let wrong_base_fee = correct_base_fee.wrapping_add(1);
    let wrong_extra_data: Bytes = wrong_base_fee.to_be_bytes().to_vec().into();

    let mut payload_override = payload.payload_inner.payload_inner.clone();
    payload_override.extra_data = wrong_extra_data;
    set_payload_override_and_rehash(
        &mut payload,
        &execution_requests,
        parent_beacon_block_root,
        payload_override,
    )?;

    let status =
        submit_payload(&env, payload, execution_requests, parent_beacon_block_root).await?;

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

/// arc_validate_header_base_fee enforces absolute bounds on base_fee_per_gas under Zero5.
///
/// - Zero5 active (LOCAL_DEV default): base_fee_per_gas = 0 is below absolute_min = 1
///   → INVALID with "block base fee mismatch" (ConsensusError::BaseFeeDiff).
/// - Zero5 not yet active: same override passes the bounds check (the block may still
///   fail for other reasons such as state root mismatch, but NOT with the bounds error).
#[tokio::test]
async fn test_base_fee_absolute_bounds_enforced_only_after_zero5() -> Result<()> {
    reth_tracing::init_test_tracing();

    // Zero5 active: base_fee_per_gas=0 must be rejected with the bounds error.
    let status = submit_with_base_fee(ArcSetup::new(), U256::ZERO).await?;
    assert!(
        matches!(
            &status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.contains("block base fee mismatch")
        ),
        "Zero5 active: expected INVALID with 'block base fee mismatch', got {status:?}"
    );

    // Zero5 not yet active: the bounds check is skipped — "block base fee mismatch" must not appear.
    let pre_zero5_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 0),
        (ArcHardfork::Zero4, 0),
        (ArcHardfork::Zero5, 10),
    ]);
    let status =
        submit_with_base_fee(ArcSetup::new().with_chain_spec(pre_zero5_spec), U256::ZERO).await?;
    assert!(
        !matches!(
            &status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.contains("block base fee mismatch")
        ),
        "Zero5 inactive: expected bounds check to be skipped, got {status:?}"
    );

    Ok(())
}

/// Produces block 1, then builds block 2 with a base_fee_per_gas that does not match
/// the nextBaseFee encoded in block 1's extra_data, and submits it.
///
/// arc_validate_against_parent_base_fee skips genesis parents (block 0), so block 1
/// must exist before the continuity check can fire.
async fn submit_with_wrong_parent_base_fee(setup: ArcSetup) -> Result<PayloadStatusEnum> {
    let mut env = ArcEnvironment::new();
    setup.apply(&mut env).await?;

    // Produce block 1 — this gives block 2 a non-genesis parent with valid extra_data.
    ProduceBlocks::new(1).execute(&mut env).await?;

    let (mut payload, execution_requests, parent_beacon_block_root) =
        build_payload_for_next_block(&env).await?;

    // The builder sets base_fee_per_gas to match parent's nextBaseFee. Adding 1 breaks continuity.
    let correct = payload.payload_inner.payload_inner.base_fee_per_gas;
    let mut payload_override = payload.payload_inner.payload_inner.clone();
    payload_override.base_fee_per_gas = correct + U256::from(1u64);
    set_payload_override_and_rehash(
        &mut payload,
        &execution_requests,
        parent_beacon_block_root,
        payload_override,
    )?;

    submit_payload(&env, payload, execution_requests, parent_beacon_block_root).await
}

/// Builds a block with `base_fee_per_gas` overridden to the given value and submits it.
async fn submit_with_base_fee(setup: ArcSetup, base_fee: U256) -> Result<PayloadStatusEnum> {
    let mut env = ArcEnvironment::new();
    setup.apply(&mut env).await?;

    let (mut payload, execution_requests, parent_beacon_block_root) =
        build_payload_for_next_block(&env).await?;

    let mut payload_override = payload.payload_inner.payload_inner.clone();
    payload_override.base_fee_per_gas = base_fee;
    set_payload_override_and_rehash(
        &mut payload,
        &execution_requests,
        parent_beacon_block_root,
        payload_override,
    )?;

    submit_payload(&env, payload, execution_requests, parent_beacon_block_root).await
}
