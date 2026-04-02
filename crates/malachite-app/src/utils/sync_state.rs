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

use std::time::Duration;

use arc_consensus_types::BlockTimestamp;

/// Represents the synchronization state of the node with the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// The node is still catching up with the network and has not yet reached the latest blocks.
    CatchingUp,
    /// The node is in sync with the network and has reached the latest blocks.
    InSync,
}

impl SyncState {
    /// Returns `true` if the node fell behind, ie. transitioned from `InSync` to `CatchingUp`.
    pub fn fell_behind(previous: SyncState, current: SyncState) -> bool {
        previous == SyncState::InSync && current == SyncState::CatchingUp
    }
}

/// Represents the synchronization state of the node with the network.
///
/// Determined by comparing the timestamp of the latest block with the current time.
/// If the difference is more than or equal to the catch-up threshold, we consider ourselves to be catching up.
pub fn sync_state(
    latest_block_timestamp: BlockTimestamp,
    catch_up_threshold: Duration,
) -> SyncState {
    if is_catching_up(latest_block_timestamp, catch_up_threshold) {
        SyncState::CatchingUp
    } else {
        SyncState::InSync
    }
}

/// Check if we are still catching up with the network
/// by comparing the timestamp of the latest block with the current time.
/// If the difference is more than or equal to CATCH_UP_THRESHOLD, we consider ourselves to be catching up.
fn is_catching_up(latest_block_timestamp: BlockTimestamp, catch_up_threshold: Duration) -> bool {
    // Time elapsed since the new latest block's timestamp
    let elapsed = timestamp_now().saturating_sub(Duration::from_secs(latest_block_timestamp));

    // Check if we are still catching up with the network
    // by comparing the timestamp of the latest block with the current time.
    // If the difference is more than a few seconds, we consider ourselves to be catching up.
    let is_catching_up = elapsed >= catch_up_threshold;

    tracing::debug!(
        ?elapsed,
        is_catching_up,
        "Checking if node is catching up with the network"
    );

    is_catching_up
}

/// Returns the duration since the unix epoch.
fn timestamp_now() -> Duration {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Clock is before UNIX epoch!")
}
