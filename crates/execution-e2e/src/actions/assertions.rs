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

//! Assertion actions for Arc e2e tests.

use crate::{action::Action, ArcEnvironment};
use alloy_consensus::TxReceipt;
use alloy_eips::Encodable2718;
use alloy_primitives::{Address, U256};
use alloy_rpc_types_eth::BlockNumberOrTag;
use arc_execution_config::hardforks::ArcHardfork;
use futures_util::future::BoxFuture;
use reth_chainspec::{ChainSpecProvider, EthereumHardfork, EthereumHardforks, Hardforks};
use reth_node_api::Block;
use reth_provider::{BlockReaderIdExt, ReceiptProvider};
use reth_rpc_api::EthApiClient;
use tracing::info;

/// Asserts that the current block number matches the expected value.
#[derive(Debug)]
pub struct AssertBlockNumber {
    /// Expected block number.
    expected: u64,
}

impl AssertBlockNumber {
    /// Creates a new AssertBlockNumber action.
    pub fn new(expected: u64) -> Self {
        Self { expected }
    }
}

impl Action for AssertBlockNumber {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let current = env.block_number();
            info!(
                current_block = current,
                expected_block = self.expected,
                "Asserting block number"
            );

            if current != self.expected {
                return Err(eyre::eyre!(
                    "Block number mismatch: expected {}, got {}",
                    self.expected,
                    current
                ));
            }

            info!(block_number = current, "Block number assertion passed");
            Ok(())
        })
    }
}

/// Asserts that a specific hardfork is active (or not active) at the current block.
#[derive(Debug)]
pub struct AssertHardfork {
    /// The hardfork to check.
    hardfork: ArcHardfork,
    /// Whether the hardfork should be active.
    expected_active: bool,
}

impl AssertHardfork {
    /// Creates a new AssertHardfork action.
    ///
    /// # Arguments
    /// * `hardfork` - The hardfork to check
    /// * `expected_active` - Whether the hardfork should be active at current block
    pub fn new(hardfork: ArcHardfork, expected_active: bool) -> Self {
        Self {
            hardfork,
            expected_active,
        }
    }

    /// Asserts that the hardfork IS active at current block.
    pub fn is_active(hardfork: ArcHardfork) -> Self {
        Self::new(hardfork, true)
    }

    /// Asserts that the hardfork is NOT active at current block.
    pub fn is_not_active(hardfork: ArcHardfork) -> Self {
        Self::new(hardfork, false)
    }
}

impl Action for AssertHardfork {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let block_number = env.block_number();
            let chain_spec = env.node().inner.provider().chain_spec();
            let is_active = chain_spec.is_fork_active_at_block(self.hardfork, block_number);

            info!(
                hardfork = ?self.hardfork,
                block_number,
                is_active,
                expected_active = self.expected_active,
                "Asserting hardfork status"
            );

            if is_active != self.expected_active {
                return Err(eyre::eyre!(
                    "Hardfork {:?} at block {}: expected active={}, got active={}",
                    self.hardfork,
                    block_number,
                    self.expected_active,
                    is_active
                ));
            }

            info!(
                hardfork = ?self.hardfork,
                block_number,
                "Hardfork assertion passed"
            );
            Ok(())
        })
    }
}

/// Asserts that a timestamp-based Ethereum hardfork is active (or not active) at the current block.
///
/// Unlike `AssertHardfork` which checks block-based Arc hardforks, this checks
/// timestamp-based Ethereum hardforks like Osaka.
#[derive(Debug)]
pub struct AssertEthereumHardfork {
    /// The Ethereum hardfork to check.
    hardfork: EthereumHardfork,
    /// Whether the hardfork should be active.
    expected_active: bool,
}

impl AssertEthereumHardfork {
    /// Asserts that the Ethereum hardfork IS active at the current block's timestamp.
    pub fn is_active(hardfork: EthereumHardfork) -> Self {
        Self {
            hardfork,
            expected_active: true,
        }
    }

    /// Asserts that the Ethereum hardfork is NOT active at the current block's timestamp.
    pub fn is_not_active(hardfork: EthereumHardfork) -> Self {
        Self {
            hardfork,
            expected_active: false,
        }
    }
}

impl Action for AssertEthereumHardfork {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let timestamp = env.current_block().timestamp;
            let chain_spec = env.node().inner.provider().chain_spec();
            let is_active =
                chain_spec.is_ethereum_fork_active_at_timestamp(self.hardfork, timestamp);

            info!(
                hardfork = ?self.hardfork,
                timestamp,
                is_active,
                expected_active = self.expected_active,
                "Asserting Ethereum hardfork status"
            );

            if is_active != self.expected_active {
                return Err(eyre::eyre!(
                    "Ethereum hardfork {:?} at timestamp {}: expected active={}, got active={}",
                    self.hardfork,
                    timestamp,
                    self.expected_active,
                    is_active
                ));
            }

            info!(
                hardfork = ?self.hardfork,
                timestamp,
                "Ethereum hardfork assertion passed"
            );
            Ok(())
        })
    }
}

/// Expected transaction execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TxStatus {
    /// Transaction should succeed (status = 1).
    #[default]
    Success,
    /// Transaction should revert (status = 0).
    Reverted,
}

/// Asserts that a transaction was included in a block and checks its execution status.
///
/// The transaction name must match one previously sent via `SendTransaction::new("name")`.
/// Optionally specify a block number with `in_block()` to check a specific block instead
/// of the current block.
#[derive(Debug)]
pub struct AssertTxIncluded {
    /// Name of the transaction to look up in `env.tx_hashes`.
    name: String,
    /// Expected execution status (success or reverted).
    expected_status: TxStatus,
    /// Specific block number to check. If None, uses current block.
    block_number: Option<u64>,
}

impl AssertTxIncluded {
    /// Creates a new assertion for a transaction.
    ///
    /// The name must match a transaction previously sent via `SendTransaction::new("name")`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            expected_status: TxStatus::default(),
            block_number: None,
        }
    }

    /// Sets the expected transaction execution status.
    pub fn expect(mut self, expected_status: TxStatus) -> Self {
        self.expected_status = expected_status;
        self
    }

    /// Sets a specific block number to check instead of using the current block.
    pub fn in_block(mut self, block_number: u64) -> Self {
        self.block_number = Some(block_number);
        self
    }
}

impl Action for AssertTxIncluded {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            // Look up transaction hash by name
            let tx_hash = *env.get_tx_hash(&self.name).ok_or_else(|| {
                eyre::eyre!("Transaction '{}' not found in environment", self.name)
            })?;

            // Use specified block number or current block
            let block_number = self.block_number.unwrap_or_else(|| env.block_number());

            info!(
                name = %self.name,
                tx_hash = %tx_hash,
                block_number,
                expected_status = ?self.expected_status,
                "Asserting transaction inclusion and status"
            );

            // Get the block from the provider
            let node = env.node();
            let block = node
                .inner
                .provider()
                .block_by_number_or_tag(BlockNumberOrTag::Number(block_number))?
                .ok_or_else(|| eyre::eyre!("Block {} not found", block_number))?;

            // Check if the transaction is in the block
            let tx_hashes: Vec<_> = block
                .body()
                .transactions()
                .map(|tx| tx.trie_hash())
                .collect();

            if !tx_hashes.contains(&tx_hash) {
                return Err(eyre::eyre!(
                    "Transaction '{}' ({}) not found in block {}. Block contains {} transactions: {:?}",
                    self.name,
                    tx_hash,
                    block_number,
                    tx_hashes.len(),
                    tx_hashes
                ));
            }

            // Get the receipt to check execution status
            let receipt = node
                .inner
                .provider()
                .receipt_by_hash(tx_hash)?
                .ok_or_else(|| {
                    eyre::eyre!(
                        "Receipt not found for transaction '{}' ({})",
                        self.name,
                        tx_hash
                    )
                })?;

            let actual_status = if receipt.status() {
                TxStatus::Success
            } else {
                TxStatus::Reverted
            };

            if actual_status != self.expected_status {
                return Err(eyre::eyre!(
                    "Transaction '{}' ({}) status mismatch: expected {:?}, got {:?}",
                    self.name,
                    tx_hash,
                    self.expected_status,
                    actual_status
                ));
            }

            info!(
                name = %self.name,
                tx_hash = %tx_hash,
                block_number,
                block_hash = %block.header().hash_slow(),
                status = ?actual_status,
                "Transaction inclusion and status assertion passed"
            );

            Ok(())
        })
    }
}

/// Asserts that a transaction was NOT included in a block.
///
/// Useful for verifying that a transaction was rejected during block building
/// (e.g., due to insufficient gas for Arc-specific intrinsic gas costs).
///
/// The transaction name must match one previously sent via `SendTransaction::new("name")`.
/// Optionally specify a block number with `in_block()` to check a specific block instead
/// of the current block.
#[derive(Debug)]
pub struct AssertTxNotIncluded {
    /// Name of the transaction to look up in `env.tx_hashes`.
    name: String,
    /// Specific block number to check. If None, uses current block.
    block_number: Option<u64>,
}

impl AssertTxNotIncluded {
    /// Creates a new assertion that a transaction should NOT be in a block.
    ///
    /// The name must match a transaction previously sent via `SendTransaction::new("name")`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            block_number: None,
        }
    }

    /// Sets a specific block number to check instead of using the current block.
    pub fn in_block(mut self, block_number: u64) -> Self {
        self.block_number = Some(block_number);
        self
    }
}

impl Action for AssertTxNotIncluded {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let tx_hash = *env.get_tx_hash(&self.name).ok_or_else(|| {
                eyre::eyre!("Transaction '{}' not found in environment", self.name)
            })?;

            let block_number = self.block_number.unwrap_or_else(|| env.block_number());

            info!(
                name = %self.name,
                tx_hash = %tx_hash,
                block_number,
                "Asserting transaction NOT included in block"
            );

            let node = env.node();
            let block = node
                .inner
                .provider()
                .block_by_number_or_tag(BlockNumberOrTag::Number(block_number))?
                .ok_or_else(|| eyre::eyre!("Block {} not found", block_number))?;

            let tx_hashes: Vec<_> = block
                .body()
                .transactions()
                .map(|tx| tx.trie_hash())
                .collect();

            if tx_hashes.contains(&tx_hash) {
                return Err(eyre::eyre!(
                    "Transaction '{}' ({}) was found in block {} but should NOT have been included",
                    self.name,
                    tx_hash,
                    block_number,
                ));
            }

            info!(
                name = %self.name,
                tx_hash = %tx_hash,
                block_number,
                "Transaction exclusion assertion passed"
            );

            Ok(())
        })
    }
}

/// Asserts that an address has a specific balance via `eth_getBalance` RPC.
///
/// Supports exact match, minimum bound, and range (minimum + maximum) checks.
#[derive(Debug)]
pub struct AssertBalance {
    address: Address,
    expected: U256,
    /// If set, asserts `balance >= expected` instead of exact equality.
    at_least: bool,
    /// Upper bound for range checks: `balance <= max`.
    max: Option<U256>,
}

impl AssertBalance {
    /// Creates a new exact balance assertion.
    pub fn new(address: Address, expected: U256) -> Self {
        Self {
            address,
            expected,
            at_least: false,
            max: None,
        }
    }

    /// Asserts that the balance is at least `expected`.
    pub fn at_least(mut self) -> Self {
        self.at_least = true;
        self
    }

    /// Sets an upper bound and enables range mode: asserts `expected <= balance <= max`.
    pub fn at_most(mut self, max: U256) -> Self {
        self.at_least = true;
        self.max = Some(max);
        self
    }
}

impl Action for AssertBalance {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let client = env
                .node()
                .rpc_client()
                .ok_or_else(|| eyre::eyre!("RPC client not available"))?;

            let balance = <jsonrpsee::http_client::HttpClient as EthApiClient<
                alloy_rpc_types_eth::TransactionRequest,
                alloy_rpc_types_eth::Transaction,
                alloy_rpc_types_eth::Block,
                alloy_rpc_types_eth::TransactionReceipt,
                alloy_rpc_types_eth::Header,
                alloy_primitives::Bytes,
            >>::balance(&client, self.address, None)
            .await?;

            info!(
                address = %self.address,
                balance = %balance,
                expected = %self.expected,
                at_least = self.at_least,
                "Asserting balance"
            );

            if self.at_least {
                if balance < self.expected {
                    return Err(eyre::eyre!(
                        "Balance of {} too low: expected >= {}, got {}",
                        self.address,
                        self.expected,
                        balance
                    ));
                }
            } else if balance != self.expected {
                return Err(eyre::eyre!(
                    "Balance mismatch for {}: expected {}, got {}",
                    self.address,
                    self.expected,
                    balance
                ));
            }

            if let Some(max) = self.max {
                if balance > max {
                    return Err(eyre::eyre!(
                        "Balance of {} too high: expected <= {}, got {}",
                        self.address,
                        max,
                        balance
                    ));
                }
            }

            info!(address = %self.address, balance = %balance, "Balance assertion passed");
            Ok(())
        })
    }
}
