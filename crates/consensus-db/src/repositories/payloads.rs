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
use arc_consensus_types::Height;

use crate::store::{Store, StoreError};

#[cfg_attr(any(test, feature = "mock"), mockall::automock(type Error = std::io::Error;))]
pub trait PayloadsRepository {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn get(&self, height: Height) -> Result<Option<ExecutionPayloadV3>, Self::Error>;
}

impl<T> PayloadsRepository for &'_ T
where
    T: PayloadsRepository,
{
    type Error = T::Error;

    async fn get(&self, height: Height) -> Result<Option<ExecutionPayloadV3>, Self::Error> {
        (*self).get(height).await
    }
}

impl PayloadsRepository for Store {
    type Error = StoreError;

    async fn get(&self, height: Height) -> Result<Option<ExecutionPayloadV3>, Self::Error> {
        self.get_payload(height).await
    }
}
