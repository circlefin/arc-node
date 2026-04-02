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

use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;

/// Error type for transactions that cannot be processed for an unknown reason (e.g. panic during execution)
#[derive(Debug)]
pub struct UnprocessableTransactionError {
    pub tx_hash: alloy_primitives::TxHash,
}

impl Display for UnprocessableTransactionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Transaction {} is unprocessable", self.tx_hash)
    }
}

impl std::error::Error for UnprocessableTransactionError {}
