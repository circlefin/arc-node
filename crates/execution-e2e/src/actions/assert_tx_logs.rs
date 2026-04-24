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

//! Receipt log assertion action for EIP-7708 e2e tests.

use crate::{action::Action, ArcEnvironment};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolEvent};
use futures_util::future::BoxFuture;
use reth_provider::ReceiptProvider;
use tracing::info;

/// Expected log entry at a specific index.
enum ExpectedLog {
    /// Exact match on address, topics, and data.
    Exact {
        address: Address,
        topics: Vec<B256>,
        data: Bytes,
    },
    /// ERC-20 Transfer(address,address,uint256) decode helper.
    TransferEvent {
        from: Address,
        to: Address,
        value: U256,
    },
    /// NativeCoinTransferred(address,address,uint256) decode helper (pre-Zero5).
    NativeCoinTransferredEvent {
        from: Address,
        to: Address,
        amount: U256,
    },
    /// Verify only the emitter address at a given index.
    EmitterOnly { address: Address },
}

sol! {
    event Transfer(address indexed from, address indexed to, uint256 value);
    event NativeCoinTransferred(address indexed from, address indexed to, uint256 amount);
}

/// keccak256("Transfer(address,address,uint256)") — derived from sol! macro.
pub const TRANSFER_EVENT_SIGNATURE: B256 = Transfer::SIGNATURE_HASH;

/// keccak256("NativeCoinTransferred(address,address,uint256)") — derived from sol! macro.
pub const NATIVE_COIN_TRANSFERRED_SIGNATURE: B256 = NativeCoinTransferred::SIGNATURE_HASH;

/// Validates a log entry against expected 3-topic event fields (signature, from, to, data).
///
/// Shared by `AssertTxLogs` and `AssertTransferEvent` to avoid duplicated validation logic.
pub(crate) fn validate_event_log(
    tx_name: &str,
    index: usize,
    log: &reth_ethereum::primitives::Log,
    signature: B256,
    from: Address,
    to: Address,
    value: U256,
) -> eyre::Result<()> {
    let topics = log.topics();
    if topics.len() != 3 {
        return Err(eyre::eyre!(
            "Tx '{}' log[{}]: expected 3 topics, got {}",
            tx_name,
            index,
            topics.len()
        ));
    }
    if topics[0] != signature {
        return Err(eyre::eyre!(
            "Tx '{}' log[{}]: topic[0] signature mismatch: expected {}, got {}",
            tx_name,
            index,
            signature,
            topics[0]
        ));
    }
    let expected_from = B256::left_padding_from(from.as_slice());
    if topics[1] != expected_from {
        return Err(eyre::eyre!(
            "Tx '{}' log[{}]: topic[1] (from) mismatch: expected {}, got {}",
            tx_name,
            index,
            expected_from,
            topics[1]
        ));
    }
    let expected_to = B256::left_padding_from(to.as_slice());
    if topics[2] != expected_to {
        return Err(eyre::eyre!(
            "Tx '{}' log[{}]: topic[2] (to) mismatch: expected {}, got {}",
            tx_name,
            index,
            expected_to,
            topics[2]
        ));
    }
    let expected_data = value.to_be_bytes::<32>();
    if log.data.data.as_ref() != expected_data.as_slice() {
        return Err(eyre::eyre!(
            "Tx '{}' log[{}]: data mismatch: expected {}, got {}",
            tx_name,
            index,
            value,
            log.data.data
        ));
    }
    Ok(())
}

/// Asserts on receipt logs for a named transaction.
///
/// Retrieves receipt via the provider, then validates log count
/// and individual log entries against expectations.
pub struct AssertTxLogs {
    tx_name: String,
    expected_log_count: Option<usize>,
    expected_logs: Vec<(usize, ExpectedLog)>,
}

impl AssertTxLogs {
    /// Creates a new log assertion for the named transaction.
    pub fn new(tx_name: impl Into<String>) -> Self {
        Self {
            tx_name: tx_name.into(),
            expected_log_count: None,
            expected_logs: Vec::new(),
        }
    }

    /// Assert exact total number of logs.
    pub fn expect_log_count(mut self, count: usize) -> Self {
        self.expected_log_count = Some(count);
        self
    }

    /// Shorthand for `expect_log_count(0)`.
    pub fn expect_no_logs(self) -> Self {
        self.expect_log_count(0)
    }

    /// Exact match on a single log entry.
    pub fn expect_log_at(
        mut self,
        index: usize,
        address: Address,
        topics: Vec<B256>,
        data: Bytes,
    ) -> Self {
        self.expected_logs.push((
            index,
            ExpectedLog::Exact {
                address,
                topics,
                data,
            },
        ));
        self
    }

    /// Decode helper for ERC-20 Transfer(address,address,uint256).
    pub fn expect_transfer_event(
        mut self,
        index: usize,
        from: Address,
        to: Address,
        value: U256,
    ) -> Self {
        self.expected_logs
            .push((index, ExpectedLog::TransferEvent { from, to, value }));
        self
    }

    /// Decode helper for pre-Zero5 NativeCoinTransferred(address,address,uint256).
    pub fn expect_native_coin_transferred_event(
        mut self,
        index: usize,
        from: Address,
        to: Address,
        amount: U256,
    ) -> Self {
        self.expected_logs.push((
            index,
            ExpectedLog::NativeCoinTransferredEvent { from, to, amount },
        ));
        self
    }

    /// Verify only the emitter address at a given index.
    pub fn expect_emitter_at(mut self, index: usize, address: Address) -> Self {
        self.expected_logs
            .push((index, ExpectedLog::EmitterOnly { address }));
        self
    }
}

impl Action for AssertTxLogs {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let tx_hash = *env.get_tx_hash(&self.tx_name).ok_or_else(|| {
                eyre::eyre!("Transaction '{}' not found in environment", self.tx_name)
            })?;

            info!(
                name = %self.tx_name,
                tx_hash = %tx_hash,
                "Asserting transaction receipt logs"
            );

            let receipt = env
                .node()
                .inner
                .provider()
                .receipt_by_hash(tx_hash)?
                .ok_or_else(|| {
                    eyre::eyre!("Receipt not found for tx '{}' ({})", self.tx_name, tx_hash)
                })?;

            let logs = &receipt.logs;

            // Assert log count
            if let Some(expected_count) = self.expected_log_count {
                if logs.len() != expected_count {
                    return Err(eyre::eyre!(
                        "Tx '{}': expected {} logs, got {}. Logs: {:?}",
                        self.tx_name,
                        expected_count,
                        logs.len(),
                        logs
                    ));
                }
            }

            // Assert individual logs
            for (index, expected) in &self.expected_logs {
                let log = logs.get(*index).ok_or_else(|| {
                    eyre::eyre!(
                        "Tx '{}': no log at index {} (total: {})",
                        self.tx_name,
                        index,
                        logs.len()
                    )
                })?;

                match expected {
                    ExpectedLog::Exact {
                        address,
                        topics,
                        data,
                    } => {
                        if log.address != *address {
                            return Err(eyre::eyre!(
                                "Tx '{}' log[{}]: emitter mismatch: expected {}, got {}",
                                self.tx_name,
                                index,
                                address,
                                log.address
                            ));
                        }
                        let log_topics: Vec<B256> = log.topics().to_vec();
                        if log_topics != *topics {
                            return Err(eyre::eyre!(
                                "Tx '{}' log[{}]: topics mismatch: expected {:?}, got {:?}",
                                self.tx_name,
                                index,
                                topics,
                                log_topics
                            ));
                        }
                        if log.data.data != *data {
                            return Err(eyre::eyre!(
                                "Tx '{}' log[{}]: data mismatch: expected {}, got {}",
                                self.tx_name,
                                index,
                                data,
                                log.data.data
                            ));
                        }
                    }
                    ExpectedLog::TransferEvent { from, to, value } => {
                        validate_event_log(
                            &self.tx_name,
                            *index,
                            log,
                            TRANSFER_EVENT_SIGNATURE,
                            *from,
                            *to,
                            *value,
                        )?;
                    }
                    ExpectedLog::NativeCoinTransferredEvent { from, to, amount } => {
                        validate_event_log(
                            &self.tx_name,
                            *index,
                            log,
                            NATIVE_COIN_TRANSFERRED_SIGNATURE,
                            *from,
                            *to,
                            *amount,
                        )?;
                    }
                    ExpectedLog::EmitterOnly { address } => {
                        if log.address != *address {
                            return Err(eyre::eyre!(
                                "Tx '{}' log[{}]: emitter mismatch: expected {}, got {}",
                                self.tx_name,
                                index,
                                address,
                                log.address
                            ));
                        }
                    }
                }
            }

            info!(
                name = %self.tx_name,
                log_count = logs.len(),
                "Receipt log assertions passed"
            );
            Ok(())
        })
    }
}
