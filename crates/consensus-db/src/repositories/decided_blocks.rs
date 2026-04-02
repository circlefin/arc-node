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

use alloy_rpc_types_engine::ExecutionPayloadV3;
use arc_consensus_types::{Address, ArcContext, Height};
use malachitebft_core_types::CommitCertificate;

use crate::store::{Store, StoreError};
use arc_consensus_types::block::DecidedBlock;

#[cfg_attr(any(test, feature = "mock"), mockall::automock(type Error = std::io::Error;))]
pub trait DecidedBlocksRepository {
    type Error: std::error::Error + Send + Sync + 'static;

    #[allow(dead_code)] // unused for now
    async fn get(&self, height: Height) -> Result<Option<DecidedBlock>, Self::Error>;

    async fn store(
        &self,
        certificate: CommitCertificate<ArcContext>,
        execution_payload: ExecutionPayloadV3,
        proposer: Address,
    ) -> Result<(), Self::Error>;
}

impl<T> DecidedBlocksRepository for &'_ T
where
    T: DecidedBlocksRepository,
{
    type Error = T::Error;

    async fn get(&self, height: Height) -> Result<Option<DecidedBlock>, Self::Error> {
        (*self).get(height).await
    }

    async fn store(
        &self,
        certificate: CommitCertificate<ArcContext>,
        execution_payload: ExecutionPayloadV3,
        proposer: Address,
    ) -> Result<(), Self::Error> {
        (*self)
            .store(certificate, execution_payload, proposer)
            .await
    }
}

impl DecidedBlocksRepository for Store {
    type Error = StoreError;

    async fn get(&self, height: Height) -> Result<Option<DecidedBlock>, Self::Error> {
        self.get_decided_block(height).await
    }

    async fn store(
        &self,
        certificate: CommitCertificate<ArcContext>,
        execution_payload: ExecutionPayloadV3,
        proposer: Address,
    ) -> Result<(), Self::Error> {
        self.store_decided_block(certificate, execution_payload, proposer)
            .await
    }
}
