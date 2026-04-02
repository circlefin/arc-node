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
use std::ops::RangeInclusive;

use alloy_rpc_types_engine::ExecutionPayloadV3;
use arc_signer::PublicKey;

pub struct Pretty<'a, T>(pub &'a T);

impl fmt::Display for Pretty<'_, PublicKey> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0.as_bytes()))
    }
}

pub struct PrettyPayload<'a>(pub &'a ExecutionPayloadV3);

impl fmt::Debug for PrettyPayload<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionPayloadV3")
            .field(
                "block_number",
                &self.0.payload_inner.payload_inner.block_number,
            )
            .field("block_hash", &self.0.payload_inner.payload_inner.block_hash)
            .field(
                "parent_hash",
                &self.0.payload_inner.payload_inner.parent_hash,
            )
            .field("timestamp", &self.0.payload_inner.payload_inner.timestamp)
            .field(
                "transactions_len",
                &self.0.payload_inner.payload_inner.transactions.len(),
            )
            .field("state_root", &self.0.payload_inner.payload_inner.state_root)
            .field(
                "fee_recipient",
                &self.0.payload_inner.payload_inner.fee_recipient,
            )
            .field(
                "receipts_root",
                &self.0.payload_inner.payload_inner.receipts_root,
            )
            .field("gas_limit", &self.0.payload_inner.payload_inner.gas_limit)
            .field("gas_used", &self.0.payload_inner.payload_inner.gas_used)
            .field(
                "base_fee_per_gas",
                &self.0.payload_inner.payload_inner.base_fee_per_gas,
            )
            .field("extra_data", &self.0.payload_inner.payload_inner.extra_data)
            .finish()
    }
}

impl<T> fmt::Display for Pretty<'_, RangeInclusive<T>>
where
    T: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..={}", self.0.start(), self.0.end())
    }
}
