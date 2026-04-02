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

//! E2E test covering beneficiary mismatch handling during payload validation.

use alloy_primitives::{address, Address};
use alloy_rpc_types_engine::PayloadStatusEnum;
use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_e2e::{
    actions::{build_payload_for_next_block, set_payload_override_and_rehash, submit_payload},
    chainspec::{localdev_with_hardforks, localdev_with_storage_override},
    ArcEnvironment, ArcSetup,
};
use eyre::Result;

async fn submit_with_overridden_beneficiary(
    setup: ArcSetup,
    header_beneficiary: Address,
    submit_ok_msg: &str,
) -> Result<PayloadStatusEnum> {
    let mut env = ArcEnvironment::new();
    setup.apply(&mut env).await?;

    let (mut payload, execution_requests, parent_beacon_block_root) =
        build_payload_for_next_block(&env).await?;
    let mut payload_override = payload.payload_inner.payload_inner.clone();
    payload_override.fee_recipient = header_beneficiary;
    set_payload_override_and_rehash(
        &mut payload,
        &execution_requests,
        parent_beacon_block_root,
        payload_override,
    )?;

    let status = submit_payload(&env, payload, execution_requests, parent_beacon_block_root)
        .await
        .expect(submit_ok_msg);

    Ok(status)
}

/// Ensure `apply_pre_execution_changes` rejects payloads whose beneficiary does
/// not match `ProtocolConfig.rewardBeneficiary()`.
/// The LOCAL_DEV genesis has rewardBeneficiary set to 0xa0Ee7A142d267C1f36714E4a8F75612F20a79720.
#[tokio::test]
async fn test_beneficiary_mismatch_rejected_with_error() -> Result<()> {
    reth_tracing::init_test_tracing();

    let status = submit_with_overridden_beneficiary(
        ArcSetup::new(),
        address!("0xbad0000000000000000000000000000000000000"),
        "submit_payload should return Ok for beneficiary mismatch",
    )
    .await?;

    assert!(
        matches!(
            &status,
            PayloadStatusEnum::Invalid { validation_error }
                if validation_error.to_ascii_lowercase().contains("beneficiary")
        ),
        "Expected INVALID with beneficiary-related validation error, got {:?}",
        status
    );

    Ok(())
}

/// Ensure beneficiary mismatch is still accepted before Zero5 hardfork activates.
#[tokio::test]
async fn test_beneficiary_mismatch_before_zero5_is_valid() -> Result<()> {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 0),
        (ArcHardfork::Zero4, 0),
        (ArcHardfork::Zero5, 10),
    ]);
    let status = submit_with_overridden_beneficiary(
        ArcSetup::new().with_chain_spec(chain_spec),
        address!("0xbad0000000000000000000000000000000000002"),
        "submit_payload should return Ok before Zero5",
    )
    .await?;

    assert!(
        matches!(status, PayloadStatusEnum::Valid),
        "Expected VALID for beneficiary mismatch before Zero5, got {:?}",
        status
    );

    Ok(())
}

/// Ensure beneficiary validation is skipped when ProtocolConfig.rewardBeneficiary() returns zero.
///
/// - When ProtocolConfig.rewardBeneficiary() returns zero address
/// - Header beneficiary mismatch check is skipped
/// - A non-blocklisted header beneficiary is accepted (proposer can set their own address)
#[tokio::test]
async fn test_validation_skipped_when_expected_beneficiary_is_zero() -> Result<()> {
    reth_tracing::init_test_tracing();

    // setting ProtocolConfig.rewardBeneficiary() to zero
    let chain_spec = localdev_with_storage_override(Address::ZERO, None);

    let status = submit_with_overridden_beneficiary(
        ArcSetup::new().with_chain_spec(chain_spec),
        address!("0xbad0000000000000000000000000000000000001"),
        "submit_payload should return Ok when expected beneficiary is zero",
    )
    .await?;
    assert!(
        matches!(status, PayloadStatusEnum::Valid),
        "Expected VALID/SYNCING when expected beneficiary is zero, got {:?}",
        status
    );
    Ok(())
}

/// Ensure proposer-selected beneficiaries are still rejected when blocklisted.
///
/// - ProtocolConfig.rewardBeneficiary() returns zero address
/// - Header beneficiary is proposer-selected and pre-blocklisted in NativeCoinControl
/// - Payload must be INVALID with blocked-address validation error
#[tokio::test]
async fn test_proposer_selected_blocklisted_beneficiary_is_invalid() -> Result<()> {
    reth_tracing::init_test_tracing();

    let blocklisted_beneficiary = address!("0xbad0000000000000000000000000000000000001");
    let chain_spec = localdev_with_storage_override(Address::ZERO, Some(blocklisted_beneficiary));

    let status = submit_with_overridden_beneficiary(
        ArcSetup::new().with_chain_spec(chain_spec),
        blocklisted_beneficiary,
        "submit_payload should return Ok for blocklisted proposer-selected beneficiary",
    )
    .await?;

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
