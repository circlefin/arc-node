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

//! Shared payload utilities for e2e tests and actions.

use crate::ArcEnvironment;
use alloy_eips::eip7685::{Requests, RequestsOrHash};
use alloy_primitives::{Address, B256};
use alloy_rpc_types_engine::{
    CancunPayloadFields, ExecutionData, ExecutionPayload, ExecutionPayloadSidecar,
    ExecutionPayloadV1, ExecutionPayloadV3, ForkchoiceState, PayloadAttributes, PayloadStatusEnum,
    PraguePayloadFields,
};
use reth_ethereum::node::EthEngineTypes;
use reth_rpc_api::clients::EngineApiClient;

/// JSON-RPC error code for "Unsupported Fork" per the Engine API spec.
const UNSUPPORTED_FORK_CODE: i32 = -38005;

/// Returns `true` if the error is a JSON-RPC "Unsupported Fork" (-38005) response.
pub(crate) fn is_unsupported_fork_err(err: &jsonrpsee::core::client::Error) -> bool {
    matches!(err, jsonrpsee::core::client::Error::Call(obj) if obj.code() == UNSUPPORTED_FORK_CODE)
}

/// Builds a payload for the next block and returns:
/// `(execution_payload, execution_requests, parent_beacon_block_root)`.
pub async fn build_payload_for_next_block(
    env: &ArcEnvironment,
) -> eyre::Result<(ExecutionPayloadV3, Requests, B256)> {
    build_payload_for_next_block_with_client(
        &env.node().inner.auth_server_handle().http_client(),
        env.current_block(),
    )
    .await
}

/// Builds a payload for the next block using the given engine client and parent block info.
/// Used when the node is created outside ArcEnvironment (e.g. NodeBuilder with custom config).
pub(crate) async fn build_payload_for_next_block_with_client<C: EngineApiClient<EthEngineTypes>>(
    engine_client: &C,
    current_block: &crate::environment::BlockInfo,
) -> eyre::Result<(ExecutionPayloadV3, Requests, B256)> {
    let parent_hash = current_block.hash;
    let parent_beacon_block_root = B256::ZERO;

    let fork_choice_state = ForkchoiceState {
        head_block_hash: parent_hash,
        safe_block_hash: parent_hash,
        finalized_block_hash: parent_hash,
    };

    let payload_attributes = PayloadAttributes {
        timestamp: current_block.timestamp + 1,
        prev_randao: B256::random(),
        suggested_fee_recipient: Address::ZERO,
        withdrawals: Some(vec![]),
        parent_beacon_block_root: Some(parent_beacon_block_root),
    };

    let fcu_result = EngineApiClient::<EthEngineTypes>::fork_choice_updated_v3(
        engine_client,
        fork_choice_state,
        Some(payload_attributes),
    )
    .await?;
    assert_valid_or_syncing(
        &fcu_result.payload_status.status,
        "forkChoiceUpdated while building payload",
    )?;

    let payload_id = fcu_result
        .payload_id
        .ok_or_else(|| eyre::eyre!("No payload ID returned from forkChoiceUpdated"))?;

    // Use getPayloadV5 (Osaka) if supported, otherwise fall back to V4 (Prague).
    // V5 is required when Osaka is active; V4 is rejected post-Osaka.
    let (execution_payload, execution_requests) =
        match EngineApiClient::<EthEngineTypes>::get_payload_v5(engine_client, payload_id).await {
            Ok(envelope) => (
                envelope.execution_payload.clone(),
                envelope.execution_requests.clone(),
            ),
            Err(e) => {
                if !is_unsupported_fork_err(&e) {
                    return Err(eyre::eyre!("getPayloadV5 failed: {e}"));
                }
                let envelope =
                    EngineApiClient::<EthEngineTypes>::get_payload_v4(engine_client, payload_id)
                        .await?;
                (
                    envelope.execution_payload.clone(),
                    envelope.execution_requests.clone(),
                )
            }
        };

    Ok((
        execution_payload,
        execution_requests,
        parent_beacon_block_root,
    ))
}

/// Mutates payload and recomputes block hash with full sidecar context.
pub fn set_payload_override_and_rehash(
    payload: &mut ExecutionPayloadV3,
    execution_requests: &Requests,
    parent_beacon_block_root: B256,
    payload_override: ExecutionPayloadV1,
) -> eyre::Result<()> {
    payload.payload_inner.payload_inner = payload_override;

    let sidecar = ExecutionPayloadSidecar::v4(
        CancunPayloadFields::new(parent_beacon_block_root, vec![]),
        PraguePayloadFields::new(execution_requests.clone()),
    );
    payload.payload_inner.payload_inner.block_hash =
        ExecutionData::new(ExecutionPayload::V3(payload.clone()), sidecar)
            .into_block_raw()?
            .hash_slow();

    Ok(())
}

/// Submits a payload via `engine_newPayloadV4` and returns the status.
pub async fn submit_payload(
    env: &ArcEnvironment,
    payload: ExecutionPayloadV3,
    execution_requests: Requests,
    parent_beacon_block_root: B256,
) -> eyre::Result<PayloadStatusEnum> {
    submit_payload_with_client(
        &env.node().inner.auth_server_handle().http_client(),
        payload,
        execution_requests,
        parent_beacon_block_root,
    )
    .await
}

/// Submits a payload via `engine_newPayloadV4` using the given engine client.
/// Used when the node is created outside ArcEnvironment (e.g. NodeBuilder with custom config).
pub(crate) async fn submit_payload_with_client<C: EngineApiClient<EthEngineTypes>>(
    engine_client: &C,
    payload: ExecutionPayloadV3,
    execution_requests: Requests,
    parent_beacon_block_root: B256,
) -> eyre::Result<PayloadStatusEnum> {
    let result = EngineApiClient::<EthEngineTypes>::new_payload_v4(
        engine_client,
        payload,
        vec![],
        parent_beacon_block_root,
        RequestsOrHash::Requests(execution_requests),
    )
    .await;

    match result {
        Ok(response) => Ok(response.status),
        Err(err) => Err(err.into()),
    }
}

/// Validates that payload status is either VALID or SYNCING.
pub fn assert_valid_or_syncing(status: &PayloadStatusEnum, context: &str) -> eyre::Result<()> {
    match status {
        PayloadStatusEnum::Valid | PayloadStatusEnum::Syncing => Ok(()),
        PayloadStatusEnum::Invalid { validation_error } => Err(eyre::eyre!(
            "{context} returned INVALID: {validation_error}"
        )),
        status => Err(eyre::eyre!(
            "{context} returned unexpected status: {:?}",
            status
        )),
    }
}
