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

//! Custom transaction pool implementation for ARC

use crate::validator::InvalidTxList;
use crate::{ArcTransactionPool, ArcTransactionValidator};
use arc_execution_config::addresses_denylist::AddressesDenylistConfig;
use arc_execution_config::chainspec::ArcChainSpec;
use reth_ethereum_primitives::TransactionSigned;
use reth_evm::ConfigureEvm;
use reth_node_api::{FullNodeTypes, NodePrimitives, NodeTypes};
use reth_node_builder::{
    components::{PoolBuilder, TxPoolBuilder},
    BuilderContext,
};
use reth_tracing::tracing::{debug, info};
use reth_transaction_pool::{blobstore::DiskFileBlobStore, TransactionValidationTaskExecutor};

/// A basic Arc transaction pool builder.
///
/// Fork from https://github.com/paradigmxyz/reth/blob/v1.7.0/crates/ethereum/node/src/node.rs#L435-L509
/// with customization to use ArcTransactionValidator.
///
/// This contains various settings that can be configured and take precedence over the node's
/// config.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ArcPoolBuilder {
    // TODO add options for txpool args
    invalid_tx_list: Option<InvalidTxList>,
    /// Config for addresses denylist (from/to). When None, no address denylist check in validator.
    addresses_denylist_config: AddressesDenylistConfig,
}

impl ArcPoolBuilder {
    pub fn new(
        invalid_tx_list: Option<InvalidTxList>,
        addresses_denylist_config: AddressesDenylistConfig,
    ) -> Self {
        Self {
            invalid_tx_list,
            addresses_denylist_config,
        }
    }
}

impl<Node, Evm> PoolBuilder<Node, Evm> for ArcPoolBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypes<
            ChainSpec = ArcChainSpec,
            Primitives: NodePrimitives<SignedTx = TransactionSigned>,
        >,
    >,
    Evm: ConfigureEvm<Primitives = <Node::Types as NodeTypes>::Primitives> + 'static,
{
    type Pool = ArcTransactionPool<Node::Provider, DiskFileBlobStore, Evm>;

    async fn build_pool(
        self,
        ctx: &BuilderContext<Node>,
        evm_config: Evm,
    ) -> eyre::Result<Self::Pool> {
        let pool_config = ctx.pool_config();

        // Blobs are disabled; use 0 cache size
        let blob_store = reth_node_builder::components::create_blob_store_with_cache(ctx, Some(0))?;

        let validator =
            TransactionValidationTaskExecutor::eth_builder(ctx.provider().clone(), evm_config)
                .with_max_tx_input_bytes(ctx.config().txpool.max_tx_input_bytes)
                .with_local_transactions_config(pool_config.local_transactions_config.clone())
                .set_tx_fee_cap(ctx.config().rpc.rpc_tx_fee_cap)
                .no_eip4844()
                .with_max_tx_gas_limit(ctx.config().txpool.max_tx_gas_limit)
                .with_minimum_priority_fee(ctx.config().txpool.minimum_priority_fee)
                .with_additional_tasks(ctx.config().txpool.additional_validation_tasks)
                .build_with_tasks(ctx.task_executor().clone(), blob_store.clone())
                .map(|inner| {
                    ArcTransactionValidator::new(
                        inner,
                        self.invalid_tx_list.clone(),
                        self.addresses_denylist_config.clone(),
                    )
                });

        let transaction_pool = TxPoolBuilder::new(ctx)
            .with_validator(validator)
            .build_and_spawn_maintenance_task(blob_store, pool_config)?;

        info!(target: "reth::cli", "Transaction pool initialized");
        debug!(target: "reth::cli", "Spawned txpool maintenance task");

        Ok(transaction_pool)
    }
}

#[cfg(test)]
mod tests {
    use crate::ArcTransactionValidator;
    use alloy_primitives::{B256, U256};
    use arc_execution_config::addresses_denylist::AddressesDenylistConfig;
    use reth_evm_ethereum::EthEvmConfig;
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, test_utils::MockTransaction,
        validate::EthTransactionValidatorBuilder, CoinbaseTipOrdering, Pool, PoolTransaction,
        TransactionOrigin, TransactionPool, TransactionValidator,
    };

    /// Helper function to create a test transaction
    fn create_test_transaction() -> MockTransaction {
        MockTransaction::legacy()
            .with_gas_limit(26_000)
            .with_gas_price(1_000_000_000) // 1 gwei
            .with_value(U256::from(1000))
    }

    /// Helper function to create an ArcTransactionValidator for testing
    fn create_arc_validator_for_test(
        provider: MockEthProvider,
        enable_eip4844: bool,
    ) -> (
        ArcTransactionValidator<MockEthProvider, MockTransaction, EthEvmConfig>,
        InMemoryBlobStore,
    ) {
        // MockEthProvider needs at least one header for best_block_number() to succeed
        provider.add_block(B256::ZERO, reth_ethereum_primitives::Block::default());
        let blob_store = InMemoryBlobStore::default();

        let mut builder = EthTransactionValidatorBuilder::new(provider, EthEvmConfig::mainnet());
        if !enable_eip4844 {
            builder = builder.no_eip4844();
        }

        let eth_validator = builder.build(blob_store.clone());
        let arc_validator =
            ArcTransactionValidator::new(eth_validator, None, AddressesDenylistConfig::Disabled);

        (arc_validator, blob_store)
    }

    #[tokio::test]
    async fn test_arc_validator_delegates_to_inner() {
        let transaction = create_test_transaction();
        let sender = transaction.sender();

        let provider = MockEthProvider::default();
        provider.add_account(sender, ExtendedAccount::new(0, U256::MAX));

        let (arc_validator, blob_store) = create_arc_validator_for_test(provider, true);

        let outcome = arc_validator
            .validate_one(TransactionOrigin::External, transaction.clone())
            .await;

        assert!(
            outcome.is_valid(),
            "Expected valid transaction, but got {:?}",
            outcome
        );

        // Test full pool integration
        let pool = Pool::new(
            arc_validator,
            CoinbaseTipOrdering::default(),
            blob_store.clone(),
            Default::default(),
        );

        let res = pool.add_external_transaction(transaction.clone()).await;
        assert!(res.is_ok(), "Failed to add transaction to pool: {:?}", res);

        let tx = pool.get(transaction.hash());
        assert!(tx.is_some(), "Transaction should be retrievable from pool");
    }

    #[tokio::test]
    async fn test_arc_validator_trait_implementation() {
        let transaction = create_test_transaction();
        let sender = transaction.sender();

        let provider = MockEthProvider::default();
        provider.add_account(sender, ExtendedAccount::new(0, U256::MAX));

        let (arc_validator, _blob_store) = create_arc_validator_for_test(provider, true);

        let outcome = arc_validator
            .validate_transaction(TransactionOrigin::External, transaction.clone())
            .await;

        assert!(
            outcome.is_valid(),
            "Expected valid transaction, but got {:?}",
            outcome
        );

        let transactions = vec![
            (TransactionOrigin::External, transaction.clone()),
            (TransactionOrigin::Local, transaction.clone()),
        ];

        let outcomes = arc_validator.validate_transactions(transactions).await;
        assert_eq!(outcomes.len(), 2);

        let transactions = vec![transaction.clone(), transaction];
        let outcomes = arc_validator
            .validate_transactions_with_origin(TransactionOrigin::External, transactions)
            .await;
        assert_eq!(outcomes.len(), 2);
    }

    #[tokio::test]
    async fn test_arc_validator_rejects_eip4844_transactions() {
        let provider = MockEthProvider::default();

        // EIP-4844 disabled (matches production config)
        let (arc_validator, _blob_store) = create_arc_validator_for_test(provider, false);

        let blob_transaction = MockTransaction::eip4844();

        let outcome = arc_validator
            .validate_one(TransactionOrigin::External, blob_transaction.clone())
            .await;

        assert!(
            outcome.is_invalid(),
            "EIP-4844 transaction should be invalid when EIP-4844 is disabled, got: {:?}",
            outcome
        );
    }
}
