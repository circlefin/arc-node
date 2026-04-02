// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
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

use eyre::Context;
use tracing::{debug, error};

use arc_consensus_types::Height;
use malachitebft_app_channel::Reply;

use crate::state::State;

/// Handles the `GetHistoryMinHeight` message from the consensus engine.
///
/// This is called when the consensus engine requests the minimum height of the application's
/// history. The application retrieves the earliest height from its state, and if a target halt
/// height is configured and is less than the latest height, it caps the minimum height to the
/// target halt height. This ensures that the consensus engine does not request history below the
/// configured halt height which typically corresponds to hard fork at the consensus level.
pub async fn handle(state: &State, reply: Reply<Height>) -> eyre::Result<()> {
    let earliest_height = state
        .store()
        .min_height()
        .await
        .wrap_err("Failed to get earliest height from the store")?
        .unwrap_or_default();

    let latest_height = state
        .store()
        .max_height()
        .await
        .wrap_err("Failed to get latest height from the store")?
        .unwrap_or_default();

    let halt_height = state.env_config().halt_height;
    debug!(min_height = ?earliest_height, max_height = ?latest_height, halt_height = ?halt_height, "GetHistoryMinHeight: min/max heights");
    let min_height = get_history_min_height(earliest_height, latest_height, halt_height);

    if let Err(e) = reply.send(min_height) {
        error!("🔴 GetHistoryMinHeight: Failed to send reply: {e:?}");
    }

    Ok(())
}

pub fn get_history_min_height(
    earliest_height: Height,
    latest_height: Height,
    target_halt_height: Option<Height>,
) -> Height {
    match target_halt_height {
        Some(halt_height) if halt_height < latest_height => earliest_height.max(halt_height),
        _ => earliest_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn h(n: u64) -> Height {
        Height::new(n)
    }

    #[test]
    fn test_get_history_min_height() {
        // No halt height configured
        assert_eq!(get_history_min_height(h(10), h(100), None), h(10));

        // Halt height greater than max decided block height
        assert_eq!(get_history_min_height(h(10), h(100), Some(h(150))), h(10));

        // Halt height less than max decided block height, but greater than earliest height
        assert_eq!(get_history_min_height(h(10), h(100), Some(h(50))), h(50));

        // Halt height less than both max decided block height and earliest height
        assert_eq!(get_history_min_height(h(60), h(100), Some(h(50))), h(60));
    }
}
