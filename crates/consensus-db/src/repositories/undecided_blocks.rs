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
use arc_consensus_types::block::ConsensusBlock;

#[cfg_attr(any(test, feature = "mock"), mockall::automock(type Error = std::io::Error;))]
pub trait UndecidedBlocksRepository {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Get the undecided block for the given height, round, and block hash.
    ///
    /// # Arguments
    /// - `height`: The height to get the undecided block for.
    /// - `round`: The round to get the undecided block for.
    async fn get(
        &self,
        height: Height,
        round: Round,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error>;

    /// Get the undecided block for the given height and block hash (ignoring round).
    /// Returns the first undecided block found that matches the height and block hash.
    ///
    /// # Arguments
    /// - `height`: The height to get the undecided block for.
    /// - `block_hash`: The block hash to get the undecided block for.
    async fn get_first(
        &self,
        height: Height,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error>;

    /// Store the undecided block.
    ///
    /// # Arguments
    /// - `block`: The block to store.
    async fn store(&self, block: ConsensusBlock) -> Result<(), Self::Error>;
}

impl<T> UndecidedBlocksRepository for &'_ T
where
    T: UndecidedBlocksRepository,
{
    type Error = T::Error;

    async fn get(
        &self,
        height: Height,
        round: Round,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error> {
        (*self).get(height, round, block_hash).await
    }

    async fn get_first(
        &self,
        height: Height,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error> {
        (*self).get_first(height, block_hash).await
    }

    async fn store(&self, block: ConsensusBlock) -> Result<(), Self::Error> {
        (*self).store(block).await
    }
}

impl UndecidedBlocksRepository for Store {
    type Error = StoreError;

    async fn get(
        &self,
        height: Height,
        round: Round,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error> {
        self.get_undecided_block(height, round, block_hash).await
    }

    async fn get_first(
        &self,
        height: Height,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error> {
        self.get_undecided_block_by_height_and_block_hash(height, block_hash)
            .await
    }

    async fn store(&self, block: ConsensusBlock) -> Result<(), Self::Error> {
        self.store_undecided_block(block).await
    }
}
