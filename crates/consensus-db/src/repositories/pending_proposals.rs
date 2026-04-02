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

use arc_consensus_types::{BlockHash, Height, Round};

use crate::store::{Store, StoreError};

#[cfg_attr(any(test, feature = "mock"), mockall::automock(type Error = std::io::Error;))]
pub trait PendingProposalsRepository {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Enforce pending proposals limit on startup.
    /// Clean up any excess proposals from previous runs.
    /// Removes all proposals outside the valid range and trims to max_pending_proposals.
    async fn enforce_limit(
        &self,
        max_pending_proposals: usize,
        current_height: Height,
    ) -> Result<Vec<(Height, Round, BlockHash)>, Self::Error>;

    /// Return the total number of stored pending proposal parts.
    async fn count(&self) -> Result<usize, StoreError>;
}

impl<T> PendingProposalsRepository for &T
where
    T: PendingProposalsRepository + ?Sized,
{
    type Error = T::Error;

    async fn enforce_limit(
        &self,
        max_pending_proposals: usize,
        current_height: Height,
    ) -> Result<Vec<(Height, Round, BlockHash)>, Self::Error> {
        (**self)
            .enforce_limit(max_pending_proposals, current_height)
            .await
    }

    async fn count(&self) -> Result<usize, StoreError> {
        (**self).count().await
    }
}

impl PendingProposalsRepository for Store {
    type Error = StoreError;

    async fn enforce_limit(
        &self,
        max_pending_proposals: usize,
        current_height: Height,
    ) -> Result<Vec<(Height, Round, BlockHash)>, StoreError> {
        self.enforce_pending_proposals_limit(max_pending_proposals, current_height)
            .await
    }

    async fn count(&self) -> Result<usize, StoreError> {
        self.get_pending_proposal_parts_count().await
    }
}
