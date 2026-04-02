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

//! EIP-2935 BlockHashHistory e2e tests for Arc Chain.
//!
//! Tests that the EIP-2935 system call persists parent block hashes in the
//! history storage contract at `0x0000F90827F1C53a10cb7A02335B175320002935`.
//!
//! The system call runs at the start of each block (Zero5+), storing
//! `parent_hash` in a ring buffer of size 8191.

use alloy_primitives::{address, Address, Bytes};
use arc_execution_config::hardforks::ArcHardfork;
use arc_execution_e2e::{
    actions::{AssertBlockNumber, CallContract, ProduceBlocks},
    chainspec::localdev_with_hardforks,
    ArcSetup, ArcTestBuilder,
};
use eyre::Result;

/// EIP-2935 History Storage Contract address.
const HISTORY_STORAGE_ADDRESS: Address = address!("0000F90827F1C53a10cb7A02335B175320002935");

/// Helper: encode a block number as 32-byte big-endian calldata for the
/// history storage contract's `get(uint256)` interface.
fn block_number_calldata(block_number: u64) -> Bytes {
    let mut buf = [0u8; 32];
    buf[24..32].copy_from_slice(&block_number.to_be_bytes());
    Bytes::copy_from_slice(&buf)
}

/// After producing blocks, querying the history storage contract for a recent
/// block number should return a non-zero hash (the parent hash written by the
/// EIP-2935 system call).
#[tokio::test]
async fn test_block_hash_history_returns_non_zero_for_recent_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        // Produce 3 blocks so block 1 and 2 have parent hashes stored.
        .with_action(ProduceBlocks::new(3))
        // Query block 1: the system call at block 1 stores the genesis hash.
        .with_action(
            CallContract::new("block_hash_history_block_1")
                .to(HISTORY_STORAGE_ADDRESS)
                .with_data(block_number_calldata(1))
                .expect_non_zero_result(),
        )
        // Query block 2: the system call at block 2 stores block 1's hash.
        .with_action(
            CallContract::new("block_hash_history_block_2")
                .to(HISTORY_STORAGE_ADDRESS)
                .with_data(block_number_calldata(2))
                .expect_non_zero_result(),
        )
        .run()
        .await
}

/// Querying the history storage contract for a far-future block number should
/// return zero (no hash stored).
#[tokio::test]
async fn test_block_hash_history_returns_zero_for_future_block() -> Result<()> {
    reth_tracing::init_test_tracing();

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new())
        .with_action(ProduceBlocks::new(1))
        // Query a block number far in the future — should return zero.
        .with_action(
            CallContract::new("block_hash_history_future_block")
                .to(HISTORY_STORAGE_ADDRESS)
                .with_data(block_number_calldata(99999))
                .expect_revert(),
        )
        .run()
        .await
}

/// Crossing the Zero5 activation boundary: hashes must start being written at the
/// activation block and not before.
///
/// Chain spec: Zero5 activates at block 3.
/// - Produce blocks 1-2: pre-Zero5, no entries written.
/// - Produce blocks 3-4: Zero5 active, system call runs.
/// - Assert: slot for block 1 is zero (pre-activation, no system call wrote there).
/// - Assert: contract has an entry for block 3 (first Zero5 block).
#[tokio::test]
async fn test_block_hash_history_starts_at_zero5_activation() -> Result<()> {
    reth_tracing::init_test_tracing();

    let chain_spec = localdev_with_hardforks(&[
        (ArcHardfork::Zero3, 0),
        (ArcHardfork::Zero4, 0),
        (ArcHardfork::Zero5, 3),
        (ArcHardfork::Zero6, 3),
    ]);

    ArcTestBuilder::new()
        .with_setup(ArcSetup::new().with_chain_spec(chain_spec))
        .with_action(ProduceBlocks::new(4))
        .with_action(AssertBlockNumber::new(4))
        // Block 1 is pre-Zero5 — no system call wrote to slot 1, expect zero.
        .with_action(
            CallContract::new("block_hash_history_pre_activation")
                .to(HISTORY_STORAGE_ADDRESS)
                .with_data(block_number_calldata(1))
                .expect_result(Bytes::from([0u8; 32])),
        )
        // Block 3 is the first Zero5 block — system call ran, expect non-zero.
        .with_action(
            CallContract::new("block_hash_history_at_activation")
                .to(HISTORY_STORAGE_ADDRESS)
                .with_data(block_number_calldata(3))
                .expect_non_zero_result(),
        )
        .run()
        .await
}
