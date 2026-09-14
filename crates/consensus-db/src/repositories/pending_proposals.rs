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
    async fn count(&self) -> Result<usize, Self::Error>;
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

    async fn count(&self) -> Result<usize, Self::Error> {
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

    async fn count(&self) -> Result<usize, Self::Error> {
        self.get_pending_proposal_parts_count().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("count failed")]
    struct CountError;

    /// An implementor whose error type is not `StoreError`, which is the point
    /// of the associated `Error` type.
    struct FailingCount;

    impl PendingProposalsRepository for FailingCount {
        type Error = CountError;

        async fn enforce_limit(
            &self,
            _max_pending_proposals: usize,
            _current_height: Height,
        ) -> Result<Vec<(Height, Round, BlockHash)>, Self::Error> {
            Ok(Vec::new())
        }

        async fn count(&self) -> Result<usize, Self::Error> {
            Err(CountError)
        }
    }

    #[tokio::test]
    async fn count_reports_the_implementor_error_type() {
        let repo = FailingCount;
        assert!(matches!(repo.count().await, Err(CountError)));

        // The blanket impl for `&T` forwards the same error type.
        let by_ref = &repo;
        assert!(matches!(by_ref.count().await, Err(CountError)));
    }
}
