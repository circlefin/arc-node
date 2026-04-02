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

//! Custom transaction pool components for ARC with blocklist support
//!
//! This module provides transaction pool components including a custom validator
//! that wraps the standard Ethereum validator and adds blocklist checking functionality.

use reth_transaction_pool::{
    CoinbaseTipOrdering, EthPooledTransaction, TransactionValidationTaskExecutor,
};

mod error;
mod pool;
mod validator;

pub use error::ArcTransactionValidatorError;
pub use pool::ArcPoolBuilder;
pub use validator::{
    ArcTransactionValidator, InvalidTxList, InvalidTxListConfig, ARC_INVALID_TX_LIST_DEFAULT_CAP,
};

/// Type alias for Arc transaction pool with custom validator
pub type ArcTransactionPool<Provider, BlobStore, Evm> = reth_transaction_pool::Pool<
    TransactionValidationTaskExecutor<ArcTransactionValidator<Provider, EthPooledTransaction, Evm>>,
    CoinbaseTipOrdering<EthPooledTransaction>,
    BlobStore,
>;
