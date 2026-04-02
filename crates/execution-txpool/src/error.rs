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

use alloy_primitives::Address;
use reth_transaction_pool::error::PoolTransactionError;
use std::any::Any;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArcTransactionValidatorError {
    #[error("Blocked address")]
    BlocklistedError,
    #[error("Transaction in invalid tx list")]
    InvalidTxError,
    #[error("Address {0} is denylisted")]
    DenylistedAddressError(Address),
}

impl PoolTransactionError for ArcTransactionValidatorError {
    fn is_bad_transaction(&self) -> bool {
        match self {
            Self::BlocklistedError => true,
            Self::InvalidTxError => true,
            Self::DenylistedAddressError(_) => true,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_tx_variant_is_bad_transaction() {
        let err = ArcTransactionValidatorError::InvalidTxError;
        assert!(
            err.is_bad_transaction(),
            "InvalidTxError must be classified as bad transaction"
        );
    }

    #[test]
    fn blocklisted_variant_is_bad_transaction() {
        let err = ArcTransactionValidatorError::BlocklistedError;
        assert!(
            err.is_bad_transaction(),
            "BlocklistedError must be classified as bad transaction"
        );
    }

    #[test]
    fn denylisted_address_variant_is_bad_transaction() {
        let err = ArcTransactionValidatorError::DenylistedAddressError(Address::ZERO);
        assert!(
            err.is_bad_transaction(),
            "DenylistedAddressError must be classified as bad transaction"
        );
    }
}
