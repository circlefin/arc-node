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

//! Periodic transaction rebroadcast.
//!
//! Reth announces each transaction to peers exactly once when it enters the pool.
//! If that single gossip attempt is missed (peer busy, queue full, multi-hop drop),
//! the transaction sits in the local pool forever — valid but invisible to validators.
//!
//! This module adds a background task that periodically broadcasts pending
//! transactions to all peers using `PropagationMode::Forced`, which ignores
//! per-peer LRU seen-caches. This ensures transactions reach validators even
//! when the initial announcement was received but the cache entry hasn't been
//! evicted — critical on permissioned chains with low transaction volume.

use reth_network::primitives::NetworkPrimitives;
use reth_network::transactions::TransactionsHandle;
use reth_transaction_pool::{PoolTransaction, TransactionPool};
use std::time::Duration;
use tracing::debug;

pub const DEFAULT_REBROADCAST_INTERVAL: Duration = Duration::from_secs(60);

/// Maximum number of transactions to broadcast per rebroadcast round.
pub const MAX_REBROADCAST: usize = 4096;

/// Background task that periodically broadcasts pending pool transactions
/// using `PropagationMode::Forced` to bypass per-peer LRU seen-caches.
pub struct TxRebroadcaster<Pool, N: NetworkPrimitives> {
    pool: Pool,
    transactions_handle: TransactionsHandle<N>,
    interval: Duration,
}

impl<Pool, N> TxRebroadcaster<Pool, N>
where
    Pool: TransactionPool + 'static,
    N: NetworkPrimitives<
        BroadcastedTransaction = <Pool::Transaction as PoolTransaction>::Consensus,
    >,
{
    pub fn new(pool: Pool, transactions_handle: TransactionsHandle<N>, interval: Duration) -> Self {
        Self {
            pool,
            transactions_handle,
            interval,
        }
    }

    /// Runs the rebroadcast loop until the task is cancelled.
    pub async fn run(self) {
        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;

        loop {
            interval.tick().await;
            self.rebroadcast_pending();
        }
    }

    fn rebroadcast_pending(&self) {
        let txs = collect_pending_txs(&self.pool);

        if txs.is_empty() {
            return;
        }

        let count = txs.len();

        // broadcast_transactions uses PropagationMode::Forced, which ignores
        // per-peer LRU seen-caches — every connected peer receives the announcement.
        self.transactions_handle.broadcast_transactions(txs);

        debug!(
            target: "arc::txpool::rebroadcast",
            count,
            "Broadcast pending transactions (Forced mode)"
        );
    }
}

/// Collects up to [`MAX_REBROADCAST`] consensus-format transactions from the pending set.
///
/// When the pool exceeds `MAX_REBROADCAST`, only the first `MAX_REBROADCAST` transactions
/// by internal pool ordering (sender-id, then nonce) are broadcast.
pub(crate) fn collect_pending_txs<Pool: TransactionPool>(
    pool: &Pool,
) -> Vec<<Pool::Transaction as PoolTransaction>::Consensus> {
    pool.pending_transactions_max(MAX_REBROADCAST)
        .into_iter()
        .map(|tx| tx.transaction.clone_into_consensus().into_inner())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{B256, U256};
    use reth_ethereum::node::EthEvmConfig;
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, test_utils::MockTransaction,
        validate::EthTransactionValidatorBuilder, CoinbaseTipOrdering, Pool, PoolTransaction,
        TransactionPool,
    };

    fn create_test_pool(
        provider: &MockEthProvider,
    ) -> Pool<
        reth_transaction_pool::validate::EthTransactionValidator<
            MockEthProvider,
            MockTransaction,
            EthEvmConfig,
        >,
        CoinbaseTipOrdering<MockTransaction>,
        InMemoryBlobStore,
    > {
        provider.add_block(B256::ZERO, reth_ethereum_primitives::Block::default());
        let blob_store = InMemoryBlobStore::default();
        let validator =
            EthTransactionValidatorBuilder::new(provider.clone(), EthEvmConfig::mainnet())
                .build(blob_store.clone());
        Pool::new(
            validator,
            CoinbaseTipOrdering::default(),
            blob_store,
            Default::default(),
        )
    }

    fn funded_tx(provider: &MockEthProvider, nonce: u64) -> MockTransaction {
        let tx = MockTransaction::legacy()
            .with_gas_limit(21_000)
            .with_gas_price(1_000_000_000)
            .with_value(U256::from(1000))
            .with_nonce(nonce);
        provider.add_account(tx.sender(), ExtendedAccount::new(nonce, U256::MAX));
        tx
    }

    #[test]
    fn collect_pending_txs_empty_pool() {
        let provider = MockEthProvider::default();
        let pool = create_test_pool(&provider);
        let txs = collect_pending_txs(&pool);
        assert!(txs.is_empty());
    }

    #[tokio::test]
    async fn collect_pending_txs_returns_correct_hashes() {
        let provider = MockEthProvider::default();
        let pool = create_test_pool(&provider);

        let tx = funded_tx(&provider, 0);
        let expected_hash = *tx.hash();
        pool.add_external_transaction(tx).await.unwrap();

        let txs = collect_pending_txs(&pool);
        assert_eq!(txs.len(), 1);
        assert_eq!(*txs[0].tx_hash(), expected_hash);
    }

    #[tokio::test]
    async fn collect_pending_txs_multiple() {
        let provider = MockEthProvider::default();
        let pool = create_test_pool(&provider);

        let tx1 = funded_tx(&provider, 0);
        let tx2 = funded_tx(&provider, 0);
        assert_ne!(
            tx1.sender(),
            tx2.sender(),
            "different senders for independent nonces"
        );

        pool.add_external_transaction(tx1).await.unwrap();
        pool.add_external_transaction(tx2).await.unwrap();

        let txs = collect_pending_txs(&pool);
        assert_eq!(txs.len(), 2);
    }

    #[tokio::test]
    async fn collect_pending_txs_truncates_at_max() {
        let provider = MockEthProvider::default();
        let pool = create_test_pool(&provider);

        for i in 0..(MAX_REBROADCAST + 100) {
            let nonce = (i / 100) as u64;
            let tx = funded_tx(&provider, nonce);
            pool.add_external_transaction(tx).await.unwrap();
        }

        let collected = collect_pending_txs(&pool);
        assert_eq!(collected.len(), MAX_REBROADCAST);
    }
}
