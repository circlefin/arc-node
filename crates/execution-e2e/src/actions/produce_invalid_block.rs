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

//! Block production with invalid payloads for bad validator simulation.
//!
//! This module provides actions to produce blocks with corrupted fields
//! to test how the execution layer handles invalid blocks.

use crate::{action::Action, actions::payload_utils::is_unsupported_fork_err, ArcEnvironment};
use alloy_eips::eip7685::RequestsOrHash;
use alloy_primitives::B256;
use alloy_rpc_types_engine::{ForkchoiceState, PayloadAttributes, PayloadStatusEnum};
use futures_util::future::BoxFuture;
use reth_ethereum::node::EthEngineTypes;
use reth_rpc_api::clients::EngineApiClient;
use tracing::{debug, info};

/// Produces a block with a corrupted state root.
///
/// This action simulates bad validator behavior by:
/// 1. Building a normal payload via forkchoiceUpdated + getPayload
/// 2. Corrupting the state root to a random value
/// 3. Submitting the corrupted payload via newPayload
/// 4. Expecting the payload to be rejected as INVALID
#[derive(Debug, Default)]
pub struct ProduceInvalidBlock;

impl ProduceInvalidBlock {
    /// Create a new action that produces a block with a corrupted state root.
    pub fn new() -> Self {
        Self
    }
}

impl Action for ProduceInvalidBlock {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let current_block = env.current_block().clone();
            let parent_hash = current_block.hash;
            let parent_timestamp = current_block.timestamp;

            info!(
                parent_hash = %parent_hash,
                parent_number = current_block.number,
                "Producing invalid block with corrupted state root"
            );

            // Get the auth server handle from the node
            let node = env.node();
            let auth_server = node.inner.auth_server_handle();
            let engine_client = auth_server.http_client();

            // Create forkchoice state pointing to current head
            let fork_choice_state = ForkchoiceState {
                head_block_hash: parent_hash,
                safe_block_hash: parent_hash,
                finalized_block_hash: parent_hash,
            };

            // Create payload attributes for the next block
            let next_timestamp = parent_timestamp + 1;
            let payload_attributes = PayloadAttributes {
                timestamp: next_timestamp,
                prev_randao: B256::random(),
                suggested_fee_recipient: alloy_primitives::Address::ZERO,
                withdrawals: Some(vec![]),
                parent_beacon_block_root: Some(B256::ZERO),
            };

            // Step 1: Send FCU with payload attributes to start building
            let fcu_result = EngineApiClient::<EthEngineTypes>::fork_choice_updated_v3(
                &engine_client,
                fork_choice_state,
                Some(payload_attributes),
            )
            .await?;

            debug!("FCU result: {:?}", fcu_result);

            match &fcu_result.payload_status.status {
                PayloadStatusEnum::Valid | PayloadStatusEnum::Syncing => {}
                PayloadStatusEnum::Invalid { validation_error } => {
                    return Err(eyre::eyre!(
                        "FCU returned Invalid status: {:?}",
                        validation_error
                    ));
                }
                status => {
                    return Err(eyre::eyre!("Unexpected FCU status: {:?}", status));
                }
            }

            let payload_id = fcu_result
                .payload_id
                .ok_or_else(|| eyre::eyre!("No payload ID returned from FCU"))?;

            debug!("Got payload ID: {:?}", payload_id);

            // Step 2: Get the built payload
            // Use getPayloadV5 (Osaka) if supported, otherwise fall back to V4 (Prague).
            // V5 is required when Osaka is active; V4 is rejected post-Osaka.
            let (execution_payload, execution_requests) =
                match EngineApiClient::<EthEngineTypes>::get_payload_v5(&engine_client, payload_id)
                    .await
                {
                    Ok(envelope) => (
                        envelope.execution_payload.clone(),
                        envelope.execution_requests.clone(),
                    ),
                    Err(e) => {
                        if !is_unsupported_fork_err(&e) {
                            return Err(eyre::eyre!("getPayloadV5 failed: {e}"));
                        }
                        let envelope = EngineApiClient::<EthEngineTypes>::get_payload_v4(
                            &engine_client,
                            payload_id,
                        )
                        .await?;
                        (
                            envelope.execution_payload.clone(),
                            envelope.execution_requests.clone(),
                        )
                    }
                };

            let mut corrupted_payload = execution_payload;
            let block_hash = corrupted_payload.payload_inner.payload_inner.block_hash;
            let block_number = corrupted_payload.payload_inner.payload_inner.block_number;

            debug!(
                block_hash = %block_hash,
                block_number,
                "Got built payload, applying corruption"
            );

            // Step 3: Corrupt the state root
            let original = corrupted_payload.payload_inner.payload_inner.state_root;
            corrupted_payload.payload_inner.payload_inner.state_root = B256::random();
            info!(
                original_state_root = %original,
                corrupted_state_root = %corrupted_payload.payload_inner.payload_inner.state_root,
                "Corrupted state root"
            );

            // Step 4: Submit the corrupted payload
            let new_payload_result = EngineApiClient::<EthEngineTypes>::new_payload_v4(
                &engine_client,
                corrupted_payload,
                vec![],
                B256::ZERO,
                RequestsOrHash::Requests(execution_requests),
            )
            .await?;

            debug!("newPayload result: {:?}", new_payload_result);

            // Step 5: Expect the payload to be rejected as invalid
            match &new_payload_result.status {
                PayloadStatusEnum::Invalid { validation_error } => {
                    info!(
                        ?validation_error,
                        block_number, "Block correctly rejected as invalid"
                    );
                    Ok(())
                }
                status => Err(eyre::eyre!(
                    "Expected newPayload to return INVALID, but got {:?}",
                    status
                )),
            }
        })
    }
}
