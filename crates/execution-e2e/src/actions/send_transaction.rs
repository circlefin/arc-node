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

/// Sends an EIP-1559 transfer transaction to the node's transaction pool.
///
/// This action:
/// 1. Creates an EIP-1559 transaction from a wallet
/// 2. Signs and submits it directly to the transaction pool
/// 3. Stores the transaction hash under the given name for later assertions
#[derive(Debug)]
pub struct SendTransaction {
    /// Name to reference this transaction in assertions.
    name: String,
    /// The value to transfer (in wei).
    value: U256,
    /// The recipient address. If None, sends to a random address.
    to: Option<Address>,
    /// Optional input data for the transaction.
    data: Option<Bytes>,
    /// Gas limit for the transaction.
    gas_limit: u64,
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
            data: None,
            gas_limit: 26000,
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

    /// Sets the gas limit for the transaction.
    pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.gas_limit = gas_limit;
        self
    }

    /// Signs, submits to pool, and returns the transaction hash and recovered transaction.
    pub async fn execute_and_return(
        &self,
        env: &mut ArcEnvironment,
    ) -> eyre::Result<(TxHash, reth_primitives_traits::Recovered<TransactionSigned>)> {
        let wallet = env.wallet_mut()?;
        let signer = wallet
            .wallet_gen()
            .first()
            .ok_or_else(|| eyre::eyre!("No wallets generated"))?
            .clone();

        let chain_id = wallet.chain_id;
        let nonce = wallet.inner_nonce;

        wallet.inner_nonce += 1;

        let to_address = self.to.unwrap_or_else(Address::random);

        info!(
            name = %self.name,
            nonce,
            value = %self.value,
            to = %to_address,
            "Sending transaction"
        );

        // Build EIP-1559 transaction request
        let tx = TransactionRequest {
            nonce: Some(nonce),
            value: Some(self.value),
            to: Some(TxKind::Call(to_address)),
            gas: Some(self.gas_limit),
            max_fee_per_gas: Some(1000e9 as u128),
            max_priority_fee_per_gas: Some(1e9 as u128),
            chain_id: Some(chain_id),
            input: TransactionInput {
                input: None,
                data: self.data.clone(),
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
