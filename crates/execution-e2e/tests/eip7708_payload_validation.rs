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

use alloy_primitives::U256;
use alloy_rpc_types_engine::PayloadStatusEnum;
use arc_execution_e2e::{
    actions::{
        assert_valid_or_syncing, build_payload_for_next_block, set_payload_override_and_rehash,
        submit_payload, AssertTxIncluded, ProduceBlocks, SendTransaction, TxStatus,
    },
    Action, ArcEnvironment, ArcSetup,
};
use eyre::Result;

/// Test #42: Payload with EIP-7708 Transfer log is accepted as VALID.
#[tokio::test]
async fn test_payload_with_eip7708_log_accepted() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut env = ArcEnvironment::new();
    ArcSetup::new().apply(&mut env).await?;

    // Produce block 1 with a value transfer (triggers EIP-7708 log)
    let mut send = SendTransaction::new("transfer")
        .with_to(alloy_primitives::address!(
            "0x000000000000000000000000000000000000bEEF"
        ))
        .with_value(U256::from(1_000_000));
    send.execute(&mut env).await?;

    let mut produce = ProduceBlocks::new(1);
    produce.execute(&mut env).await?;

    // Verify the tx was included successfully
    let mut assert_included = AssertTxIncluded::new("transfer").expect(TxStatus::Success);
    assert_included.execute(&mut env).await?;

    // Now build the next payload and submit via Engine API
    let (payload, execution_requests, parent_beacon_block_root) =
        build_payload_for_next_block(&env).await?;

    let status =
        submit_payload(&env, payload, execution_requests, parent_beacon_block_root).await?;

    assert_valid_or_syncing(&status, "EIP-7708 payload")?;

    Ok(())
}

/// Test #43: Payload with corrupted stateRoot after EIP-7708 tx is rejected as INVALID.
#[tokio::test]
async fn test_payload_with_corrupted_state_root_rejected() -> Result<()> {
    reth_tracing::init_test_tracing();

    let mut env = ArcEnvironment::new();
    ArcSetup::new().apply(&mut env).await?;

    // Produce block 1 with a value transfer
    let mut send = SendTransaction::new("transfer")
        .with_to(alloy_primitives::address!(
            "0x000000000000000000000000000000000000bEEF"
        ))
        .with_value(U256::from(1_000_000));
    send.execute(&mut env).await?;

    let mut produce = ProduceBlocks::new(1);
    produce.execute(&mut env).await?;

    // Build next payload
    let (mut payload, execution_requests, parent_beacon_block_root) =
        build_payload_for_next_block(&env).await?;

    // Corrupt the state root
    let mut payload_override = payload.payload_inner.payload_inner.clone();
    payload_override.state_root = alloy_primitives::B256::repeat_byte(0xDE);
    set_payload_override_and_rehash(
        &mut payload,
        &execution_requests,
        parent_beacon_block_root,
        payload_override,
    )?;

    let status =
        submit_payload(&env, payload, execution_requests, parent_beacon_block_root).await?;

    assert!(
        matches!(status, PayloadStatusEnum::Invalid { .. }),
        "Expected INVALID status for corrupted state root, got {status:?}"
    );

    Ok(())
}
