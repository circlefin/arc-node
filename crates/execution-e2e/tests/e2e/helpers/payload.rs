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

use alloy_primitives::B256;
use alloy_rpc_types_engine::{ExecutionData, ExecutionPayload, ForkchoiceState, PayloadAttributes};
use arc_execution_e2e::DEFAULT_FEE_RECIPIENT;

/// Builds default payload attributes for the next block.
pub fn next_payload_attributes(parent_timestamp: u64) -> PayloadAttributes {
    PayloadAttributes {
        timestamp: parent_timestamp + 1,
        prev_randao: B256::random(),
        suggested_fee_recipient: DEFAULT_FEE_RECIPIENT,
        withdrawals: Some(vec![]),
        parent_beacon_block_root: Some(B256::ZERO),
        slot_number: None,
    }
}

/// Builds a forkchoice state where head, safe, and finalized point to the same block.
pub fn forkchoice_state(block_hash: B256) -> ForkchoiceState {
    ForkchoiceState {
        head_block_hash: block_hash,
        safe_block_hash: block_hash,
        finalized_block_hash: block_hash,
    }
}

/// Mutates the execution payload and recomputes the block hash.
pub fn mutate_payload(
    data: &mut ExecutionData,
    mutate: impl FnOnce(&mut ExecutionPayload),
) -> eyre::Result<()> {
    mutate(&mut data.payload);
    rehash_payload(data)
}

fn rehash_payload(data: &mut ExecutionData) -> eyre::Result<()> {
    let block_hash = data.clone().into_block_raw()?.hash_slow();
    data.payload.as_v1_mut().block_hash = block_hash;
    Ok(())
}
