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
use tracing::{debug, error, warn};

use arc_consensus_types::Height;
use arc_eth_engine::engine::Engine;
use malachitebft_app_channel::Reply;

use crate::state::State;

/// Handles the `GetHistoryMinHeight` message from the consensus engine.
///
/// This is called when the consensus engine requests the minimum height of the application's
/// history. The application retrieves the earliest height from the consensus store and the
/// earliest available block from the execution layer, taking the maximum of the two as the
/// floor. If a target halt height is configured and is less than the latest height, it further
/// caps the minimum height to the target halt height. This ensures that the consensus engine
/// does not request history at heights where either the CL store or EL no longer has data,
/// and does not go below the configured halt height which typically corresponds to a hard fork
/// at the consensus level.
pub async fn handle(state: &State, engine: &Engine, reply: Reply<Height>) -> eyre::Result<()> {
    let cl_earliest_height = state
        .store()
        .min_height()
        .await
        .wrap_err("Failed to get earliest height from the store")?
        .unwrap_or_default();

    let cl_latest_height = state
        .store()
        .max_height()
        .await
        .wrap_err("Failed to get latest height from the store")?
        .unwrap_or_default();

    let el_earliest_height = match engine.eth.get_block_by_number("earliest").await {
        Ok(Some(block)) => Height::new(block.block_number),
        Ok(None) => {
            warn!("EL returned no block for 'earliest', falling back to CL-only");
            cl_earliest_height
        }
        Err(e) => {
            warn!("Failed to get EL earliest block, falling back to CL-only: {e:#}");
            cl_earliest_height
        }
    };

    let halt_height = state.env_config().halt_height;

    debug!(
        %cl_earliest_height,
        %el_earliest_height,
        %cl_latest_height,
        halt_height = halt_height.map(|h| h.to_string()).unwrap_or_else(|| "None".to_string()),
        "History bounds"
    );

    let min_height = get_history_min_height(
        cl_earliest_height,
        el_earliest_height,
        cl_latest_height,
        halt_height,
    );

    if let Err(e) = reply.send(min_height) {
        error!("🔴 Failed to send reply: {e:?}");
    }

    Ok(())
}

pub fn get_history_min_height(
    cl_earliest_height: Height,
    el_earliest_height: Height,
    cl_latest_height: Height,
    target_halt_height: Option<Height>,
) -> Height {
    let floor = cl_earliest_height.max(el_earliest_height);

    if let Some(halt_height) = target_halt_height
        && halt_height < cl_latest_height
    {
        floor.max(halt_height)
    } else {
        floor
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
        // No halt height configured, CL and EL agree
        assert_eq!(get_history_min_height(h(10), h(10), h(100), None), h(10));

        // Halt height greater than max decided block height
        assert_eq!(
            get_history_min_height(h(10), h(10), h(100), Some(h(150))),
            h(10)
        );

        // Halt height less than max decided block height, but greater than earliest height
        assert_eq!(
            get_history_min_height(h(10), h(10), h(100), Some(h(50))),
            h(50)
        );

        // Halt height less than both max decided block height and earliest height
        assert_eq!(
            get_history_min_height(h(60), h(60), h(100), Some(h(50))),
            h(60)
        );
    }

    #[test]
    fn test_el_earliest_higher_than_cl() {
        // EL has pruned blocks, so its earliest is higher than CL's
        assert_eq!(get_history_min_height(h(10), h(50), h(100), None), h(50));
    }

    #[test]
    fn test_cl_earliest_higher_than_el() {
        // CL earliest is higher than EL earliest
        assert_eq!(get_history_min_height(h(50), h(10), h(100), None), h(50));
    }

    #[test]
    fn test_el_earliest_with_halt_height() {
        // EL earliest (50) > halt height (30), halt height < latest (100)
        // floor = max(10, 50) = 50, then max(50, 30) = 50
        assert_eq!(
            get_history_min_height(h(10), h(50), h(100), Some(h(30))),
            h(50)
        );

        // EL earliest (20) < halt height (50), halt height < latest (100)
        // floor = max(10, 20) = 20, then max(20, 50) = 50
        assert_eq!(
            get_history_min_height(h(10), h(20), h(100), Some(h(50))),
            h(50)
        );
    }

    #[test]
    fn test_halt_height_equals_latest_height() {
        // halt_height == cl_latest_height: halt has not been passed yet, so it is ignored
        assert_eq!(
            get_history_min_height(h(10), h(10), h(100), Some(h(100))),
            h(10)
        );
    }
}
