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

//! Extracts deployed contract address from a CREATE transaction.

use crate::{action::Action, ArcEnvironment};
use futures_util::future::BoxFuture;
use reth_provider::TransactionsProvider;
use tracing::info;

/// Extracts the deployed contract address from a CREATE transaction
/// and stores it in the environment under `"{tx_name}_address"`.
///
/// Computes the address from the sender and nonce of the transaction.
pub struct StoreDeployedAddress {
    tx_name: String,
}

impl StoreDeployedAddress {
    /// Creates a new action for the named CREATE transaction.
    pub fn new(tx_name: impl Into<String>) -> Self {
        Self {
            tx_name: tx_name.into(),
        }
    }
}

impl Action for StoreDeployedAddress {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let tx_hash = *env.get_tx_hash(&self.tx_name).ok_or_else(|| {
                eyre::eyre!("Transaction '{}' not found in environment", self.tx_name)
            })?;

            let (tx, _meta) = env
                .node()
                .inner
                .provider()
                .transaction_by_hash_with_meta(tx_hash)?
                .ok_or_else(|| {
                    eyre::eyre!("Transaction not found for '{}' ({})", self.tx_name, tx_hash)
                })?;

            use reth_primitives_traits::SignerRecoverable;
            let sender = tx.recover_signer().map_err(|e| {
                eyre::eyre!("Failed to recover signer for '{}': {:?}", self.tx_name, e)
            })?;

            use alloy_consensus::Transaction;
            let nonce = tx.nonce();

            let contract_address = sender.create(nonce);
            let address_name = format!("{}_address", self.tx_name);

            info!(
                tx_name = %self.tx_name,
                sender = %sender,
                nonce,
                contract_address = %contract_address,
                stored_as = %address_name,
                "Stored deployed contract address"
            );

            env.insert_address(address_name, contract_address)?;
            Ok(())
        })
    }
}
