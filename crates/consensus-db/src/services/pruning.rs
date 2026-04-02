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

use arc_consensus_types::{Height, PruningConfig};
use tracing::info;

use crate::store::{Store, StoreError};

#[cfg_attr(any(test, feature = "mock"), mockall::automock(type Error = std::io::Error;))]
pub trait PruningService {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Prune decided blocks.
    ///
    /// # Important
    /// Should be called regardless of whether pruning is enabled.
    ///
    /// As historical decided blocks are fetched from EL, we just keep a minimum number of blocks
    /// in the DB to help with EL's amnesia upon recovery.
    async fn prune_decided_blocks(&self) -> Result<Vec<Height>, Self::Error>;

    /// Prune historical certificates.
    ///
    /// # Important
    /// This will only prune certificates if pruning is enabled.
    ///
    /// # Arguments
    /// - `latest_height`: The latest committed height. Used to determine the effective retain height.
    async fn prune_historical_certs(
        &self,
        latest_height: Height,
    ) -> Result<Vec<Height>, Self::Error>;

    /// Clean up stale consensus data (undecided blocks and pending proposals) for committed heights.
    ///
    /// # Important
    /// Should always be called when committing a block, regardless of pruning configuration.
    ///
    /// # Arguments
    /// - `current_height`: All undecided/pending data with `height <= current_height` will be removed
    async fn clean_stale_consensus_data(&self, current_height: Height) -> Result<(), Self::Error>;
}

impl<T> PruningService for &T
where
    T: PruningService + ?Sized,
{
    type Error = T::Error;

    async fn prune_decided_blocks(&self) -> Result<Vec<Height>, Self::Error> {
        T::prune_decided_blocks(*self).await
    }

    async fn prune_historical_certs(
        &self,
        latest_height: Height,
    ) -> Result<Vec<Height>, Self::Error> {
        T::prune_historical_certs(*self, latest_height).await
    }

    async fn clean_stale_consensus_data(&self, current_height: Height) -> Result<(), Self::Error> {
        T::clean_stale_consensus_data(*self, current_height).await
    }
}

pub struct ProdPruningService<'a> {
    store: &'a Store,
    config: &'a PruningConfig,
}

impl<'a> ProdPruningService<'a> {
    pub fn new(store: &'a Store, config: &'a PruningConfig) -> Self {
        Self { store, config }
    }
}

impl<'a> PruningService for ProdPruningService<'a> {
    type Error = StoreError;

    async fn prune_decided_blocks(&self) -> Result<Vec<Height>, StoreError> {
        self.store.prune_blocks().await
    }

    async fn prune_historical_certs(
        &self,
        latest_height: Height,
    ) -> Result<Vec<Height>, StoreError> {
        if !self.config.enabled() {
            return Ok(Vec::new());
        }

        let retain_height = self.config.effective_certificates_min_height(latest_height);

        info!(height = %latest_height, %retain_height, "Pruning historical data");
        self.store.prune_historical_certs(retain_height).await
    }

    async fn clean_stale_consensus_data(&self, current_height: Height) -> Result<(), StoreError> {
        self.store.clean_stale_consensus_data(current_height).await
    }
}
