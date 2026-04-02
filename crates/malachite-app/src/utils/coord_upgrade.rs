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

use std::time::Duration;

use arc_consensus_types::Height;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::store::Store;

#[derive(Debug, thiserror::Error)]
#[error("Halt and wait for external termination signal")]
pub struct HaltAndWait;

/// Check if the next height matches the configured halt height.
pub async fn check_halt_height(
    store: &Store,
    next_height: Height,
    halt_height: Option<Height>,
) -> eyre::Result<()> {
    const SLEEP_BEFORE_HALT: Duration = Duration::from_secs(10);

    if let Some(height) = halt_height {
        if height == next_height {
            warn!("Next height matches configured halt height {height}");

            // Create a savepoint in the database to ensure that
            // no repair of the database is needed on restart.
            store.savepoint();

            info!("Sleeping {SLEEP_BEFORE_HALT:?} before halting...");
            sleep(Duration::from_secs(10)).await;

            return Err(HaltAndWait.into());
        }
    }

    Ok(())
}
