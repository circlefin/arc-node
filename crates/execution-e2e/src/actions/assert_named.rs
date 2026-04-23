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

//! Environment-aware assertion actions that resolve named addresses at execution time.
//!
//! These complement the static `AssertTxLogs` and `AssertBalance` actions for cases
//! where the target address is only known after a prior action (e.g. `StoreDeployedAddress`)
//! has run.

use crate::{action::Action, ArcEnvironment};
use alloy_primitives::{Address, U256};
use alloy_rpc_types_eth::BlockNumberOrTag;
use futures_util::future::BoxFuture;
use reth_provider::ReceiptProvider;
use reth_rpc_api::EthApiClient;
use tracing::info;

use super::assert_tx_logs::{validate_event_log, TRANSFER_EVENT_SIGNATURE};

/// An address reference that can be either a concrete address or a named address
/// resolved from the environment at execution time.
#[derive(Clone)]
pub enum AddressRef {
    /// A concrete address known at builder time.
    Literal(Address),
    /// A named address resolved from the environment at execution time.
    Named(String),
}

impl AddressRef {
    fn resolve(&self, env: &ArcEnvironment) -> eyre::Result<Address> {
        match self {
            Self::Literal(addr) => Ok(*addr),
            Self::Named(name) => env
                .get_address(name)
                .copied()
                .ok_or_else(|| eyre::eyre!("Named address '{}' not found in environment", name)),
        }
    }
}

impl From<Address> for AddressRef {
    fn from(addr: Address) -> Self {
        Self::Literal(addr)
    }
}

/// Asserts an EIP-7708 Transfer event at a specific log index in a named transaction's receipt.
///
/// Resolves `from` and `to` from the environment at execution time, supporting
/// both literal addresses and named deployed-contract addresses.
pub struct AssertTransferEvent {
    tx_name: String,
    log_index: usize,
    from: AddressRef,
    to: AddressRef,
    value: U256,
}

impl AssertTransferEvent {
    /// Creates a new transfer event assertion.
    ///
    /// `from` and `to` accept either `Address` or `AddressRef::Named("name")`.
    pub fn new(
        tx_name: impl Into<String>,
        log_index: usize,
        from: impl Into<AddressRef>,
        to: impl Into<AddressRef>,
        value: U256,
    ) -> Self {
        Self {
            tx_name: tx_name.into(),
            log_index,
            from: from.into(),
            to: to.into(),
            value,
        }
    }

    /// Helper: named address reference for use with `from` / `to` parameters.
    pub fn named(name: impl Into<String>) -> AddressRef {
        AddressRef::Named(name.into())
    }
}

impl Action for AssertTransferEvent {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let from = self.from.resolve(env)?;
            let to = self.to.resolve(env)?;

            let tx_hash = *env.get_tx_hash(&self.tx_name).ok_or_else(|| {
                eyre::eyre!("Transaction '{}' not found in environment", self.tx_name)
            })?;

            let receipt = env
                .node()
                .inner
                .provider()
                .receipt_by_hash(tx_hash)?
                .ok_or_else(|| {
                    eyre::eyre!("Receipt not found for tx '{}' ({})", self.tx_name, tx_hash)
                })?;

            let log = receipt.logs.get(self.log_index).ok_or_else(|| {
                eyre::eyre!(
                    "Tx '{}': no log at index {} (total: {})",
                    self.tx_name,
                    self.log_index,
                    receipt.logs.len()
                )
            })?;

            validate_event_log(
                &self.tx_name,
                self.log_index,
                log,
                TRANSFER_EVENT_SIGNATURE,
                from,
                to,
                self.value,
            )?;

            info!(
                tx = %self.tx_name,
                index = self.log_index,
                from = %from,
                to = %to,
                value = %self.value,
                "Transfer event assertion passed"
            );
            Ok(())
        })
    }
}

/// Asserts account balance for a named address from the environment.
pub struct AssertNamedBalance {
    address_name: String,
    expected: U256,
}

impl AssertNamedBalance {
    /// Assert the balance of a named address equals the expected value.
    pub fn of(address_name: impl Into<String>) -> AssertNamedBalanceBuilder {
        AssertNamedBalanceBuilder {
            address_name: address_name.into(),
        }
    }
}

/// Builder for `AssertNamedBalance`.
pub struct AssertNamedBalanceBuilder {
    address_name: String,
}

impl AssertNamedBalanceBuilder {
    /// Assert exact balance match.
    pub fn equals(self, expected: U256) -> AssertNamedBalance {
        AssertNamedBalance {
            address_name: self.address_name,
            expected,
        }
    }
}

impl Action for AssertNamedBalance {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let address = *env.get_address(&self.address_name).ok_or_else(|| {
                eyre::eyre!(
                    "Named address '{}' not found in environment",
                    self.address_name
                )
            })?;

            let block_number = env.block_number();

            info!(
                name = %self.address_name,
                address = %address,
                expected = %self.expected,
                block_number,
                "Asserting named account balance"
            );

            let client = env
                .node()
                .rpc_client()
                .ok_or_else(|| eyre::eyre!("RPC client not available"))?;

            let balance: U256 = <jsonrpsee::http_client::HttpClient as EthApiClient<
                alloy_rpc_types_eth::TransactionRequest,
                alloy_rpc_types_eth::Transaction,
                alloy_rpc_types_eth::Block,
                alloy_rpc_types_eth::TransactionReceipt,
                alloy_rpc_types_eth::Header,
                alloy_primitives::Bytes,
            >>::balance(
                &client,
                address,
                Some(BlockNumberOrTag::Number(block_number).into()),
            )
            .await
            .map_err(|e| {
                eyre::eyre!(
                    "eth_getBalance failed for '{}' ({}) at block {}: {}",
                    self.address_name,
                    address,
                    block_number,
                    e
                )
            })?;

            if balance != self.expected {
                return Err(eyre::eyre!(
                    "Balance mismatch for '{}' ({}): expected {}, got {} (block {})",
                    self.address_name,
                    address,
                    self.expected,
                    balance,
                    block_number
                ));
            }

            info!(
                name = %self.address_name,
                address = %address,
                balance = %balance,
                "Named balance assertion passed"
            );
            Ok(())
        })
    }
}
