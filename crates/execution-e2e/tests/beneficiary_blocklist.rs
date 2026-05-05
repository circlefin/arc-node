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
use arc_execution_e2e::{
    actions::{build_payload_for_next_block, set_payload_override_and_rehash, submit_payload},
    chainspec::localdev_with_storage_override,
    ArcEnvironment, ArcSetup,
};
use eyre::Result;

/// Ensure proposer-selected beneficiaries are rejected when blocklisted.
///
/// - Header beneficiary is pre-blocklisted in NativeCoinControl
/// - Payload must be INVALID with blocked-address validation error
#[tokio::test]
async fn test_proposer_selected_blocklisted_beneficiary_is_invalid() -> Result<()> {
    reth_tracing::init_test_tracing();

    let blocklisted_beneficiary = address!("0xbad0000000000000000000000000000000000001");
    let chain_spec = localdev_with_storage_override(Some(blocklisted_beneficiary));

    let mut env = ArcEnvironment::new();
    ArcSetup::new()
        .with_chain_spec(chain_spec)
        .apply(&mut env)
        .await?;

    let (mut payload, execution_requests, parent_beacon_block_root) =
        build_payload_for_next_block(&env).await?;
    let mut payload_override = payload.payload_inner.payload_inner.clone();
    payload_override.fee_recipient = blocklisted_beneficiary;
    set_payload_override_and_rehash(
        &mut payload,
        &execution_requests,
        parent_beacon_block_root,
        payload_override,
    )?;

    let status = submit_payload(&env, payload, execution_requests, parent_beacon_block_root)
        .await
        .expect("submit_payload should return Ok for blocklisted proposer-selected beneficiary");

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
