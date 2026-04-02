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

//! Arc transaction validator implementation with blocklist and addresses denylist support

use crate::error::ArcTransactionValidatorError;
use alloy_primitives::{Address, TxHash};
use arc_execution_config::addresses_denylist::AddressesDenylistConfig;
use arc_execution_validation::{is_denylisted, DenylistError};
use arc_precompiles::{
    native_coin_control::{compute_is_blocklisted_storage_slot, UNBLOCKLISTED_STATUS},
    NATIVE_COIN_CONTROL_ADDRESS,
};
use arc_shared::metrics::denylist::record_denylist_rejection;
use parking_lot::RwLock;
use reth_chainspec::{ChainSpecProvider, EthereumHardforks};
use reth_ethereum::pool::EthTransactionValidator;
use reth_evm::ConfigureEvm;
use reth_primitives_traits::{BlockTy, SealedBlock};
use reth_provider::StateProviderFactory;
use reth_storage_api::errors::provider::ProviderResult;
use reth_storage_api::AccountInfoReader;
use reth_storage_api::BlockNumReader;
use reth_storage_api::StateProvider;
use reth_transaction_pool::{
    error::InvalidPoolTransactionError, EthPoolTransaction, TransactionOrigin,
    TransactionValidationOutcome, TransactionValidator,
};
use schnellru::{ByLength, LruMap};
use std::{marker::PhantomData, sync::Arc};
use tracing::{info, warn};

/// Default capacity for the invalid transaction list when no override is provided.
pub const ARC_INVALID_TX_LIST_DEFAULT_CAP: u32 = 100_000; // 32 bytes * 100_000 = ~3.2 MB (+ LRU overhead)

/// Configuration for the invalid transaction list.
#[derive(Debug, Clone)]
pub struct InvalidTxListConfig {
    /// Whether the invalid tx list is enabled.
    pub enabled: bool,
    /// Maximum capacity of the invalid tx list LRU cache.
    pub capacity: u32,
}

impl Default for InvalidTxListConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            capacity: ARC_INVALID_TX_LIST_DEFAULT_CAP,
        }
    }
}

impl InvalidTxListConfig {
    /// Creates a new InvalidTxListConfig.
    pub fn new(enabled: bool, capacity: u32) -> Self {
        Self { enabled, capacity }
    }
}

static METRICS_LABELS: &[(&str, &str)] = &[("component", "arc_reth_node")];

pub type InvalidTxList = InvalidTxListInner<DefaultInvalidTxListMetrics>;

pub trait InvalidTxListMetricsSink {
    fn hit();
    fn size(len: usize);
    fn inserts(n: usize);
    fn insert_batches();
}

#[derive(Debug, Clone)]
pub struct DefaultInvalidTxListMetrics;

impl InvalidTxListMetricsSink for DefaultInvalidTxListMetrics {
    fn hit() {
        metrics::counter!("arc_invalid_tx_list_hits_total", METRICS_LABELS).increment(1);
    }
    fn size(len: usize) {
        metrics::gauge!("arc_invalid_tx_list_size", METRICS_LABELS).set(len as f64);
    }
    fn inserts(n: usize) {
        metrics::counter!("arc_invalid_tx_list_inserts_total", METRICS_LABELS).increment(n as u64);
    }
    fn insert_batches() {
        metrics::counter!("arc_invalid_tx_list_batch_inserts_total", METRICS_LABELS).increment(1);
    }
}

#[derive(Debug, Clone)]
pub struct InvalidTxListInner<Sink: InvalidTxListMetricsSink>(
    Arc<RwLock<LruMap<TxHash, ()>>>,
    PhantomData<Sink>,
);

impl<Sink> InvalidTxListInner<Sink>
where
    Sink: InvalidTxListMetricsSink,
{
    pub fn new(cap: u32) -> Self {
        let lru = Arc::new(RwLock::new(LruMap::new(ByLength::new(cap))));
        info!(capacity = cap, "Created InvalidTxList");
        Sink::size(0);
        Self(lru, PhantomData)
    }

    pub fn contains(&self, hash: &TxHash) -> bool {
        let hit = { self.0.read().peek(hash).is_some() };
        if hit {
            Sink::hit();
        }
        hit
    }

    pub fn insert_many(&self, hashes: impl IntoIterator<Item = TxHash>) {
        let mut hashes_count = 0usize;
        let mut success = true;
        let len = {
            let mut map = self.0.write();
            for hash in hashes {
                if !map.insert(hash, ()) {
                    success = false;
                }
                hashes_count += 1;
            }
            map.len()
        };
        if !success {
            info!("The invalid tx list has capacity 0, no TX was inserted")
        }
        Sink::size(len);
        Sink::inserts(hashes_count);
        Sink::insert_batches();
    }

    pub fn len(&self) -> usize {
        self.0.read().len()
    }
}

/// Custom mempool transaction validator that includes blocklist and addresses denylist checks
#[derive(Debug, Clone)]
pub struct ArcTransactionValidator<Client, Tx, Evm> {
    /// The underlying Ethereum validator that handles standard validation logic
    inner: Arc<EthTransactionValidator<Client, Tx, Evm>>,
    invalid_tx_list: Option<InvalidTxList>,
    addresses_denylist_config: AddressesDenylistConfig,
}

impl<Client, Tx, Evm> ArcTransactionValidator<Client, Tx, Evm> {
    /// Create a new [ArcTransactionValidator] wrapping the Ethereum validator
    pub fn new(
        inner: EthTransactionValidator<Client, Tx, Evm>,
        invalid_tx_list: Option<InvalidTxList>,
        addresses_denylist_config: AddressesDenylistConfig,
    ) -> Self {
        Self {
            inner: Arc::new(inner),
            invalid_tx_list,
            addresses_denylist_config,
        }
    }
}

impl<Client, Tx, Evm> ArcTransactionValidator<Client, Tx, Evm>
where
    Client: ChainSpecProvider<ChainSpec: EthereumHardforks> + StateProviderFactory + BlockNumReader,
    Tx: EthPoolTransaction,
    Evm: ConfigureEvm,
{
    /// Validates a single transaction.
    ///
    /// This behaves the same as [`ArcTransactionValidator::validate_one_with_state`], but creates
    /// a new state provider internally.
    pub async fn validate_one(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
    ) -> TransactionValidationOutcome<Tx> {
        self.validate_one_with_state(origin, transaction, &mut None)
            .await
    }

    /// Validates a single transaction with a provided state provider.
    /// This behaves the same as [`EthTransactionValidator::validate_one_with_state`], but in
    /// addition applies blocklist and addresses denylist checks:
    pub async fn validate_one_with_state(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
        state: &mut Option<Box<dyn AccountInfoReader + Send>>,
    ) -> TransactionValidationOutcome<Tx> {
        // ✅ invalid tx list pre-check: refuse tx by hash immediately
        if let Some(invalid_tx_list) = &self.invalid_tx_list {
            if invalid_tx_list.contains(transaction.hash()) {
                warn!(
                    origin = ?origin,
                    hash = %transaction.hash(),
                    sender = %transaction.sender(),
                    reason = "invalid_tx",
                    "transaction rejected"
                );
                return TransactionValidationOutcome::Invalid(
                    transaction,
                    InvalidPoolTransactionError::other(
                        ArcTransactionValidatorError::InvalidTxError,
                    ),
                );
            }
        }

        match self.inner.client().latest() {
            Ok(state_provider) => {
                let sender = transaction.sender();
                if self.is_address_blocklisted(sender, &state_provider) {
                    info!(
                        "Rejecting {:?} transaction {} - blocklisted sender: {}",
                        origin,
                        transaction.hash(),
                        sender
                    );
                    return TransactionValidationOutcome::Invalid(
                        transaction,
                        InvalidPoolTransactionError::other(
                            ArcTransactionValidatorError::BlocklistedError,
                        ),
                    );
                }

                // Check recipient address if this transaction with value > 0
                if let Some(to) = transaction.to() {
                    if !transaction.value().is_zero()
                        && self.is_address_blocklisted(to, &state_provider)
                    {
                        info!(
                            "Rejecting {:?} transaction {} - blocklisted recipient: {} (sender: {})",
                            origin,
                            transaction.hash(),
                            to,
                            sender
                        );
                        return TransactionValidationOutcome::Invalid(
                            transaction,
                            InvalidPoolTransactionError::other(
                                ArcTransactionValidatorError::BlocklistedError,
                            ),
                        );
                    }
                }

                // check if transaction's to/from addresses are denylisted
                match self.check_for_denylisted_addresses(&transaction, &state_provider) {
                    Ok(Some(address)) => {
                        record_denylist_rejection();
                        warn!(
                            origin = ?origin,
                            address = %address,
                            reason = "denylisted",
                            location = "mempool",
                            "transaction rejected due to denylisted address"
                        );
                        return TransactionValidationOutcome::Invalid(
                            transaction,
                            InvalidPoolTransactionError::other(
                                ArcTransactionValidatorError::DenylistedAddressError(address),
                            ),
                        );
                    }
                    Ok(None) => {}
                    Err(err) => {
                        return TransactionValidationOutcome::Error(
                            *transaction.hash(),
                            Box::new(err),
                        );
                    }
                };

                // Store the provider for the inner validator for reuse
                *state = Some(Box::new(state_provider));
            }
            Err(err) => {
                return TransactionValidationOutcome::Error(*transaction.hash(), Box::new(err));
            }
        }

        // If blocklist and addresses denylist validation pass, delegate to the inner validator
        self.inner
            .validate_one_with_state(origin, transaction, state)
    }

    /// If the transaction has a denylisted address, returns Ok(Some(address)); otherwise Ok(None).
    /// If an error occurs, returns Err(boxed TransactionValidationOutcome).
    fn check_for_denylisted_addresses(
        &self,
        transaction: &Tx,
        state_provider: &dyn StateProvider,
    ) -> ProviderResult<Option<Address>> {
        if !self.addresses_denylist_config.is_enabled() {
            return Ok(None);
        }

        let addresses = std::iter::once(transaction.sender()).chain(transaction.to());
        for address in addresses {
            match is_denylisted(state_provider, &self.addresses_denylist_config, address) {
                Ok(true) => return Ok(Some(address)),
                Ok(false) => {} // continue to next address
                Err(DenylistError::StorageReadFailed(e)) => return Err(e),
            }
        }

        Ok(None)
    }

    /// Check if an address is blocklisted by reading from the native coin control precompile storage.
    fn is_address_blocklisted(
        &self,
        address: alloy_primitives::Address,
        state: &dyn StateProvider,
    ) -> bool {
        let storage_slot = compute_is_blocklisted_storage_slot(address);
        state
            .storage(NATIVE_COIN_CONTROL_ADDRESS, storage_slot)
            .ok()
            .flatten()
            .is_some_and(|value| value != UNBLOCKLISTED_STATUS)
    }
}

impl<Client, Tx, Evm> TransactionValidator for ArcTransactionValidator<Client, Tx, Evm>
where
    Client: ChainSpecProvider<ChainSpec: EthereumHardforks> + StateProviderFactory + BlockNumReader,
    Tx: EthPoolTransaction,
    Evm: ConfigureEvm,
{
    type Transaction = Tx;
    type Block = BlockTy<Evm::Primitives>;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        self.validate_one(origin, transaction).await
    }

    fn on_new_head_block(&self, new_tip_block: &SealedBlock<Self::Block>) {
        self.inner.on_new_head_block(new_tip_block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::Transaction;
    use alloy_primitives::{Address, B256, U256};
    use arc_execution_config::addresses_denylist::compute_denylist_storage_slot;
    use arc_execution_config::addresses_denylist::{
        AddressesDenylistConfig, DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
    };
    use reth_evm_ethereum::EthEvmConfig;
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use reth_transaction_pool::blobstore::InMemoryBlobStore;
    use reth_transaction_pool::{
        test_utils::MockTransaction, validate::EthTransactionValidatorBuilder, PoolTransaction,
    };
    use serial_test::serial;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Helper function
    fn create_arc_validator_for_test(
        provider: MockEthProvider,
    ) -> ArcTransactionValidator<MockEthProvider, MockTransaction, EthEvmConfig> {
        // MockEthProvider needs at least one header for best_block_number() to succeed
        provider.add_block(B256::ZERO, reth_ethereum_primitives::Block::default());
        let blob_store = InMemoryBlobStore::default();
        let eth_validator = EthTransactionValidatorBuilder::new(provider, EthEvmConfig::mainnet())
            .no_eip4844()
            .build(blob_store);
        ArcTransactionValidator::new(
            eth_validator,
            Some(InvalidTxList::new(ARC_INVALID_TX_LIST_DEFAULT_CAP)),
            AddressesDenylistConfig::Disabled,
        )
    }

    #[derive(Debug)]
    enum ExpectedOutcome {
        Valid,
        Invalid,
    }

    struct BlocklistTestCase {
        name: &'static str,
        tx: MockTransaction,
        sender_blocklisted: bool,
        to_blocklisted: bool,
        expected_outcome: ExpectedOutcome,
    }

    #[tokio::test]
    async fn test_validate_one_with_state() {
        let test_cases = [
            BlocklistTestCase {
                name: "valid_sender_and_recipient",
                tx: MockTransaction::legacy()
                    .with_gas_limit(26_000)
                    .with_gas_price(1_000_000_000)
                    .with_value(U256::from(1000)),
                sender_blocklisted: false,
                to_blocklisted: false,
                expected_outcome: ExpectedOutcome::Valid,
            },
            BlocklistTestCase {
                name: "blocklisted_sender",
                tx: MockTransaction::legacy()
                    .with_gas_limit(26_000)
                    .with_gas_price(1_000_000_000)
                    .with_value(U256::from(1000)),
                sender_blocklisted: true,
                to_blocklisted: false,
                expected_outcome: ExpectedOutcome::Invalid,
            },
            BlocklistTestCase {
                name: "blocklisted_to_with_value",
                tx: MockTransaction::legacy()
                    .with_gas_limit(26_000)
                    .with_gas_price(1_000_000_000)
                    .with_value(U256::from(1000)),
                sender_blocklisted: false,
                to_blocklisted: true,
                expected_outcome: ExpectedOutcome::Invalid,
            },
            BlocklistTestCase {
                name: "blocklisted_to_with_zero_value",
                tx: MockTransaction::legacy()
                    .with_gas_limit(26_000)
                    .with_gas_price(1_000_000_000)
                    .with_value(U256::ZERO),
                sender_blocklisted: false,
                to_blocklisted: true,
                expected_outcome: ExpectedOutcome::Valid,
            },
        ];

        for test_case in test_cases {
            let transaction = test_case.tx;

            let provider = MockEthProvider::default();
            provider.add_block(B256::ZERO, reth_ethereum_primitives::Block::default());
            provider.add_account(transaction.sender(), ExtendedAccount::new(0, U256::MAX));
            if let Some(to_address) = transaction.to() {
                provider.add_account(to_address, ExtendedAccount::new(0, U256::MAX));
            }

            // Set up blocklist storage for the precompile using ExtendedAccount
            if test_case.sender_blocklisted || test_case.to_blocklisted {
                let mut storage = std::collections::HashMap::new();

                if test_case.sender_blocklisted {
                    let sender_slot = compute_is_blocklisted_storage_slot(transaction.sender());
                    storage.insert(sender_slot, U256::from(1));
                }
                if test_case.to_blocklisted {
                    if let Some(to_address) = transaction.to() {
                        let recipient_slot = compute_is_blocklisted_storage_slot(to_address);
                        storage.insert(recipient_slot, U256::from(1));
                    }
                }

                let precompile_account =
                    ExtendedAccount::new(0, U256::ZERO).extend_storage(storage);
                provider.add_account(NATIVE_COIN_CONTROL_ADDRESS, precompile_account);
            }

            let arc_validator = create_arc_validator_for_test(provider);
            let outcome = arc_validator
                .validate_one_with_state(TransactionOrigin::External, transaction, &mut None)
                .await;

            let matches_expectation = matches!(
                (&outcome, &test_case.expected_outcome),
                (
                    TransactionValidationOutcome::Valid { .. },
                    ExpectedOutcome::Valid
                ) | (
                    TransactionValidationOutcome::Invalid(_, _),
                    ExpectedOutcome::Invalid
                )
            );
            assert!(matches_expectation, "Test '{}' failed", test_case.name);
        }
    }

    #[tokio::test]
    async fn invalid_tx_list_disabled_allows_tx() {
        // Build a validator with invalid_tx_list None and ensure tx is not rejected for invalid_tx reason.
        let tx = MockTransaction::legacy()
            .with_gas_limit(26_000)
            .with_gas_price(1_000_000_000)
            .with_value(U256::from(777));
        let provider = MockEthProvider::default();
        provider.add_block(B256::ZERO, reth_ethereum_primitives::Block::default());
        provider.add_account(tx.sender(), ExtendedAccount::new(0, U256::MAX));
        if let Some(to) = tx.to() {
            provider.add_account(to, ExtendedAccount::new(0, U256::MAX));
        }

        let blob_store = InMemoryBlobStore::default();
        let eth_validator = EthTransactionValidatorBuilder::new(provider, EthEvmConfig::mainnet())
            .no_eip4844()
            .build(blob_store);
        let arc_validator =
            ArcTransactionValidator::new(eth_validator, None, AddressesDenylistConfig::Disabled); // invalid tx list disabled

        let outcome = arc_validator
            .validate_one_with_state(TransactionOrigin::External, tx, &mut None)
            .await;

        // Expect a Valid outcome (no invalid tx list pre-check, and no blocklist entries set up).
        assert!(
            matches!(outcome, TransactionValidationOutcome::Valid { .. }),
            "tx should validate when invalid tx list disabled"
        );
    }

    #[tokio::test]
    async fn invalid_tx_hash_is_rejected() {
        let tx = MockTransaction::legacy()
            .with_gas_limit(26_000)
            .with_gas_price(1_000_000_000)
            .with_value(U256::from(1234));

        let provider = MockEthProvider::default();
        provider.add_account(tx.sender(), ExtendedAccount::new(0, U256::MAX));
        let to = tx
            .to()
            .expect("legacy mock transaction must have a recipient address");
        provider.add_account(to, ExtendedAccount::new(0, U256::MAX));

        let arc_validator = create_arc_validator_for_test(provider);

        // Pre-insert hash into invalid tx list.
        if let Some(invalid_tx_list) = &arc_validator.invalid_tx_list {
            invalid_tx_list.insert_many([*tx.hash()]);
            assert!(
                invalid_tx_list.contains(tx.hash()),
                "hash should be in invalid tx list"
            );
        } else {
            panic!("invalid tx list unexpectedly disabled in test");
        }

        let outcome = arc_validator
            .validate_one_with_state(TransactionOrigin::External, tx, &mut None)
            .await;

        let TransactionValidationOutcome::Invalid(_, err) = &outcome else {
            panic!("expected Invalid outcome with invalid tx error, got {outcome:?}");
        };
        let inner: &ArcTransactionValidatorError = err
            .downcast_other_ref::<ArcTransactionValidatorError>()
            .unwrap();
        assert!(matches!(
            inner,
            ArcTransactionValidatorError::InvalidTxError
        ));
    }

    #[tokio::test]
    async fn addresses_denylist_config_none_accepts_tx() {
        // When addresses_denylist_config is None, no address denylist check; tx is accepted.
        let tx = MockTransaction::legacy()
            .with_gas_limit(26_000)
            .with_gas_price(1_000_000_000)
            .with_value(U256::from(100));
        let provider = MockEthProvider::default();
        provider.add_block(B256::ZERO, reth_ethereum_primitives::Block::default());
        provider.add_account(tx.sender(), ExtendedAccount::new(0, U256::MAX));
        if let Some(to) = tx.to() {
            provider.add_account(to, ExtendedAccount::new(0, U256::MAX));
        }
        let blob_store = InMemoryBlobStore::default();
        let eth_validator = EthTransactionValidatorBuilder::new(provider, EthEvmConfig::mainnet())
            .no_eip4844()
            .build(blob_store);
        let arc_validator =
            ArcTransactionValidator::new(eth_validator, None, AddressesDenylistConfig::Disabled);
        let outcome = arc_validator
            .validate_one_with_state(TransactionOrigin::External, tx, &mut None)
            .await;
        assert!(
            matches!(outcome, TransactionValidationOutcome::Valid { .. }),
            "tx should be accepted when addresses_denylist_config is None"
        );
    }

    #[tokio::test]
    async fn addresses_denylist_enabled_sender_excluded_accepts_tx() {
        // When denylist is enabled but sender is in address exclusions, tx is accepted.
        let tx = MockTransaction::legacy()
            .with_gas_limit(26_000)
            .with_gas_price(1_000_000_000)
            .with_value(U256::from(200));
        let sender = tx.sender();
        let contract = Address::from([0x36u8; 20]);
        let config = AddressesDenylistConfig::try_new(
            true,
            Some(contract),
            Some(DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
            vec![sender],
        )
        .unwrap();
        let provider = MockEthProvider::default();
        provider.add_block(B256::ZERO, reth_ethereum_primitives::Block::default());
        provider.add_account(sender, ExtendedAccount::new(0, U256::MAX));
        if let Some(to) = tx.to() {
            provider.add_account(to, ExtendedAccount::new(0, U256::MAX));
        }
        let blob_store = InMemoryBlobStore::default();
        let eth_validator = EthTransactionValidatorBuilder::new(provider, EthEvmConfig::mainnet())
            .no_eip4844()
            .build(blob_store);
        let arc_validator = ArcTransactionValidator::new(eth_validator, None, config);
        let outcome = arc_validator
            .validate_one_with_state(TransactionOrigin::External, tx, &mut None)
            .await;
        assert!(
            matches!(outcome, TransactionValidationOutcome::Valid { .. }),
            "tx should be accepted when sender is in address exclusions"
        );
    }

    #[tokio::test]
    async fn addresses_denylist_denylisted_address_rejected() {
        // When denylist is enabled and sender or recipient is denylisted in contract storage, tx is rejected.
        let tx = MockTransaction::legacy()
            .with_gas_limit(26_000)
            .with_gas_price(1_000_000_000)
            .with_value(U256::from(300));
        let contract = Address::from([0x36u8; 20]);
        let config = AddressesDenylistConfig::try_new(
            true,
            Some(contract),
            Some(DEFAULT_DENYLIST_ERC7201_BASE_SLOT),
            Vec::new(),
        )
        .unwrap();

        for (name, denylisted_address) in [
            ("denylisted_sender", tx.sender()),
            (
                "denylisted_recipient",
                tx.to().expect("MockTransaction::legacy has a to address"),
            ),
        ] {
            let provider = MockEthProvider::default();
            provider.add_block(B256::ZERO, reth_ethereum_primitives::Block::default());
            provider.add_account(tx.sender(), ExtendedAccount::new(0, U256::MAX));
            if let Some(to) = tx.to() {
                provider.add_account(to, ExtendedAccount::new(0, U256::MAX));
            }
            let slot = compute_denylist_storage_slot(
                denylisted_address,
                DEFAULT_DENYLIST_ERC7201_BASE_SLOT,
            );
            let mut denylist_storage = std::collections::HashMap::new();
            denylist_storage.insert(slot, U256::from(1));
            let denylist_account =
                ExtendedAccount::new(0, U256::ZERO).extend_storage(denylist_storage);
            provider.add_account(contract, denylist_account);
            let blob_store = InMemoryBlobStore::default();
            let eth_validator =
                EthTransactionValidatorBuilder::new(provider, EthEvmConfig::mainnet())
                    .no_eip4844()
                    .build(blob_store);
            let arc_validator = ArcTransactionValidator::new(eth_validator, None, config.clone());
            let outcome = arc_validator
                .validate_one_with_state(TransactionOrigin::External, tx.clone(), &mut None)
                .await;
            let TransactionValidationOutcome::Invalid(_, err) = &outcome else {
                panic!("expected Invalid outcome for {name}, got {outcome:?}");
            };
            let inner: &ArcTransactionValidatorError = err
                .downcast_other_ref::<ArcTransactionValidatorError>()
                .unwrap();
            assert!(
                matches!(inner, ArcTransactionValidatorError::DenylistedAddressError(addr) if *addr == denylisted_address),
                "expected DenylistedAddressError({denylisted_address}) for {name}, got {inner:?}"
            );
        }
    }

    static HITS: AtomicU64 = AtomicU64::new(0);
    static SIZE_LAST: AtomicU64 = AtomicU64::new(0);
    static INSERTS: AtomicU64 = AtomicU64::new(0);
    static BATCHES: AtomicU64 = AtomicU64::new(0);

    fn load(a: &AtomicU64) -> u64 {
        a.load(Ordering::Relaxed)
    }
    fn store(a: &AtomicU64, v: u64) {
        a.store(v, Ordering::Relaxed);
    }
    fn inc(a: &AtomicU64, v: u64) {
        a.fetch_add(v, Ordering::Relaxed);
    }

    fn reset_counters() {
        store(&HITS, 0);
        store(&SIZE_LAST, 0);
        store(&INSERTS, 0);
        store(&BATCHES, 0);
    }

    struct TestInvalidTxListMetrics;

    impl InvalidTxListMetricsSink for TestInvalidTxListMetrics {
        fn hit() {
            inc(&HITS, 1);
        }
        fn size(len: usize) {
            store(&SIZE_LAST, len as u64);
        }
        fn inserts(n: usize) {
            inc(&INSERTS, n as u64);
        }
        fn insert_batches() {
            inc(&BATCHES, 1);
        }
    }

    type TestInvalidTxList = InvalidTxListInner<TestInvalidTxListMetrics>;

    #[test]
    #[serial]
    fn reports_size_on_new_is_zero() {
        reset_counters();

        let _deny = TestInvalidTxList::new(8);

        assert_eq!(load(&SIZE_LAST), 0, "constructor should report size = 0");
        assert_eq!(load(&INSERTS), 0, "no inserts yet");
        assert_eq!(load(&BATCHES), 0, "no batches yet");
        assert_eq!(load(&HITS), 0, "no contains() calls yet");
    }

    #[test]
    #[serial]
    fn size_and_counters_after_insert_many() {
        reset_counters();

        let deny = TestInvalidTxList::new(8);

        // h1 duplicated once
        let h1 = TxHash::repeat_byte(1);
        let h2 = TxHash::repeat_byte(2);
        deny.insert_many([h1, h2, h1]);

        assert_eq!(load(&SIZE_LAST), 2, "unique size in LRU");
        assert_eq!(load(&INSERTS), 3, "attempted inserts (incl. dups)");
        assert_eq!(load(&BATCHES), 1, "one batch recorded");
        assert_eq!(load(&HITS), 0, "no contains() hits");
    }

    #[test]
    #[serial]
    fn contains_increments_hits_on_true_hit_only() {
        reset_counters();

        let deny = TestInvalidTxList::new(8);

        // Insert one hash so it will be found later.
        let h1 = TxHash::repeat_byte(42);
        deny.insert_many([h1]);

        // Hit: should increment `hits`.
        assert!(deny.contains(&h1));

        // Miss: should NOT increment `hits`.
        let h2 = TxHash::repeat_byte(99);
        assert!(!deny.contains(&h2));

        assert_eq!(load(&HITS), 1, "only the true hit increments the counter");
        assert_eq!(
            load(&SIZE_LAST),
            1,
            "LRU size should stay 1 after single insert"
        );
        assert_eq!(load(&INSERTS), 1, "one insert attempted");
        assert_eq!(load(&BATCHES), 1, "one batch recorded");
    }

    #[test]
    #[serial]
    fn capacity_is_enforced_and_eviction_occurs() {
        reset_counters();

        let cap = 3u32;
        let deny = TestInvalidTxList::new(cap);

        // Insert more unique hashes than capacity so eviction must happen.
        let keys: Vec<_> = (0u8..10).map(TxHash::repeat_byte).collect();
        deny.insert_many(keys.clone());

        assert_eq!(
            load(&SIZE_LAST),
            cap as u64,
            "size must be capped at capacity"
        );
        assert_eq!(load(&INSERTS), keys.len() as u64, "all attempts counted");
        assert_eq!(load(&BATCHES), 1, "one batch recorded so far");

        assert!(deny.contains(&TxHash::repeat_byte(9))); // hit
        assert!(deny.contains(&TxHash::repeat_byte(8))); // hit
        assert!(!deny.contains(&TxHash::repeat_byte(0))); // miss (evicted long ago)

        assert_eq!(load(&HITS), 2, "only true contains() calls count as hits");
    }

    #[test]
    #[serial]
    fn cap_one_eviction_behavior() {
        reset_counters();

        let deny = TestInvalidTxList::new(1);

        let h1 = TxHash::repeat_byte(1);
        let h2 = TxHash::repeat_byte(2);
        deny.insert_many([h1]);

        // Hit: should increment `hits`.
        assert!(deny.contains(&h1));

        // Should evict a, keep b
        deny.insert_many([h2]);
        assert!(!deny.contains(&h1));
        assert!(deny.contains(&h2));

        assert_eq!(load(&SIZE_LAST), 1, "LRU size should remain 1");
        assert_eq!(load(&INSERTS), 2, "two attempted inserts counted");
        assert_eq!(load(&BATCHES), 2, "two batches recorded");
        assert_eq!(load(&HITS), 2, "two successful contains() hits");
    }

    #[test]
    #[serial]
    fn duplicates_beyond_capacity_accounting() {
        reset_counters();

        let deny = TestInvalidTxList::new(3);

        let keys = [
            TxHash::repeat_byte(1),
            TxHash::repeat_byte(2),
            TxHash::repeat_byte(3),
            TxHash::repeat_byte(2),
            TxHash::repeat_byte(4), // triggers eviction
            TxHash::repeat_byte(3), // may be evicted, duplicate attempt
        ];
        deny.insert_many(keys);

        assert_eq!(
            load(&INSERTS),
            keys.len() as u64,
            "all attempts counted including duplicates"
        );
        assert_eq!(load(&SIZE_LAST), 3, "size capped at capacity");
        assert_eq!(load(&BATCHES), 1, "single batch recorded");
        assert_eq!(load(&HITS), 0, "no contains() calls made");
    }

    #[test]
    #[serial]
    fn cycling_eviction_hits_only_present() {
        reset_counters();

        let cap = 5u32;
        let deny = TestInvalidTxList::new(cap);
        // Cycle through 0..15 twice
        for _ in 0..2 {
            for i in 0u8..15u8 {
                deny.insert_many([TxHash::repeat_byte(i)]);
            }
        }
        // Only last cap keys should remain (10..14)
        for i in 0u8..10u8 {
            assert!(
                !deny.contains(&TxHash::repeat_byte(i)),
                "evicted key {} incorrectly present",
                i
            );
        }
        for i in 10u8..15u8 {
            assert!(
                deny.contains(&TxHash::repeat_byte(i)),
                "expected key {} to be present",
                i
            );
        }
        assert_eq!(load(&SIZE_LAST), cap as u64, "size capped at capacity");
        assert_eq!(load(&INSERTS), 30, "30 total attempted inserts (2 * 15)");
        assert_eq!(
            load(&BATCHES),
            30,
            "each single-item insert counted as a batch"
        );
        assert_eq!(load(&HITS), 5, "hits only for final 5 present keys");
    }

    #[test]
    #[serial]
    fn empty_batch_still_counts_as_batch() {
        reset_counters();

        let deny = TestInvalidTxList::new(8);

        // Insert an empty batch.
        deny.insert_many(std::iter::empty());

        // Metrics: batch counted, no inserts, size unchanged, no hits.
        assert_eq!(
            load(&BATCHES),
            1,
            "even empty insert_many counts as a batch"
        );
        assert_eq!(load(&INSERTS), 0, "no attempted inserts");
        assert_eq!(load(&SIZE_LAST), 0, "LRU size remains zero");
        assert_eq!(load(&HITS), 0, "no contains() hits");
    }
    #[test]
    #[serial]
    fn inserting_same_key_twice_does_not_increase_size() {
        reset_counters();

        let deny = TestInvalidTxList::new(8);
        let h = TxHash::repeat_byte(55);

        deny.insert_many([h]);
        let first_size = load(&SIZE_LAST);

        // Insert the same key again.
        deny.insert_many([h]);
        let second_size = load(&SIZE_LAST);

        assert_eq!(
            first_size, second_size,
            "reinserting same key shouldn't grow size"
        );
        assert_eq!(load(&INSERTS), 2, "two insert attempts counted");
        assert_eq!(load(&BATCHES), 2, "two batches recorded");
        assert_eq!(load(&HITS), 0, "no contains() hits");
    }

    #[test]
    #[serial]
    fn contains_does_not_promote_mru() {
        reset_counters();

        let deny = TestInvalidTxList::new(2);

        let a = TxHash::repeat_byte(1);
        let b = TxHash::repeat_byte(2);
        let c = TxHash::repeat_byte(3);

        // LRU order after these inserts: newest=b, oldest=a
        deny.insert_many([a, b]);
        assert_eq!(load(&SIZE_LAST), 2);

        // contains(a) MUST NOT promote since we use peek()
        assert!(deny.contains(&a)); // hit 1

        // Insert c -> should evict the oldest (which must still be 'a')
        deny.insert_many([c]);
        assert_eq!(load(&SIZE_LAST), 2);

        assert!(!deny.contains(&a)); // miss
        assert!(deny.contains(&b)); // hit 2
        assert!(deny.contains(&c)); // hit 3

        assert_eq!(load(&HITS), 3, "two post-insert hits + one pre-insert hit");
        assert_eq!(load(&INSERTS), 3, "three insert attempts counted");
        assert_eq!(load(&BATCHES), 2, "two batches recorded");
    }

    #[test]
    #[serial]
    fn exactly_at_capacity_no_eviction() {
        reset_counters();

        let cap = 3u32;
        let deny = TestInvalidTxList::new(cap);

        let a = TxHash::repeat_byte(1);
        let b = TxHash::repeat_byte(2);
        let c = TxHash::repeat_byte(3);
        let d = TxHash::repeat_byte(4);
        deny.insert_many([a, b, c]);

        assert_eq!(load(&SIZE_LAST), cap as u64, "size should equal capacity");
        assert_eq!(load(&INSERTS), 3, "three attempted inserts");
        assert_eq!(load(&BATCHES), 1, "one batch");

        assert!(deny.contains(&a));
        assert!(deny.contains(&b));
        assert!(deny.contains(&c));
        assert_eq!(load(&HITS), 3, "three successful contains() calls");
        assert!(!deny.contains(&d));
        assert_eq!(load(&HITS), 3, "still three successful contains() calls");
    }

    #[test]
    #[serial]
    fn duplicate_inserts_beyond_capacity_counts_attempts_and_capped_size() {
        reset_counters();

        let cap = 3;
        let deny = TestInvalidTxList::new(cap);

        let a = TxHash::repeat_byte(1);
        let b = TxHash::repeat_byte(2);
        let c = TxHash::repeat_byte(3);
        let d = TxHash::repeat_byte(4);

        // 6 attempts, with duplicates for a and c.
        deny.insert_many([a, a, b, c, c, d]);

        assert_eq!(load(&INSERTS), 6, "all attempts counted");
        assert_eq!(load(&BATCHES), 1, "one batch recorded");
        assert_eq!(load(&SIZE_LAST), cap as u64, "size capped at capacity");

        assert!(!deny.contains(&a));
        assert!(deny.contains(&b));
        assert!(deny.contains(&c));
        assert!(deny.contains(&d));
        assert_eq!(load(&HITS), 3, "only true hits are counted");
    }

    #[test]
    #[serial]
    fn zero_capacity_discards_everything() {
        reset_counters();

        let deny = TestInvalidTxList::new(0);

        let a = TxHash::repeat_byte(10);
        let b = TxHash::repeat_byte(11);
        let c = TxHash::repeat_byte(12);
        deny.insert_many([a, b, c]);

        assert_eq!(load(&SIZE_LAST), 0, "size must remain zero");
        assert_eq!(load(&INSERTS), 3, "attempts still counted");
        assert_eq!(load(&BATCHES), 1, "one batch recorded");

        // Nothing is present; contains() should miss and not increment hits.
        assert!(!deny.contains(&a));
        assert!(!deny.contains(&b));
        assert!(!deny.contains(&c));
        assert_eq!(load(&HITS), 0, "misses do not increment hits");
    }

    #[test]
    #[serial]
    fn multiple_batches_accumulate_properly() {
        reset_counters();

        let cap = 4u32;
        let deny = TestInvalidTxList::new(cap);

        let a = TxHash::repeat_byte(1);
        let b = TxHash::repeat_byte(2);
        let c = TxHash::repeat_byte(3);
        let d = TxHash::repeat_byte(4);
        let e = TxHash::repeat_byte(5);

        // Batch 1: 2 items
        deny.insert_many([a, b]);
        // Batch 2: 3 more (total uniques 5; size should end at min(5, cap)=4)
        deny.insert_many([c, d, e]);

        assert_eq!(load(&BATCHES), 2, "two batches recorded");
        assert_eq!(load(&INSERTS), 5, "all attempts counted");
        assert_eq!(load(&SIZE_LAST), cap as u64, "size capped at capacity");

        assert!(!deny.contains(&a));
        assert!(deny.contains(&b));
        assert!(deny.contains(&c));
        assert!(deny.contains(&d));
        assert!(deny.contains(&e));
        assert_eq!(load(&HITS), 4, "only hits increment");
    }

    #[test]
    #[serial]
    fn contains_on_missing_emits_no_metrics() {
        reset_counters();

        let deny = TestInvalidTxList::new(8);

        let x = TxHash::repeat_byte(200);
        assert!(!deny.contains(&x));

        assert_eq!(load(&HITS), 0, "miss should not count as a hit");
        assert_eq!(load(&SIZE_LAST), 0, "size still zero");
        assert_eq!(load(&INSERTS), 0, "no inserts attempted");
        assert_eq!(load(&BATCHES), 0, "no batches recorded");
    }

    #[test]
    #[serial]
    fn large_batch_eviction_correct() {
        reset_counters();

        let cap = 5u32;
        let deny = TestInvalidTxList::new(cap);

        let keys: Vec<_> = (0u8..100).map(TxHash::repeat_byte).collect();
        deny.insert_many(keys.clone());

        assert_eq!(load(&SIZE_LAST), cap as u64, "size capped at capacity");
        assert_eq!(load(&INSERTS), 100, "all attempts counted");
        assert_eq!(load(&BATCHES), 1, "one batch recorded");

        // Old ones should miss...
        assert!(!deny.contains(&TxHash::repeat_byte(0)));
        assert!(!deny.contains(&TxHash::repeat_byte(1)));

        // ...last 5 should hit.
        for b in 95u8..100u8 {
            assert!(deny.contains(&TxHash::repeat_byte(b)));
        }

        assert_eq!(load(&HITS), 5, "only the last five are present");
    }
}
