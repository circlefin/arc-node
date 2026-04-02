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

use arc_consensus_types::{Height, StoredCommitCertificate};

use crate::store::{Store, StoreError};

#[cfg_attr(any(test, feature = "mock"), mockall::automock(type Error = std::io::Error;))]
pub trait CertificatesRepository {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Get the height of the highest certificate stored.
    async fn max_height(&self) -> Result<Option<Height>, Self::Error>;

    /// Get the commit certificate at the given height.
    async fn get(&self, height: Height) -> Result<Option<StoredCommitCertificate>, Self::Error>;
}

impl<T> CertificatesRepository for &T
where
    T: CertificatesRepository + ?Sized,
{
    type Error = T::Error;

    async fn max_height(&self) -> Result<Option<Height>, Self::Error> {
        (**self).max_height().await
    }

    async fn get(&self, height: Height) -> Result<Option<StoredCommitCertificate>, Self::Error> {
        (**self).get(height).await
    }
}

impl CertificatesRepository for Store {
    type Error = StoreError;

    async fn max_height(&self) -> Result<Option<Height>, StoreError> {
        self.max_height().await
    }

    async fn get(&self, height: Height) -> Result<Option<StoredCommitCertificate>, StoreError> {
        self.get_certificate(Some(height)).await
    }
}
