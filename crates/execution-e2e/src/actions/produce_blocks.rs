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

//! Block production actions for Arc e2e tests.
//!
//! Uses Engine API directly to produce blocks, allowing for empty blocks
//! which is common in testing scenarios.

use super::{assert_valid_or_syncing, build_payload_for_next_block, submit_payload};
use crate::{action::Action, environment::BlockInfo, ArcEnvironment};
use alloy_rpc_types_engine::ForkchoiceState;
use futures_util::future::BoxFuture;
use reth_ethereum::node::EthEngineTypes;
use reth_rpc_api::clients::EngineApiClient;
use tracing::{debug, info};

/// Produces a specified number of blocks using Engine API.
///
/// For each block:
/// 1. Sends forkchoiceUpdated with payload attributes to start building
/// 2. Retrieves payload via getPayload
/// 3. Submits payload via newPayload
/// 4. Updates forkchoice to finalize the block
/// 5. Updates the environment's current block info
#[derive(Debug)]
pub struct ProduceBlocks {
    /// Number of blocks to produce.
    num_blocks: u64,
}

impl ProduceBlocks {
    /// Creates a new ProduceBlocks action.
    pub fn new(num_blocks: u64) -> Self {
        Self { num_blocks }
    }
}

impl Action for ProduceBlocks {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let starting_block = env.block_number();
            info!(
                starting_block,
                num_blocks = self.num_blocks,
                "Producing blocks via Engine API"
            );

            for i in 0..self.num_blocks {
                let current_block = env.current_block().clone();
                let parent_hash = current_block.hash;

                debug!(
                    parent_hash = %parent_hash,
                    parent_number = current_block.number,
                    "Building block on parent"
                );

                // Step 1-2: Build payload via Engine API (FCU + getPayload)
                let (execution_payload, execution_requests, parent_beacon_block_root) =
                    build_payload_for_next_block(env).await?;
                let block_hash = execution_payload.payload_inner.payload_inner.block_hash;
                let block_number = execution_payload.payload_inner.payload_inner.block_number;
                let block_timestamp = execution_payload.payload_inner.payload_inner.timestamp;

                debug!(
                    block_hash = %block_hash,
                    block_number,
                    "Got built payload"
                );

                // Step 3: Submit the payload
                let new_payload_status = submit_payload(
                    env,
                    execution_payload,
                    execution_requests,
                    parent_beacon_block_root,
                )
                .await?;

                debug!("newPayload status: {:?}", new_payload_status);
                assert_valid_or_syncing(&new_payload_status, "newPayload")?;

                // Get the auth server handle from the node
                let node = env.node();
                let auth_server = node.inner.auth_server_handle();
                let engine_client = auth_server.http_client();

                // Step 4: Update forkchoice to make the new block canonical and finalized
                let new_fork_choice = ForkchoiceState {
                    head_block_hash: block_hash,
                    safe_block_hash: block_hash,
                    finalized_block_hash: block_hash,
                };

                let finalize_result = EngineApiClient::<EthEngineTypes>::fork_choice_updated_v3(
                    &engine_client,
                    new_fork_choice,
                    None,
                )
                .await?;

                debug!("Finalize FCU result: {:?}", finalize_result);
                assert_valid_or_syncing(&finalize_result.payload_status.status, "Finalize FCU")?;

                // Step 5: Update environment's current block
                env.set_current_block(BlockInfo::new(block_hash, block_number, block_timestamp));

                info!(
                    block_number,
                    block_hash = %block_hash,
                    iteration = i + 1,
                    "Produced block"
                );
            }

            let final_block = env.block_number();
            info!(
                starting_block,
                final_block,
                blocks_produced = self.num_blocks,
                "Finished producing blocks"
            );

            Ok(())
        })
    }
}
