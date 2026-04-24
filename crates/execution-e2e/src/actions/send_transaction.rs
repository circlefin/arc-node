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

//! Transaction sending actions for Arc e2e tests.
//!
//! Provides actions to send EIP-1559 transactions to the node's transaction pool
//! via direct pool injection.

use crate::{action::Action, ArcEnvironment};
use alloy_network::eip2718::{Decodable2718, Encodable2718};
use alloy_primitives::{Address, Bytes, TxHash, TxKind, U256};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use futures_util::future::BoxFuture;
use reth_e2e_test_utils::transaction::TransactionTestContext;
use reth_ethereum_primitives::TransactionSigned;
use reth_primitives_traits::SignerRecoverable;
use reth_transaction_pool::{TransactionOrigin, TransactionPool};
use tracing::{debug, info};

/// Closure that resolves calldata from the environment at execution time.
type DataResolver = Box<dyn Fn(&ArcEnvironment) -> eyre::Result<Bytes> + Send + Sync>;

/// Sends an EIP-1559 transaction to the node's transaction pool.
///
/// This action:
/// 1. Creates an EIP-1559 transaction from a wallet
/// 2. Signs and submits it directly to the transaction pool
/// 3. Stores the transaction hash under the given name for later assertions
///
/// Supports both CALL (with `to` address) and CREATE (without `to`, using `with_create()`)
/// transactions. For CREATE transactions, the deployed contract address is extracted
/// from the receipt and stored under `"{name}_address"` in the environment.
pub struct SendTransaction {
    /// Name to reference this transaction in assertions.
    name: String,
    /// The value to transfer (in wei).
    value: U256,
    /// The recipient address. If None and not create, sends to a random address.
    to: Option<Address>,
    /// Named address to look up at execution time (overrides `to` if set).
    to_named: Option<String>,
    /// If true, this is a CREATE transaction (no `to` address).
    create: bool,
    /// Optional input data for the transaction.
    data: Option<Bytes>,
    /// Deferred calldata resolver — called at execution time with access to the environment.
    data_resolver: Option<DataResolver>,
    /// Gas limit for the transaction.
    gas_limit: u64,
    /// Wallet index to sign from (default: 0 = first wallet).
    wallet_index: usize,
}

impl std::fmt::Debug for SendTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendTransaction")
            .field("name", &self.name)
            .field("value", &self.value)
            .field("to", &self.to)
            .field("to_named", &self.to_named)
            .field("create", &self.create)
            .field("data", &self.data)
            .field("data_resolver", &self.data_resolver.as_ref().map(|_| ".."))
            .field("gas_limit", &self.gas_limit)
            .field("wallet_index", &self.wallet_index)
            .finish()
    }
}

impl SendTransaction {
    /// Creates a new named SendTransaction action with default values.
    ///
    /// The name is used to reference this transaction in assertions via
    /// `AssertTxIncluded::new("name")`.
    ///
    /// Default is a simple 1 wei transfer to a random address.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: U256::from(1),
            to: None,
            to_named: None,
            create: false,
            data: None,
            data_resolver: None,
            gas_limit: 26000,
            wallet_index: 0,
        }
    }

    /// Sets the value to transfer.
    pub fn with_value(mut self, value: U256) -> Self {
        self.value = value;
        self
    }

    /// Sets the recipient address.
    pub fn with_to(mut self, to: Address) -> Self {
        self.to = Some(to);
        self
    }

    /// Sets input data for the transaction (e.g., contract call).
    pub fn with_data(mut self, data: Bytes) -> Self {
        self.data = Some(data);
        self
    }

    /// Marks this as a CREATE transaction (no `to` address).
    /// The `data` field should contain the deployment bytecode.
    pub fn with_create(mut self) -> Self {
        self.create = true;
        self
    }

    /// Sets the recipient to a named address stored in the environment.
    ///
    /// At execution time, looks up the address by name (e.g. `"deploy_address"`
    /// for a contract deployed by a CREATE tx named `"deploy"`).
    pub fn with_to_named(mut self, address_name: impl Into<String>) -> Self {
        self.to_named = Some(address_name.into());
        self
    }

    /// Deferred calldata — the closure is called at execution time with access
    /// to the environment, so it can resolve named addresses or other runtime state.
    pub fn with_data_fn(
        mut self,
        f: impl Fn(&ArcEnvironment) -> eyre::Result<Bytes> + Send + Sync + 'static,
    ) -> Self {
        self.data_resolver = Some(Box::new(f));
        self
    }

    /// Sets the gas limit for the transaction.
    pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.gas_limit = gas_limit;
        self
    }

    /// Signs from the wallet at the given index (default: 0).
    ///
    /// Useful for tests that need to send from different roles,
    /// e.g. index 7 is the operator in localdev genesis.
    pub fn with_wallet_index(mut self, index: usize) -> Self {
        self.wallet_index = index;
        self
    }

    /// Executes the action and returns the transaction hash.
    pub async fn execute_and_return(
        &self,
        env: &mut ArcEnvironment,
    ) -> eyre::Result<(TxHash, reth_primitives_traits::Recovered<TransactionSigned>)> {
        let (signer, chain_id) = {
            let wallet = env.wallet_mut()?;
            let wallets = wallet.wallet_gen();
            let signer = wallets
                .get(self.wallet_index)
                .ok_or_else(|| {
                    eyre::eyre!(
                        "Wallet index {} not available (only {} wallets)",
                        self.wallet_index,
                        wallets.len()
                    )
                })?
                .clone();
            (signer, wallet.chain_id)
        };

        let nonce = env.next_nonce_for_wallet(self.wallet_index)?;

        let tx_kind = if self.create {
            info!(
                name = %self.name,
                nonce,
                value = %self.value,
                "Sending CREATE transaction"
            );
            TxKind::Create
        } else {
            // Resolve recipient: named address > explicit address > random
            let to_address = if let Some(ref addr_name) = self.to_named {
                *env.get_address(addr_name).ok_or_else(|| {
                    eyre::eyre!(
                        "Named address '{}' not found in environment for tx '{}'",
                        addr_name,
                        self.name
                    )
                })?
            } else {
                self.to.unwrap_or_else(Address::random)
            };
            info!(
                name = %self.name,
                nonce,
                value = %self.value,
                to = %to_address,
                "Sending transaction"
            );
            TxKind::Call(to_address)
        };

        // Resolve calldata: deferred resolver > explicit data
        let resolved_data = if let Some(ref resolver) = self.data_resolver {
            Some(resolver(env)?)
        } else {
            self.data.clone()
        };

        // Build EIP-1559 transaction request
        let tx = TransactionRequest {
            nonce: Some(nonce),
            value: Some(self.value),
            to: Some(tx_kind),
            gas: Some(self.gas_limit),
            max_fee_per_gas: Some(1000e9 as u128),
            max_priority_fee_per_gas: Some(1e9 as u128),
            chain_id: Some(chain_id),
            input: TransactionInput {
                input: None,
                data: resolved_data,
            },
            ..Default::default()
        };

        // Sign transaction using reth's TransactionTestContext
        let signed_tx = TransactionTestContext::sign_tx(signer, tx).await;
        let tx_hash = *signed_tx.tx_hash();

        debug!(tx_hash = %tx_hash, "Transaction signed");

        // Convert TxEnvelope to TransactionSigned for pool submission
        let raw_tx: Bytes = signed_tx.encoded_2718().into();
        let tx_signed = TransactionSigned::decode_2718(&mut raw_tx.as_ref())
            .map_err(|e| eyre::eyre!("Failed to decode transaction: {:?}", e))?;

        // Recover the signer
        let recovered_tx = tx_signed
            .try_into_recovered()
            .map_err(|e| eyre::eyre!("Failed to recover transaction signer: {:?}", e))?;

        // Get pool from node and add transaction
        env.node()
            .inner
            .pool
            .add_consensus_transaction(recovered_tx.clone(), TransactionOrigin::Local)
            .await
            .map_err(|e| eyre::eyre!("Failed to submit transaction to pool: {:?}", e))?;

        info!(name = %self.name, tx_hash = %tx_hash, "Transaction submitted to pool");

        Ok((tx_hash, recovered_tx))
    }
}

impl Action for SendTransaction {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let (tx_hash, _) = self.execute_and_return(env).await?;
            env.insert_tx_hash(self.name.clone(), tx_hash)?;
            Ok(())
        })
    }
}
