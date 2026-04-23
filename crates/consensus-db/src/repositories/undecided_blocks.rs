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

    /// Gets the undecided block with the given height, round, and block hash.
    /// Returns `None` if no such block exists.
    async fn get_by_round_and_hash(
        &self,
        height: Height,
        round: Round,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error>;

    /// Gets all undecided blocks for the given height and round.
    async fn get_by_round(
        &self,
        height: Height,
        round: Round,
    ) -> Result<Vec<ConsensusBlock>, Self::Error>;

    /// Gets the undecided block with the given height and block hash.
    /// Scans across all rounds, returning the first match found.
    /// Returns `None` if no such block exists.
    async fn get_by_hash(
        &self,
        height: Height,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error>;

    /// Stores the undecided block.
    async fn store_undecided_block(&self, block: ConsensusBlock) -> Result<(), Self::Error>;
}

impl<T> UndecidedBlocksRepository for &'_ T
where
    T: UndecidedBlocksRepository,
{
    type Error = T::Error;

    async fn get_by_round_and_hash(
        &self,
        height: Height,
        round: Round,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error> {
        (*self)
            .get_by_round_and_hash(height, round, block_hash)
            .await
    }

    async fn get_by_round(
        &self,
        height: Height,
        round: Round,
    ) -> Result<Vec<ConsensusBlock>, Self::Error> {
        (*self).get_by_round(height, round).await
    }

    async fn get_by_hash(
        &self,
        height: Height,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error> {
        (*self).get_by_hash(height, block_hash).await
    }

    async fn store_undecided_block(&self, block: ConsensusBlock) -> Result<(), Self::Error> {
        (*self).store_undecided_block(block).await
    }
}

impl UndecidedBlocksRepository for Store {
    type Error = StoreError;

    async fn get_by_round_and_hash(
        &self,
        height: Height,
        round: Round,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error> {
        self.get_undecided_block(height, round, block_hash).await
    }

    async fn get_by_round(
        &self,
        height: Height,
        round: Round,
    ) -> Result<Vec<ConsensusBlock>, Self::Error> {
        self.get_undecided_blocks(height, round).await
    }

    async fn get_by_hash(
        &self,
        height: Height,
        block_hash: BlockHash,
    ) -> Result<Option<ConsensusBlock>, Self::Error> {
        self.get_undecided_block_by_height_and_block_hash(height, block_hash)
            .await
    }

    async fn store_undecided_block(&self, block: ConsensusBlock) -> Result<(), Self::Error> {
        self.store_undecided_block(block).await
    }
}
