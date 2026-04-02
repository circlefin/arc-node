// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
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

use crate::invalid_payloads::InvalidPayload;
use crate::store::{Store, StoreError};

/// Repository for persisting invalid payload records.
///
/// This trait abstracts the storage of [`InvalidPayload`] entries so that callers
/// (and their tests) are not coupled to the concrete [`Store`] type.
#[cfg_attr(any(test, feature = "mock"), mockall::automock(type Error = std::io::Error;))]
pub trait InvalidPayloadsRepository {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Appends an invalid-payload record to the store.
    ///
    /// Creates the underlying collection for the payload's height if one does not
    /// already exist.
    async fn append(&self, invalid_payload: InvalidPayload) -> Result<(), Self::Error>;
}

impl<T> InvalidPayloadsRepository for &'_ T
where
    T: InvalidPayloadsRepository,
{
    type Error = T::Error;

    async fn append(&self, invalid_payload: InvalidPayload) -> Result<(), Self::Error> {
        (*self).append(invalid_payload).await
    }
}

impl InvalidPayloadsRepository for Store {
    type Error = StoreError;

    async fn append(&self, invalid_payload: InvalidPayload) -> Result<(), Self::Error> {
        self.append_invalid_payload(invalid_payload).await
    }
}
