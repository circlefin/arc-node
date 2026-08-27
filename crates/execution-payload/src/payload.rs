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

//! Arc Node custom payload builder: InvalidTxFilteringPayloadBuilder.
//! Also, ArcNetworkPayloadBuilderBuilder is needed to inject it in reth_node_builder.
//! InvalidTxFilteringPayloadBuilder wraps ArcEthereumPayloadBuilder and
//! adds failed TXs to the invalid tx list when payload building fails or panics.
//! Panics during individual transaction execution are caught inline in
//! `arc_ethereum_payload` and converted to `UnprocessableTransactionError`.

use alloy_primitives::U256;
use alloy_primitives::{hex, TxHash, B256};
use alloy_rlp::Encodable;
use eyre::Result;
use reth_basic_payload_builder::{
    is_better_payload, BuildArguments, BuildOutcome, HeaderForPayload, MissingPayloadBehaviour,
    PayloadBuilder as RethPayloadBuilder, PayloadConfig,
};
use reth_chainspec::{ChainSpecProvider, EthChainSpec, EthereumHardforks};
use reth_consensus_common::validation::MAX_RLP_BLOCK_SIZE;
use reth_errors::{BlockExecutionError, BlockValidationError, ConsensusError};
use reth_ethereum::trie::updates::TrieUpdates;
use reth_ethereum_engine_primitives::EthPayloadAttributes;
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_ethereum_primitives::{EthPrimitives, TransactionSigned};
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome, BlockExecutor},
    ConfigureEvm, Evm, NextBlockEnvAttributes,
};
use reth_execution_cache::{
    CachedStateMetrics, CachedStateMetricsSource, CachedStateProvider, SavedCache,
};
use reth_node_api::{NodeTypes, PrimitivesTy};
use reth_node_builder::{
    components::PayloadBuilderBuilder, node::FullNodeTypes, BuilderContext, PayloadBuilderConfig,
};
use reth_payload_builder::{BlobSidecars, EthBuiltPayload};
use reth_payload_primitives::PayloadBuilderError;
use reth_primitives_traits::transaction::error::InvalidTransactionError;
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_storage_api::{StateProviderBox, StateProviderFactory};
use reth_transaction_pool::{
    error::InvalidPoolTransactionError, BestTransactions, BestTransactionsAttributes,
    PoolTransaction, TransactionPool, ValidPoolTransaction,
};
use reth_trie_parallel::state_root_task::StateRootHandle;
use revm::context_interface::result::InvalidTransaction;
use revm::context_interface::Block as _;
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Arc,
    time::{Duration, Instant},
};
use tracing::{debug, error, info, trace, warn};

use crate::builder::UnprocessableTransactionError;
use crate::metrics::PayloadBuildMetrics;
use arc_execution_txpool::InvalidTxList;
use arc_precompiles::helpers::ERR_BLOCKED_ADDRESS;

type BestTransactionsIter<Pool> = Box<
    dyn BestTransactions<Item = Arc<ValidPoolTransaction<<Pool as TransactionPool>::Transaction>>>,
>;

#[derive(Clone)]
pub struct ArcNetworkPayloadBuilderBuilder {
    invalid_tx_list: Option<InvalidTxList>,
    /// Custom payload builder maximum execution time, in milliseconds.
    /// When unset, Reth's `builder.deadline` is adopted.
    payload_builder_deadline_ms: Option<u64>,
    /// When true, `on_missing_payload` waits for the in-flight build instead of
    /// racing an empty block.
    wait_for_payload: bool,
}

impl ArcNetworkPayloadBuilderBuilder {
    pub fn new(
        invalid_tx_list: Option<InvalidTxList>,
        payload_builder_deadline_ms: Option<u64>,
        wait_for_payload: bool,
    ) -> Self {
        Self {
            invalid_tx_list,
            payload_builder_deadline_ms,
            wait_for_payload,
        }
    }
}

impl<Node, Pool, EvmCfg> PayloadBuilderBuilder<Node, Pool, EvmCfg>
    for ArcNetworkPayloadBuilderBuilder
where
    Node: FullNodeTypes,
    Node::Types: NodeTypes<ChainSpec: EthereumHardforks, Primitives = EthPrimitives>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = reth_node_api::TxTy<Node::Types>>>
        + Unpin
        + 'static,
    EvmCfg: ConfigureEvm<
            Primitives = PrimitivesTy<Node::Types>,
            NextBlockEnvCtx = NextBlockEnvAttributes,
        > + Clone
        + Send
        + 'static,
    <Node::Types as NodeTypes>::Payload: reth_node_api::PayloadTypes<
        BuiltPayload = EthBuiltPayload,
        PayloadAttributes = reth_ethereum_engine_primitives::EthPayloadAttributes,
    >,
{
    type PayloadBuilder = InvalidTxFilteringPayloadBuilder<
        ArcEthereumPayloadBuilder<Pool, Node::Provider, EvmCfg>,
        Pool,
    >;

    fn build_payload_builder(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
        evm_config: EvmCfg,
    ) -> impl std::future::Future<Output = Result<Self::PayloadBuilder>> + Send {
        let invalid_tx_list = self.invalid_tx_list.clone();
        let payload_builder_deadline_ms = self.payload_builder_deadline_ms;
        let provider = ctx.provider().clone();
        let conf = ctx.payload_builder_config();
        let chain = ctx.chain_spec().chain();
        let gas_limit = conf.gas_limit_for(chain);
        let deadline = payload_builder_deadline_ms
            .map(Duration::from_millis)
            .unwrap_or(ctx.config().builder.deadline);
        let loop_time_limit = Some(deadline);
        let wait_for_payload = self.wait_for_payload;
        async move {
            let inner = ArcEthereumPayloadBuilder::new(
                provider,
                pool.clone(),
                evm_config,
                EthereumBuilderConfig::new()
                    .with_gas_limit(gas_limit)
                    .with_await_payload_on_missing(wait_for_payload),
                loop_time_limit,
            );
            Ok(InvalidTxFilteringPayloadBuilder {
                inner,
                pool,
                invalid_tx_list,
            })
        }
    }
}

#[derive(Clone)]
pub struct InvalidTxFilteringPayloadBuilder<B, P> {
    inner: B,
    pool: P,
    invalid_tx_list: Option<InvalidTxList>,
}

impl<B, P> RethPayloadBuilder for InvalidTxFilteringPayloadBuilder<B, P>
where
    B: RethPayloadBuilder,
    P: TransactionPool + Unpin,
{
    type Attributes = <B as RethPayloadBuilder>::Attributes;
    type BuiltPayload = <B as RethPayloadBuilder>::BuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
        let res = catch_unwind(AssertUnwindSafe(|| self.inner.try_build(args)));
        handle_build_res(res, &self.pool, self.invalid_tx_list.as_ref())
    }

    fn on_missing_payload(
        &self,
        args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        self.inner.on_missing_payload(args)
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, HeaderForPayload<Self::BuiltPayload>>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        match catch_unwind(AssertUnwindSafe(|| self.inner.build_empty_payload(config))) {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(e)) => {
                purge_unprocessable_tx(&e, &self.pool, self.invalid_tx_list.as_ref());
                Err(e)
            }
            Err(panic) => {
                purge_pending_and_resume_panic(panic, &self.pool, self.invalid_tx_list.as_ref())
            }
        }
    }
}

/// Type alias for the result of `catch_unwind` wrapping a payload build operation.
type CatchUnwindBuildResult<T> =
    Result<Result<BuildOutcome<T>, PayloadBuilderError>, Box<dyn std::any::Any + Send>>;

/// If the error wraps an `UnprocessableTransactionError`, purge that transaction
/// from the pool and add it to the invalid tx list. The error is always returned
/// unchanged so the caller can propagate it.
fn purge_unprocessable_tx<P: TransactionPool>(
    e: &PayloadBuilderError,
    pool: &P,
    invalid_tx_list: Option<&InvalidTxList>,
) {
    if let Some(tx_hash) = extract_unprocessable_tx_hash(e) {
        if let Some(tx) = pool.get(&tx_hash) {
            log_transaction_details(&tx, "unprocessable transaction details");
        } else {
            error!(tx_hash = %tx_hash, "unprocessable transaction not found in pool");
        }

        error!(tx_hash = %tx_hash, quarantined = invalid_tx_list.is_some(), "evicting unprocessable transaction from pool");
        evict_unincludable_txs(pool, invalid_tx_list, vec![tx_hash]);
    }
}

/// Purge all pending transactions from the pool into the invalid tx list, then
/// resume the panic. This function never returns.
fn purge_pending_and_resume_panic<P: TransactionPool>(
    panic: Box<dyn std::any::Any + Send>,
    pool: &P,
    invalid_tx_list: Option<&InvalidTxList>,
) -> ! {
    let pending_hashes: Vec<TxHash> = pool
        .pending_transactions()
        .iter()
        .inspect(|tx| log_transaction_details(tx, "pending TX data on payload builder panic"))
        .map(|tx| *tx.hash())
        .collect();

    error!(
        quarantined = invalid_tx_list.is_some(),
        "payload builder panicked, evicting all PENDING TXs from pool"
    );
    evict_unincludable_txs(pool, invalid_tx_list, pending_hashes);
    std::panic::resume_unwind(panic)
}

/// Handles the result of a `catch_unwind` call around the inner payload builder's
/// `try_build`.
///
/// This function processes three cases:
/// 1. Success: Returns the build outcome directly
/// 2. Builder error: Purges unprocessable transactions, then returns the error
/// 3. Panic: Purges all pending transactions, then resumes the panic
fn handle_build_res<T, P: TransactionPool>(
    res: CatchUnwindBuildResult<T>,
    pool: &P,
    invalid_tx_list: Option<&InvalidTxList>,
) -> Result<BuildOutcome<T>, PayloadBuilderError> {
    match res {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(e)) => {
            purge_unprocessable_tx(&e, pool, invalid_tx_list);
            Err(e)
        }
        Err(panic) => purge_pending_and_resume_panic(panic, pool, invalid_tx_list),
    }
}

/// Logs detailed information about a transaction.
fn log_transaction_details<T: PoolTransaction>(tx: &Arc<ValidPoolTransaction<T>>, context: &str) {
    info!(
        tx_hash = %tx.hash(),
        tx_type = %tx.tx_type(),
        sender = %tx.sender(),
        to = ?tx.to(),
        id = ?tx.id(),
        encoded_length = %tx.encoded_length(),
        nonce = %tx.nonce(),
        gas_limit = %tx.gas_limit(),
        cost = ?tx.cost(),
        max_fee_per_gas = ?tx.max_fee_per_gas(),
        priority_fee_or_price = ?tx.priority_fee_or_price(),
        is_local = %tx.is_local(),
        is_eip4844 = %tx.is_eip4844(),
        authorization_count = ?tx.authorization_count(),
        value = ?tx.transaction.value(),
        input_len = %tx.transaction.input().len(),
        input_dump = %dump_tx_data(tx.transaction.input()),
        "{}", context
    );
}

/// Extracts the transaction hash from an UnprocessableTransactionError if present.
///
/// Error structure:
/// PayloadBuilderError::Other(UnprocessableTransactionError)
fn extract_unprocessable_tx_hash(err: &PayloadBuilderError) -> Option<TxHash> {
    use reth_payload_primitives::PayloadBuilderError as PBE;

    match err {
        PBE::Other(boxed_err) => boxed_err
            .downcast_ref::<UnprocessableTransactionError>()
            .map(|e| e.tx_hash),
        _ => None,
    }
}

/// Introduced to improve testability of `evict_unincludable_txs`
trait PendingPool {
    fn remove_transactions_and_descendants(&self, hashes: Vec<TxHash>) -> usize;
    fn pending_len(&self) -> usize;
}

impl<T: TransactionPool> PendingPool for T {
    fn remove_transactions_and_descendants(&self, hashes: Vec<TxHash>) -> usize {
        self.remove_transactions_and_descendants(hashes).len()
    }
    fn pending_len(&self) -> usize {
        self.pending_transactions().len()
    }
}

/// Evicts permanently un-includable transactions.
///
/// Pool removal is unconditional — it frees the slot and unblocks the sender's nonce, which
/// is the remediation and must happen regardless of whether the invalid-tx-list
/// feature is enabled. Quarantining the hash in the `InvalidTxList` is an
/// optional re-gossip suppression on top, applied only when a list is configured.
fn evict_unincludable_txs<P: PendingPool>(
    pool: &P,
    invalid_tx_list: Option<&InvalidTxList>,
    hashes: Vec<TxHash>,
) {
    let before = pool.pending_len();

    if hashes.is_empty() {
        error!("evict_unincludable_txs: no transactions to evict");
    }

    if let Some(invalid_tx_list) = invalid_tx_list {
        invalid_tx_list.insert_many(hashes.iter().copied());
    }
    let removed = pool.remove_transactions_and_descendants(hashes);
    warn!(
        removed,
        pending_before = before,
        pending_after = pool.pending_len(),
        quarantined = invalid_tx_list.is_some(),
        "evicted unincludable txs from pool"
    );
}

/// True when a tx can never be included under the current block gas limit (as opposed to
/// merely not fitting this block's remaining gas, which can be retried in a fresh block).
fn exceeds_block_gas_limit_permanently(tx_gas_limit: u64, block_gas_limit: u64) -> bool {
    tx_gas_limit > block_gas_limit
}

/// True when an EVM rejection is a blocklist hit. A blocklisted address can never produce an
/// includable tx until it is unblocklisted, so such a tx is permanently un-includable.
fn is_blocked_address_error(err: Option<&InvalidTransaction>) -> bool {
    matches!(err, Some(InvalidTransaction::Str(msg)) if msg.as_ref() == ERR_BLOCKED_ADDRESS)
}

/// Format TX data as a multi-line hexdump if too long.
fn dump_tx_data(bytes: &[u8]) -> String {
    const INLINE_LIMIT_BYTES: usize = 512;
    const BYTES_PER_LINE: usize = 128;
    const MAX_LINES: usize = 64; // safety cap => 128 * 64 = 8192 bytes shown max

    if bytes.len() <= INLINE_LIMIT_BYTES {
        return hex::encode(bytes);
    }

    let mut out = String::new();
    let mut offset = 0usize;
    let mut lines = 0usize;
    while offset < bytes.len() && lines < MAX_LINES {
        let end = (offset.saturating_add(BYTES_PER_LINE)).min(bytes.len());
        let slice = &bytes[offset..end];
        out.push_str(&format!("{:04x}: {}\n", offset, hex::encode(slice)));
        offset = end;
        lines = lines.saturating_add(1);
    }
    if offset < bytes.len() {
        out.push_str(&format!(
            "... truncated after {} bytes ({} total)",
            offset,
            bytes.len()
        ));
    }
    out
}

/// Arc's Custom payload builder based on upstream Reth:
/// https://github.com/paradigmxyz/reth/blob/74351d98e906b8af5f118694529fb2b71d316946/crates/ethereum/payload/src/lib.rs#L138
/// Enforces a time budget to avoid overruns under heavy mempool load.
/// The rest is following the logic in EthereumPayloadBuilder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArcEthereumPayloadBuilder<Pool, Client, EvmConfig> {
    /// Client providing access to node state.
    client: Client,
    /// Transaction pool.
    pool: Pool,
    /// The type responsible for creating the evm.
    evm_config: EvmConfig,
    /// Payload builder configuration.
    builder_config: EthereumBuilderConfig,
    /// Optional time limit for the main transaction selection loop.
    loop_time_limit: Option<Duration>,
}

impl<Pool, Client, EvmConfig> ArcEthereumPayloadBuilder<Pool, Client, EvmConfig> {
    /// `EthereumPayloadBuilder` constructor.
    pub const fn new(
        client: Client,
        pool: Pool,
        evm_config: EvmConfig,
        builder_config: EthereumBuilderConfig,
        loop_time_limit: Option<Duration>,
    ) -> Self {
        Self {
            client,
            pool,
            evm_config,
            builder_config,
            loop_time_limit,
        }
    }
}

impl<Pool, Client, EvmConfig> RethPayloadBuilder
    for ArcEthereumPayloadBuilder<Pool, Client, EvmConfig>
where
    EvmConfig: ConfigureEvm<Primitives = EthPrimitives, NextBlockEnvCtx = NextBlockEnvAttributes>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: EthereumHardforks> + Clone,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
{
    type Attributes = EthPayloadAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<EthPayloadAttributes, EthBuiltPayload>,
    ) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError> {
        arc_ethereum_payload(
            self.evm_config.clone(),
            self.client.clone(),
            self.pool.clone(),
            self.builder_config.clone(),
            self.loop_time_limit,
            args,
            |attributes| self.pool.best_transactions_with_attributes(attributes),
        )
    }

    /// Await the build in flight instead of racing a redundant second build via
    /// `build_empty_payload`.
    fn on_missing_payload(
        &self,
        _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
    ) -> MissingPayloadBehaviour<Self::BuiltPayload> {
        if self.builder_config.await_payload_on_missing {
            MissingPayloadBehaviour::AwaitInProgress
        } else {
            MissingPayloadBehaviour::RaceEmptyPayload
        }
    }

    fn build_empty_payload(
        &self,
        config: PayloadConfig<Self::Attributes, HeaderForPayload<Self::BuiltPayload>>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        let args = BuildArguments::new(
            Default::default(),
            Default::default(),
            None,
            config,
            Default::default(),
            None,
        );

        // This is what's done in upstream EthereumPayloadBuilder::build_empty_payload
        arc_ethereum_payload(
            self.evm_config.clone(),
            self.client.clone(),
            self.pool.clone(),
            self.builder_config.clone(),
            self.loop_time_limit,
            args,
            |attributes| self.pool.best_transactions_with_attributes(attributes),
        )?
        .into_payload()
        .ok_or_else(|| PayloadBuilderError::MissingPayload)
    }
}

/// Proposer revenue contributed by a single transaction on Arc.
///
/// On Arc, Proposer revenue equals `effective_gas_price * gas_used`, not
/// `effective_tip_per_gas * gas_used` (the upstream-reth formula, which
/// assumes base fees are burned).
fn proposer_revenue<T: alloy_consensus::Transaction>(tx: &T, gas_used: u64, base_fee: u64) -> U256 {
    let effective_gas_price = tx.effective_gas_price(Some(base_fee));
    // u128 * u64 fits in U256 (max 192 bits);
    // bounded by block_gas_limit * max_fee_per_gas.
    #[allow(clippy::arithmetic_side_effects)]
    {
        U256::from(effective_gas_price) * U256::from(gas_used)
    }
}

/// Outcome of running a single transaction in the payload-building loop.
enum TxOutcome {
    /// Transaction executed successfully — record `gas_used`, advance.
    Included(u64),
    /// Skip this tx silently (e.g. nonce too low). Caller continues.
    Skip,
    /// Skip and mark the tx invalid in the pool so descendants are evicted.
    SkipAndMarkInvalid,
    /// Tx was rejected because its sender or recipient is blocklisted. Skip, mark
    /// invalid, and evict from the pool — a blocklisted address is permanently
    /// un-includable until unblocklisted.
    SkipMarkInvalidAndEvictBlocked,
    /// Tx gas limit exceeds the gas remaining in the block. Skip and mark invalid
    /// with the executor's reported limits so descendants are evicted.
    SkipExceedsGasLimit {
        transaction_gas_limit: u64,
        block_available_gas: u64,
    },
    /// Unrecoverable error for this build attempt — propagate.
    Fatal(PayloadBuilderError),
}

/// Classifies the result of `catch_unwind(|| builder.execute_transaction(...))`
/// into one of six loop-control outcomes. The caller is responsible for invoking
/// `best_txs.mark_invalid(...)` (and pool eviction) on the `SkipAndMarkInvalid`,
/// `SkipMarkInvalidAndEvictBlocked`, and `SkipExceedsGasLimit` arms — kept out of
/// this helper so the signature stays non-generic.
fn classify_tx_outcome(
    result: std::thread::Result<Result<u64, BlockExecutionError>>,
    tx_hash: TxHash,
    tx: &TransactionSigned,
) -> TxOutcome {
    match result {
        Ok(Ok(gas_used)) => TxOutcome::Included(gas_used),
        Ok(Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
            error, ..
        }))) => {
            if error.is_nonce_too_low() {
                trace!(target: "payload_builder", %error, ?tx, "(arc) skipping nonce too low transaction");
                TxOutcome::Skip
            } else if is_blocked_address_error(error.as_invalid_tx_err()) {
                trace!(target: "payload_builder", %error, ?tx, "(arc) evicting blocklisted transaction and its descendants");
                TxOutcome::SkipMarkInvalidAndEvictBlocked
            } else {
                trace!(target: "payload_builder", %error, ?tx, "(arc) skipping invalid transaction and its descendants");
                TxOutcome::SkipAndMarkInvalid
            }
        }
        // The executor is the source of truth for block gas availability. Keep this
        // non-fatal in case local builder accounting diverges from executor rules.
        Ok(Err(BlockExecutionError::Validation(
            BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit,
                block_available_gas,
            },
        ))) => {
            trace!(target: "payload_builder", %transaction_gas_limit, %block_available_gas, ?tx, "(arc) skipping transaction exceeding block gas limit");
            TxOutcome::SkipExceedsGasLimit {
                transaction_gas_limit,
                block_available_gas,
            }
        }
        Ok(Err(err)) => TxOutcome::Fatal(PayloadBuilderError::evm(err)),
        Err(_panic_payload) => {
            TxOutcome::Fatal(PayloadBuilderError::other(UnprocessableTransactionError {
                tx_hash,
            }))
        }
    }
}

/// True when an optional time budget has been exceeded.
fn time_budget_exhausted(started: Instant, limit: Option<Duration>) -> bool {
    limit.is_some_and(|l| started.elapsed() >= l)
}

/// True when the Osaka hardfork is active AND the candidate block size
/// exceeds [`MAX_RLP_BLOCK_SIZE`].
fn osaka_size_exceeded(is_osaka: bool, size: usize) -> bool {
    is_osaka && size > MAX_RLP_BLOCK_SIZE
}

/// Returns `state_provider` wrapped in a [`CachedStateProvider`] when an
/// execution cache is present, otherwise returns it unwrapped. Centralises
/// the conditional so the caller stays free of control flow.
fn maybe_wrap_with_execution_cache(
    state_provider: StateProviderBox,
    execution_cache: Option<SavedCache>,
) -> StateProviderBox {
    if let Some(cache) = execution_cache {
        // reth 2.2 removed `SavedCache::metrics()`, so we construct fresh builder-sourced
        // metrics per build, matching the default payload builder. Per-cache hit/miss
        // continuity is lost; global Prometheus aggregates are unaffected because the
        // metric handles are shared.
        Box::new(CachedStateProvider::new(
            state_provider,
            cache.cache().clone(),
            CachedStateMetrics::zeroed(CachedStateMetricsSource::Builder),
        ))
    } else {
        state_provider
    }
}

/// Drives the sparse-trie state-root task to completion, falling back to
/// sync state-root computation (returns `None`) on failure. Returns the
/// precomputed `(state_root, trie_updates)` pair to hand to
/// `BlockBuilder::finish` when the parallel result is usable.
///
/// Caller must clear the builder's state hook before invoking — the helper
/// stays non-generic by keeping the builder out of its signature.
fn try_precomputed_state_root(
    trie_handle: Option<StateRootHandle>,
    payload_id: &impl std::fmt::Display,
) -> Option<(B256, TrieUpdates)> {
    let mut handle = trie_handle?;
    match handle.state_root() {
        Ok(outcome) => {
            debug!(target: "payload_builder", id=%payload_id, state_root=?outcome.state_root, "(arc) received state root from sparse trie");
            Some((
                outcome.state_root,
                Arc::unwrap_or_clone(outcome.trie_updates),
            ))
        }
        Err(err) => {
            warn!(target: "payload_builder", id=%payload_id, %err, "(arc) sparse trie failed, falling back to sync state root");
            None
        }
    }
}

/// Constructs an transaction payload using the best transactions from the pool.
/// It follows the upstream Ethereum payload building logic with a Arc-specific deadline for the main loop.
///
///
/// Given build arguments including an Ethereum client, transaction pool,
/// and configuration, this function creates a transaction payload. Returns
/// a result indicating success with the payload or an error in case of failure.
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn arc_ethereum_payload<EvmConfig, Client, Pool, F>(
    evm_config: EvmConfig,
    client: Client,
    pool: Pool,
    builder_config: EthereumBuilderConfig,
    loop_time_limit: Option<Duration>,
    args: BuildArguments<EthPayloadAttributes, EthBuiltPayload>,
    best_txs: F,
) -> Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError>
where
    EvmConfig: ConfigureEvm<Primitives = EthPrimitives, NextBlockEnvCtx = NextBlockEnvAttributes>,
    Client: StateProviderFactory + ChainSpecProvider<ChainSpec: EthereumHardforks>,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TransactionSigned>>,
    F: FnOnce(BestTransactionsAttributes) -> BestTransactionsIter<Pool>,
{
    let BuildArguments {
        mut cached_reads,
        execution_cache,
        trie_handle,
        config,
        cancel,
        best_payload,
    } = args;
    let PayloadConfig {
        parent_header,
        attributes,
        payload_id,
    } = config;

    let total_start = Instant::now();

    let stage_start = Instant::now();
    let state_provider = maybe_wrap_with_execution_cache(
        client.state_by_block_hash(parent_header.hash())?,
        execution_cache,
    );
    let state = StateProviderDatabase::new(state_provider.as_ref());
    let mut db = State::builder()
        .with_database(cached_reads.as_db_mut(state))
        .with_bundle_update()
        .build();
    PayloadBuildMetrics::record_stage_state_setup(stage_start.elapsed());

    let mut builder = evm_config
        .builder_for_next_block(
            &mut db,
            &parent_header,
            NextBlockEnvAttributes {
                timestamp: attributes.timestamp,
                suggested_fee_recipient: attributes.suggested_fee_recipient,
                prev_randao: attributes.prev_randao,
                gas_limit: builder_config.gas_limit(parent_header.gas_limit),
                parent_beacon_block_root: attributes.parent_beacon_block_root,
                withdrawals: attributes.withdrawals.clone().map(Into::into),
                extra_data: builder_config.extra_data,
                slot_number: None,
            },
        )
        .map_err(PayloadBuilderError::other)?;

    let chain_spec = client.chain_spec();

    info!(target: "payload_builder", id=%payload_id, parent_header = ?parent_header.hash(), parent_number = parent_header.number, "(arc) building new payload");
    let mut cumulative_gas_used = 0u64;
    let block_gas_limit: u64 = builder.evm_mut().block().gas_limit();
    let base_fee = builder.evm_mut().block().basefee();

    let mut best_txs = best_txs(BestTransactionsAttributes::new(
        base_fee,
        None, // Explicitly disable blob transactions by not providing a blob gas price.
    ));
    let mut total_fees = U256::ZERO;

    trie_handle.as_ref().inspect(|handle| {
        builder
            .executor_mut()
            .set_state_hook(Some(Box::new(handle.state_hook())));
    });

    let stage_start = Instant::now();
    builder.apply_pre_execution_changes().map_err(|err| {
        warn!(target: "payload_builder", %err, "(arc) failed to apply pre-execution changes");
        PayloadBuilderError::Internal(err.into())
    })?;
    PayloadBuildMetrics::record_stage_pre_execution(stage_start.elapsed());

    let mut block_transactions_rlp_length = 0usize;
    let is_osaka = chain_spec.is_osaka_active_at_timestamp(attributes.timestamp);

    let withdrawals_rlp_length = attributes.withdrawals.as_ref().map_or(0, |w| w.length());

    let loop_started = Instant::now();

    while let Some(pool_tx) = best_txs.next() {
        if time_budget_exhausted(loop_started, loop_time_limit) {
            #[allow(clippy::cast_possible_truncation)]
            let elapsed_ms = loop_started.elapsed().as_millis() as u64;
            warn!(elapsed_ms, "(arc) loop time budget reached; sealing early");
            break;
        }

        // ensure we still have capacity for this transaction
        if block_gas_limit
            < cumulative_gas_used
                .checked_add(pool_tx.gas_limit())
                .expect("total gas shouldn't overflow")
        {
            // we can't fit this transaction into the block, so we need to mark it as invalid
            // which also removes all dependent transaction from the iterator before we can
            // continue
            best_txs.mark_invalid(
                &pool_tx,
                &InvalidPoolTransactionError::ExceedsGasLimit(pool_tx.gas_limit(), block_gas_limit),
            );
            // A tx whose gas limit exceeds the block gas limit can never be included in any
            // block, so evict it from the pool instead of skipping it every build. This is a
            // recoverable condition (the limit may be raised), so we only free the pool slot
            // and leave re-admission to the validator's stateful gas-limit check rather than
            // quarantining the hash in the invalid tx list.
            if exceeds_block_gas_limit_permanently(pool_tx.gas_limit(), block_gas_limit) {
                warn!(target: "payload_builder", tx_hash = %pool_tx.hash(), tx_gas_limit = pool_tx.gas_limit(), block_gas_limit, "(arc) evicting permanently un-includable transaction (exceeds block gas limit)");
                evict_unincludable_txs(&pool, None, vec![*pool_tx.hash()]);
            }
            continue;
        }

        // check if the job was cancelled, if so we can exit early
        if cancel.is_cancelled() {
            PayloadBuildMetrics::record_stage_tx_execution(loop_started.elapsed());
            PayloadBuildMetrics::record_outcome_cancelled();
            PayloadBuildMetrics::record_total_duration(total_start);
            return Ok(BuildOutcome::Cancelled);
        }

        // convert tx to a signed transaction
        let tx = pool_tx.to_consensus();

        let tx_rlp_len = tx.inner().length();

        let estimated_block_size_with_tx = block_transactions_rlp_length
            .saturating_add(tx_rlp_len)
            .saturating_add(withdrawals_rlp_length)
            .saturating_add(1024); // 1Kb of overhead for the block header

        if osaka_size_exceeded(is_osaka, estimated_block_size_with_tx) {
            best_txs.mark_invalid(
                &pool_tx,
                &InvalidPoolTransactionError::OversizedData {
                    size: estimated_block_size_with_tx,
                    limit: MAX_RLP_BLOCK_SIZE,
                },
            );
            continue;
        }

        let raw_result = catch_unwind(AssertUnwindSafe(|| {
            builder
                .execute_transaction(tx.clone())
                .map(|out| out.tx_gas_used())
        }));
        let gas_used = match classify_tx_outcome(raw_result, *pool_tx.hash(), &tx) {
            TxOutcome::Included(g) => g,
            TxOutcome::Skip => continue,
            TxOutcome::SkipAndMarkInvalid => {
                best_txs.mark_invalid(
                    &pool_tx,
                    &InvalidPoolTransactionError::Consensus(
                        InvalidTransactionError::TxTypeNotSupported,
                    ),
                );
                continue;
            }
            TxOutcome::SkipMarkInvalidAndEvictBlocked => {
                best_txs.mark_invalid(
                    &pool_tx,
                    &InvalidPoolTransactionError::Consensus(
                        InvalidTransactionError::TxTypeNotSupported,
                    ),
                );
                // A blocklisted address can never produce an includable tx until it is
                // unblocklisted, so evict it from the pool instead of skipping it every
                // build. This is a recoverable condition (the address may be
                // unblocklisted), so we only free the pool slot and leave re-admission to
                // the validator's stateful blocklist check rather than quarantining the
                // hash in the invalid tx list. Other invalid errors (e.g. insufficient
                // funds) may become valid later, so they keep the skip-only behavior above.
                warn!(target: "payload_builder", tx_hash = %pool_tx.hash(), sender = %pool_tx.sender(), to = ?pool_tx.to(), "(arc) evicting permanently un-includable transaction (blocklisted address)");
                evict_unincludable_txs(&pool, None, vec![*pool_tx.hash()]);
                continue;
            }
            TxOutcome::SkipExceedsGasLimit {
                transaction_gas_limit,
                block_available_gas,
            } => {
                best_txs.mark_invalid(
                    &pool_tx,
                    &InvalidPoolTransactionError::ExceedsGasLimit(
                        transaction_gas_limit,
                        block_available_gas,
                    ),
                );
                continue;
            }
            TxOutcome::Fatal(e) => return Err(e),
        };

        block_transactions_rlp_length = block_transactions_rlp_length.saturating_add(tx_rlp_len);

        #[allow(clippy::arithmetic_side_effects)]
        {
            total_fees += proposer_revenue(tx.inner(), gas_used, base_fee);
        }
        cumulative_gas_used = cumulative_gas_used
            .checked_add(gas_used)
            .expect("total gas shouldn't overflow");
    }

    PayloadBuildMetrics::record_stage_tx_execution(loop_started.elapsed());

    // check if we have a better block
    if !is_better_payload(best_payload.as_ref(), total_fees) {
        // Release db
        drop(builder);
        PayloadBuildMetrics::record_outcome_aborted();
        PayloadBuildMetrics::record_total_duration(total_start);
        // can skip building the block
        return Ok(BuildOutcome::Aborted {
            fees: total_fees,
            cached_reads,
        });
    }

    let builder_finish = Instant::now();
    // `set_state_hook(None)` is idempotent; call unconditionally so the
    // sparse-trie hook (if any) is always cleared before block finalization.
    builder.executor_mut().set_state_hook(None);
    let precomputed = try_precomputed_state_root(trie_handle, &payload_id);
    let BlockBuilderOutcome {
        execution_result,
        block,
        ..
    } = builder.finish(state_provider.as_ref(), precomputed)?;
    PayloadBuildMetrics::record_stage_post_execution(builder_finish.elapsed());

    let stage_start = Instant::now();
    let requests = chain_spec
        .is_prague_active_at_timestamp(attributes.timestamp)
        .then_some(execution_result.requests);

    let sealed_block = Arc::new(block.sealed_block().clone());
    debug!(target: "payload_builder", id=%payload_id, sealed_block_header = ?sealed_block.sealed_header(), "(arc) sealed built block");

    if osaka_size_exceeded(is_osaka, sealed_block.rlp_length()) {
        PayloadBuildMetrics::record_stage_assembly_and_sealing(stage_start.elapsed());
        PayloadBuildMetrics::record_total_duration(total_start);
        return Err(PayloadBuilderError::other(ConsensusError::BlockTooLarge {
            rlp_length: sealed_block.rlp_length(),
            max_rlp_length: MAX_RLP_BLOCK_SIZE,
        }));
    }

    let payload = EthBuiltPayload::new(sealed_block, total_fees, requests, None)
        // add blob sidecars from the executed txs; empty for now
        .with_sidecars(BlobSidecars::Empty);
    PayloadBuildMetrics::record_stage_assembly_and_sealing(stage_start.elapsed());

    PayloadBuildMetrics::record_outcome_better();
    PayloadBuildMetrics::record_total_duration(total_start);

    Ok(BuildOutcome::Better {
        payload,
        cached_reads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::TxHash;
    use reth_transaction_pool::test_utils::testing_pool;
    use std::panic::AssertUnwindSafe;

    #[derive(Clone, Debug)]
    struct MockPendingPool {
        hashes: Vec<TxHash>,
        removed: std::cell::RefCell<Vec<TxHash>>,
    }
    impl MockPendingPool {
        fn new(hashes: Vec<TxHash>) -> Self {
            Self {
                hashes,
                removed: std::cell::RefCell::new(Vec::new()),
            }
        }
    }
    impl PendingPool for MockPendingPool {
        fn remove_transactions_and_descendants(&self, hashes: Vec<TxHash>) -> usize {
            let len = hashes.len();
            self.removed.borrow_mut().extend(hashes);
            len
        }
        fn pending_len(&self) -> usize {
            self.hashes.len()
        }
    }

    #[test]
    fn exceeds_block_gas_limit_permanently_classifies_correctly() {
        // Strictly greater than the block limit: can never fit any block.
        assert!(exceeds_block_gas_limit_permanently(30000001, 30000000));
        // Equal to or below the block limit: fits a fresh block, only temporarily skipped.
        assert!(!exceeds_block_gas_limit_permanently(30000000, 30000000));
        assert!(!exceeds_block_gas_limit_permanently(21000, 30000000));
    }

    #[test]
    fn is_blocked_address_error_only_matches_blocklist() {
        // Blocklist rejection: permanent until unblocklisted -> evict.
        let blocked = InvalidTransaction::Str(ERR_BLOCKED_ADDRESS.into());
        assert!(is_blocked_address_error(Some(&blocked)));

        // Other string errors must not be treated as blocklist hits.
        let other = InvalidTransaction::Str("some other error".into());
        assert!(!is_blocked_address_error(Some(&other)));

        // Temporarily-invalid errors may become valid later -> keep.
        assert!(!is_blocked_address_error(Some(
            &InvalidTransaction::NonceTooLow { tx: 1, state: 2 }
        )));
        assert!(!is_blocked_address_error(Some(
            &InvalidTransaction::LackOfFundForMaxFee {
                fee: Box::new(U256::from(1)),
                balance: Box::new(U256::ZERO),
            }
        )));

        // No invalid-tx error at all (e.g. a non-InvalidTransaction rejection).
        assert!(!is_blocked_address_error(None));
    }

    #[test]
    fn evict_unincludable_txs_inserts_all() {
        let hashes: Vec<TxHash> = (0..3).map(TxHash::repeat_byte).collect();
        let pool = MockPendingPool::new(hashes.clone());
        let invalid_tx_list = InvalidTxList::new(16);
        evict_unincludable_txs(&pool, Some(&invalid_tx_list), hashes.clone());
        assert_eq!(hashes.len(), invalid_tx_list.len());
        for h in hashes {
            assert!(invalid_tx_list.contains(&h));
        }
    }

    #[test]
    fn evict_unincludable_txs_empty_no_insert() {
        let pool = MockPendingPool::new(vec![]);
        let invalid_tx_list = InvalidTxList::new(16);
        evict_unincludable_txs(&pool, Some(&invalid_tx_list), vec![]);
        assert_eq!(0, invalid_tx_list.len());
    }

    #[test]
    fn evict_unincludable_txs_removes_from_pool() {
        let hashes: Vec<TxHash> = (0..5).map(TxHash::repeat_byte).collect();
        let pool = MockPendingPool::new(hashes.clone());
        let invalid_tx_list = InvalidTxList::new(64);
        evict_unincludable_txs(&pool, Some(&invalid_tx_list), hashes.clone());
        assert_eq!(hashes.len(), invalid_tx_list.len());
        for h in &hashes {
            assert!(invalid_tx_list.contains(h));
        }

        let removed = pool.removed.borrow().clone();
        assert_eq!(hashes.len(), removed.len());
        for h in &hashes {
            assert!(removed.contains(h));
        }
    }

    #[test]
    fn evict_unincludable_txs_removes_from_pool_when_list_disabled() {
        // When the invalid tx list is disabled (None), pool removal must still happen —
        // this is the remediation under `--invalid-tx-list-enable=false`.
        let hashes: Vec<TxHash> = (0..4).map(TxHash::repeat_byte).collect();
        let pool = MockPendingPool::new(hashes.clone());
        evict_unincludable_txs(&pool, None, hashes.clone());

        let removed = pool.removed.borrow().clone();
        assert_eq!(hashes.len(), removed.len());
        for h in &hashes {
            assert!(removed.contains(h));
        }
    }

    #[test]
    fn extract_unprocessable_tx_hash_extracts_correctly() {
        let test_hash = TxHash::repeat_byte(0xCD);
        let unproc_err = UnprocessableTransactionError { tx_hash: test_hash };
        let payload_err = PayloadBuilderError::other(unproc_err);

        let extracted = extract_unprocessable_tx_hash(&payload_err);
        assert_eq!(
            extracted,
            Some(test_hash),
            "Should extract the transaction hash"
        );
    }

    #[test]
    fn extract_unprocessable_tx_hash_returns_none_for_other_errors() {
        // Test with a different PayloadBuilderError variant
        let payload_err = PayloadBuilderError::MissingPayload;

        let extracted = extract_unprocessable_tx_hash(&payload_err);
        assert_eq!(
            extracted, None,
            "Should return None for non-EvmExecutionError"
        );
    }

    #[test]
    fn extract_unprocessable_tx_hash_returns_none_for_non_unprocessable_other_error() {
        let other_err = std::io::Error::other("dummy");
        let payload_err = PayloadBuilderError::other(other_err);

        let extracted = extract_unprocessable_tx_hash(&payload_err);
        assert_eq!(
            extracted, None,
            "Should return None for Other errors that are not UnprocessableTransactionError"
        );
    }

    #[tracing_test::traced_test]
    #[test]
    fn log_transaction_details_logs_expected_fields() {
        use reth_transaction_pool::test_utils::{MockTransaction, MockTransactionFactory};

        let mut factory = MockTransactionFactory::default();
        let tx = MockTransaction::eip1559();
        let valid_tx = factory.validated_arc(tx);

        log_transaction_details(&valid_tx, "test context");

        assert!(logs_contain("tx_hash"));
        assert!(logs_contain("sender"));
        assert!(logs_contain("to"));
        assert!(logs_contain("input_dump"));
        assert!(logs_contain("nonce"));
        assert!(logs_contain("gas_limit"));
        assert!(logs_contain("test context"));
    }

    #[test]
    fn dump_tx_data_small_input_returns_hex() {
        let data = vec![0xab, 0xcd, 0xef];
        let result = dump_tx_data(&data);
        assert_eq!(result, "abcdef");
    }

    #[test]
    fn dump_tx_data_at_inline_limit_returns_hex() {
        let data = vec![0x42; 512];
        let result = dump_tx_data(&data);
        // Should be simple hex, no line formatting
        assert_eq!(result, "42".repeat(512));
        assert!(!result.contains(':'));
    }

    #[test]
    fn dump_tx_data_over_inline_limit_formats_with_offsets() {
        let data = vec![0xaa; 513];
        let result = dump_tx_data(&data);
        // Should have offset formatting
        assert!(result.starts_with("0000: "));
        assert!(result.contains('\n'));
    }

    #[test]
    fn dump_tx_data_large_input_truncates() {
        // 64 lines * 128 bytes = 8192, so use more than that
        let data = vec![0xff; 10000];
        let result = dump_tx_data(&data);
        assert!(result.contains("truncated"));
        assert!(result.contains("10000 total"));
    }

    #[test]
    fn handle_build_res_returns_outcome_on_success() {
        let pool = testing_pool();
        let outcome: BuildOutcome<()> = BuildOutcome::Cancelled;
        let res: CatchUnwindBuildResult<()> = Ok(Ok(outcome));

        let result = handle_build_res(res, &pool, None);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), BuildOutcome::Cancelled));
    }

    #[test]
    fn handle_build_res_unprocessable_tx_without_invalid_tx_list() {
        let pool = testing_pool();
        let test_hash = TxHash::repeat_byte(0xAB);
        let unproc_err = UnprocessableTransactionError { tx_hash: test_hash };
        let payload_err = PayloadBuilderError::other(unproc_err);
        let res: CatchUnwindBuildResult<()> = Ok(Err(payload_err));

        let result = handle_build_res(res, &pool, None);

        assert!(result.is_err());
    }

    #[test]
    fn handle_build_res_unprocessable_tx_with_invalid_tx_list() {
        let pool = testing_pool();
        let invalid_tx_list = InvalidTxList::new(16);
        let test_hash = TxHash::repeat_byte(0xCD);
        let unproc_err = UnprocessableTransactionError { tx_hash: test_hash };
        let payload_err = PayloadBuilderError::other(unproc_err);
        let res: CatchUnwindBuildResult<()> = Ok(Err(payload_err));

        let result = handle_build_res(res, &pool, Some(&invalid_tx_list));

        assert!(result.is_err());
        assert!(invalid_tx_list.contains(&test_hash));
    }

    #[test]
    fn handle_build_res_other_error_without_invalid_tx_list() {
        let pool = testing_pool();
        let res: CatchUnwindBuildResult<()> = Ok(Err(PayloadBuilderError::MissingPayload));

        let result = handle_build_res(res, &pool, None);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PayloadBuilderError::MissingPayload
        ));
    }

    #[test]
    fn handle_build_res_other_error_with_invalid_tx_list() {
        let pool = testing_pool();
        let invalid_tx_list = InvalidTxList::new(16);
        let res: CatchUnwindBuildResult<()> = Ok(Err(PayloadBuilderError::MissingPayload));

        let result = handle_build_res(res, &pool, Some(&invalid_tx_list));

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PayloadBuilderError::MissingPayload
        ));
        // Ensure nothing was added to the invalid tx list
        assert_eq!(invalid_tx_list.len(), 0);
    }

    #[test]
    #[should_panic(expected = "test panic")]
    fn handle_build_res_panic_without_invalid_tx_list() {
        let pool = testing_pool();
        let panic_res = catch_unwind(AssertUnwindSafe(|| -> Result<BuildOutcome<()>, _> {
            panic!("test panic")
        }));

        let _ = handle_build_res(panic_res, &pool, None);
    }

    #[tokio::test]
    async fn handle_build_res_panic_with_invalid_tx_list() {
        use reth_transaction_pool::test_utils::MockTransaction;

        let pool = testing_pool();
        let tx = MockTransaction::eip1559();
        let tx_hash = *tx.hash();
        pool.add_transaction(reth_transaction_pool::TransactionOrigin::Local, tx)
            .await
            .expect("failed to add transaction");

        let invalid_tx_list = InvalidTxList::new(16);
        let panic_res = catch_unwind(AssertUnwindSafe(|| -> Result<BuildOutcome<()>, _> {
            panic!("test panic")
        }));

        // Catch the resumed panic so we can check the invalid tx list afterwards
        let panic_result = catch_unwind(AssertUnwindSafe(|| {
            handle_build_res(panic_res, &pool, Some(&invalid_tx_list))
        }));

        assert!(panic_result.is_err());
        assert!(invalid_tx_list.contains(&tx_hash));
    }

    // --- Tests for InvalidTxFilteringPayloadBuilder::build_empty_payload ---

    /// Configurable mock for the inner PayloadBuilder.
    #[derive(Clone)]
    struct MockInnerBuilder {
        behavior: MockBuildBehavior,
    }

    #[derive(Clone)]
    enum MockBuildBehavior {
        Succeed,
        FailUnprocessable(TxHash),
        Panic(&'static str),
    }

    impl MockInnerBuilder {
        fn build_payload() -> EthBuiltPayload {
            use reth_payload_builder::BlobSidecars;

            let block = reth_ethereum::Block {
                header: alloy_consensus::Header::default(),
                body: Default::default(),
            };
            let sealed = reth_ethereum::primitives::SealedBlock::from(block);
            EthBuiltPayload::new(Arc::new(sealed), U256::ZERO, None, None)
                .with_sidecars(BlobSidecars::Empty)
        }
    }

    impl RethPayloadBuilder for MockInnerBuilder {
        type Attributes = EthPayloadAttributes;
        type BuiltPayload = EthBuiltPayload;

        fn try_build(
            &self,
            _args: BuildArguments<Self::Attributes, Self::BuiltPayload>,
        ) -> Result<BuildOutcome<Self::BuiltPayload>, PayloadBuilderError> {
            unimplemented!("not used in build_empty_payload tests")
        }

        fn build_empty_payload(
            &self,
            _config: PayloadConfig<Self::Attributes, HeaderForPayload<Self::BuiltPayload>>,
        ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
            match &self.behavior {
                MockBuildBehavior::Succeed => Ok(Self::build_payload()),
                MockBuildBehavior::FailUnprocessable(hash) => {
                    Err(PayloadBuilderError::other(UnprocessableTransactionError {
                        tx_hash: *hash,
                    }))
                }
                MockBuildBehavior::Panic(msg) => panic!("{msg}"),
            }
        }
    }

    fn empty_payload_config(
    ) -> PayloadConfig<EthPayloadAttributes, HeaderForPayload<EthBuiltPayload>> {
        let attributes = EthPayloadAttributes {
            timestamp: 1,
            prev_randao: Default::default(),
            suggested_fee_recipient: Default::default(),
            withdrawals: Some(vec![]),
            parent_beacon_block_root: Some(Default::default()),
            slot_number: None,
        };
        PayloadConfig {
            parent_header: Arc::new(reth_ethereum::primitives::SealedHeader::default()),
            attributes,
            payload_id: reth_payload_builder::PayloadId::new([0u8; 8]),
        }
    }

    fn filtering_builder(
        behavior: MockBuildBehavior,
        invalid_tx_list: Option<InvalidTxList>,
    ) -> InvalidTxFilteringPayloadBuilder<
        MockInnerBuilder,
        reth_transaction_pool::test_utils::TestPool,
    > {
        InvalidTxFilteringPayloadBuilder {
            inner: MockInnerBuilder { behavior },
            pool: testing_pool(),
            invalid_tx_list,
        }
    }

    #[test]
    fn build_empty_payload_success() {
        let builder = filtering_builder(MockBuildBehavior::Succeed, None);
        let result = builder.build_empty_payload(empty_payload_config());
        assert!(result.is_ok());
    }

    #[test]
    fn build_empty_payload_error_purges_unprocessable_tx() {
        let invalid_tx_list = InvalidTxList::new(16);
        let test_hash = TxHash::repeat_byte(0xBB);
        let builder = filtering_builder(
            MockBuildBehavior::FailUnprocessable(test_hash),
            Some(invalid_tx_list.clone()),
        );

        let result = builder.build_empty_payload(empty_payload_config());
        assert!(result.is_err());
        assert!(invalid_tx_list.contains(&test_hash));
    }

    #[test]
    fn build_empty_payload_error_without_invalid_list() {
        let test_hash = TxHash::repeat_byte(0xBB);
        let builder = filtering_builder(MockBuildBehavior::FailUnprocessable(test_hash), None);

        let result = builder.build_empty_payload(empty_payload_config());
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "empty payload panic")]
    fn build_empty_payload_panic_resumes() {
        let builder = filtering_builder(
            MockBuildBehavior::Panic("empty payload panic"),
            Some(InvalidTxList::new(16)),
        );
        let _ = builder.build_empty_payload(empty_payload_config());
    }

    // --- Regression tests for `proposer_revenue` ---
    //
    // Arc redirects base fees to the beneficiary instead of burning them (see
    // `ArcEvmHandler::reward_beneficiary`), so proposer revenue must be
    // computed from `effective_gas_price`, not `effective_tip_per_gas`. These
    // tests pin that formula against a reth bump accidentally reintroducing
    // the upstream tip-only pattern.
    #[test]
    fn proposer_revenue_dynamic_fee_tx_includes_base_fee_plus_tip() {
        use alloy_consensus::TxEip1559;
        use alloy_primitives::{Address, TxKind};

        let base_fee: u64 = 100;
        let priority_fee: u128 = 50;
        let max_fee: u128 = 200;
        let gas_used: u64 = 21_000;

        let tx = TxEip1559 {
            chain_id: 1,
            nonce: 0,
            gas_limit: 100_000,
            max_fee_per_gas: max_fee,
            max_priority_fee_per_gas: priority_fee,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            access_list: Default::default(),
            input: Default::default(),
        };

        // effective_gas_price = min(max_fee, base_fee + priority_fee) = 150
        let expected = U256::from(150u128) * U256::from(gas_used);
        assert_eq!(proposer_revenue(&tx, gas_used, base_fee), expected);

        // Regression guard: full fee is strictly more than tip-only. If a reth
        // bump reintroduces `effective_tip_per_gas`, this assert fails.
        let tip_only = U256::from(priority_fee) * U256::from(gas_used);
        assert!(proposer_revenue(&tx, gas_used, base_fee) > tip_only);

        // Zero-tip trip-wire: proposer revenue is `base_fee * gas_used`, not 0.
        // `effective_tip_per_gas` would return 0 here.
        let zero_tip_tx = TxEip1559 {
            max_priority_fee_per_gas: 0,
            ..tx
        };
        assert_eq!(
            proposer_revenue(&zero_tip_tx, gas_used, base_fee),
            U256::from(base_fee) * U256::from(gas_used),
        );
    }

    #[test]
    fn proposer_revenue_dynamic_fee_tx_capped_at_max_fee() {
        use alloy_consensus::TxEip1559;
        use alloy_primitives::{Address, TxKind};

        let base_fee: u64 = 100;
        let priority_fee: u128 = 200; // base + tip would exceed max
        let max_fee: u128 = 250;
        let gas_used: u64 = 21_000;

        let tx = TxEip1559 {
            chain_id: 1,
            nonce: 0,
            gas_limit: 100_000,
            max_fee_per_gas: max_fee,
            max_priority_fee_per_gas: priority_fee,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            access_list: Default::default(),
            input: Default::default(),
        };

        // effective_gas_price = min(250, 100 + 200) = 250
        let expected = U256::from(max_fee) * U256::from(gas_used);
        assert_eq!(proposer_revenue(&tx, gas_used, base_fee), expected);
    }

    #[test]
    fn proposer_revenue_legacy_equals_gas_price_times_gas() {
        use alloy_consensus::TxLegacy;
        use alloy_primitives::{Address, TxKind};

        let gas_price: u128 = 75;
        let gas_used: u64 = 21_000;
        // Base fee is irrelevant for legacy txs.
        let base_fee: u64 = 100;

        let tx = TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price,
            gas_limit: 100_000,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Default::default(),
        };

        let expected = U256::from(gas_price) * U256::from(gas_used);
        assert_eq!(proposer_revenue(&tx, gas_used, base_fee), expected);
    }

    // A tx whose gas limit exceeds the gas remaining in the block must skip and
    // mark-invalid (matching the default builder), never abort the build. Before
    // reth 2.2 this error fell into the catch-all `Fatal` arm, which would have
    // killed payload production for one oversized pool tx.
    #[test]
    fn classify_gas_limit_exceeded_skips_with_executor_limits() {
        use alloy_consensus::{Signed, TxEip1559};
        use alloy_primitives::{Address, Signature, TxKind};

        let tx = TxEip1559 {
            chain_id: 1,
            nonce: 0,
            gas_limit: 30_000_000,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            access_list: Default::default(),
            input: Default::default(),
        };
        let signed: TransactionSigned =
            Signed::new_unhashed(tx, Signature::test_signature()).into();

        let err = BlockExecutionError::Validation(
            BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: 30_000_000,
                block_available_gas: 1_000_000,
            },
        );

        match classify_tx_outcome(Ok(Err(err)), TxHash::ZERO, &signed) {
            TxOutcome::SkipExceedsGasLimit {
                transaction_gas_limit,
                block_available_gas,
            } => {
                assert_eq!(transaction_gas_limit, 30_000_000);
                assert_eq!(block_available_gas, 1_000_000);
            }
            _ => panic!("expected SkipExceedsGasLimit"),
        }
    }

    // --- Tests for `arc_ethereum_payload` Arc-specific branches ---
    //
    // Covers the three Arc additions over reth's upstream builder: the
    // loop-time-budget early seal, the in-loop Osaka `OversizedData` rejection,
    // and the `is_better_payload` early abort. The harness pairs `EthEvmConfig`
    // with an empty `MockEthProvider` and a recording `BestTransactions` iterator
    // (`RecordingBestTxs`) to observe which branch ran via `mark_invalid` calls.
    //
    // The post-build `BlockTooLarge` branch is not covered: it needs a sealed
    // block over `MAX_RLP_BLOCK_SIZE` (8 MiB) while every Osaka-gated in-loop
    // estimate stayed under it, i.e. executing ~8 MiB of txs, with no production
    // seam to shrink the constant.
    mod arc_ethereum_payload_branches {
        use super::*;
        use alloy_primitives::{Address, Bytes, B256};
        use reth_basic_payload_builder::PayloadConfig;
        use reth_chainspec::{ChainSpec, ChainSpecBuilder};
        use reth_evm_ethereum::EthEvmConfig;
        use reth_payload_builder::PayloadId;
        use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
        use reth_transaction_pool::test_utils::{MockTransaction, MockTransactionFactory};
        use reth_transaction_pool::ValidPoolTransaction;
        use std::sync::Mutex;

        type TestPoolTx = MockTransaction;
        type TestValidTx = ValidPoolTransaction<TestPoolTx>;

        /// The kind of invalidation recorded by `RecordingBestTxs`.
        ///
        /// `InvalidPoolTransactionError` is neither `Clone` nor easily comparable,
        /// so we project each `mark_invalid` call to the discriminant the tests need
        /// to distinguish.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum MarkedKind {
            OversizedData,
            ExceedsGasLimit,
            Other,
        }

        impl MarkedKind {
            fn from_err(err: &InvalidPoolTransactionError) -> Self {
                match err {
                    InvalidPoolTransactionError::OversizedData { .. } => Self::OversizedData,
                    InvalidPoolTransactionError::ExceedsGasLimit(..) => Self::ExceedsGasLimit,
                    _ => Self::Other,
                }
            }
        }

        /// A `BestTransactions` iterator over a fixed list of validated pool
        /// transactions that records every `mark_invalid` call. This lets a test
        /// feed exact transactions into `arc_ethereum_payload` and then assert which
        /// ones the builder rejected and why.
        ///
        /// `BestTransactions: Send` requires the recorder state to be `Send`, hence
        /// `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>`.
        struct RecordingBestTxs {
            txs: std::vec::IntoIter<Arc<TestValidTx>>,
            /// Records `(tx_hash, marked_kind)` for each `mark_invalid` call.
            marked: Arc<Mutex<Vec<(TxHash, MarkedKind)>>>,
            /// Counts how many times `next()` was polled.
            polled: Arc<Mutex<usize>>,
        }

        impl Iterator for RecordingBestTxs {
            type Item = Arc<TestValidTx>;
            fn next(&mut self) -> Option<Self::Item> {
                let mut polled = self.polled.lock().expect("polled lock poisoned");
                *polled = polled.saturating_add(1);
                drop(polled);
                self.txs.next()
            }
        }

        impl BestTransactions for RecordingBestTxs {
            fn mark_invalid(&mut self, tx: &Self::Item, kind: &InvalidPoolTransactionError) {
                self.marked
                    .lock()
                    .expect("marked lock poisoned")
                    .push((*tx.hash(), MarkedKind::from_err(kind)));
            }
            fn no_updates(&mut self) {}
            fn set_skip_blobs(&mut self, _skip_blobs: bool) {}
        }

        /// Handles returned by the harness so the test can inspect builder behavior.
        struct Recorders {
            marked: Arc<Mutex<Vec<(TxHash, MarkedKind)>>>,
            polled: Arc<Mutex<usize>>,
        }

        /// Builds an `EthEvmConfig` + `MockEthProvider` over a chain spec, plus a
        /// `best_txs` closure yielding `txs`, and runs `arc_ethereum_payload`.
        ///
        /// `parent_gas_limit` flows into the next-block gas limit; set it high enough
        /// that the per-tx gas-capacity check (L592) passes for txs that should reach
        /// the size check.
        fn run_payload(
            chain_spec: Arc<ChainSpec>,
            parent_gas_limit: u64,
            loop_time_limit: Option<Duration>,
            best_payload: Option<EthBuiltPayload>,
            txs: Vec<Arc<TestValidTx>>,
        ) -> (
            Result<BuildOutcome<EthBuiltPayload>, PayloadBuilderError>,
            Recorders,
        ) {
            let evm_config = EthEvmConfig::ethereum(chain_spec.clone());
            let provider = MockEthProvider::default().with_chain_spec((*chain_spec).clone());

            // Parent header: timestamp 0 keeps Osaka/Prague active-at-timestamp(0)
            // true, and the chosen gas limit drives next-block capacity.
            let parent = alloy_consensus::Header {
                gas_limit: parent_gas_limit,
                number: 0,
                timestamp: 0,
                ..Default::default()
            };
            let parent_header = reth_ethereum::primitives::SealedHeader::seal_slow(parent);

            // Fee recipient must be non-zero/funded enough for sealing to succeed on
            // empty state; an empty `ExtendedAccount` is sufficient.
            let fee_recipient = Address::repeat_byte(0xAA);
            provider.add_account(fee_recipient, ExtendedAccount::new(0, U256::ZERO));

            let attributes = reth_ethereum_engine_primitives::EthPayloadAttributes {
                timestamp: 0,
                prev_randao: B256::ZERO,
                suggested_fee_recipient: fee_recipient,
                withdrawals: Some(vec![]),
                parent_beacon_block_root: Some(B256::ZERO),
                slot_number: None,
            };

            let config = PayloadConfig {
                parent_header: Arc::new(parent_header),
                attributes,
                payload_id: PayloadId::new([0u8; 8]),
            };

            let args = BuildArguments {
                cached_reads: Default::default(),
                execution_cache: None,
                trie_handle: None,
                config,
                cancel: Default::default(),
                best_payload,
            };

            let marked: Arc<Mutex<Vec<(TxHash, MarkedKind)>>> = Arc::new(Mutex::new(Vec::new()));
            let polled = Arc::new(Mutex::new(0usize));
            let marked_for_closure = marked.clone();
            let polled_for_closure = polled.clone();

            let best_txs = move |_attrs: BestTransactionsAttributes| -> BestTransactionsIter<
                reth_transaction_pool::test_utils::TestPool,
            > {
                Box::new(RecordingBestTxs {
                    txs: txs.into_iter(),
                    marked: marked_for_closure,
                    polled: polled_for_closure,
                })
            };

            let pool = reth_transaction_pool::test_utils::testing_pool();
            let outcome = arc_ethereum_payload(
                evm_config,
                provider,
                pool,
                EthereumBuilderConfig::new(),
                loop_time_limit,
                args,
                best_txs,
            );

            (outcome, Recorders { marked, polled })
        }

        fn prague_spec() -> Arc<ChainSpec> {
            Arc::new(ChainSpecBuilder::mainnet().prague_activated().build())
        }

        fn osaka_spec() -> Arc<ChainSpec> {
            Arc::new(ChainSpecBuilder::mainnet().osaka_activated().build())
        }

        /// Validates a `MockTransaction` into the `Arc<ValidPoolTransaction<..>>`
        /// shape the iterator yields.
        fn validate(tx: MockTransaction) -> Arc<TestValidTx> {
            MockTransactionFactory::default().validated_arc(tx)
        }

        /// AC #3: with `loop_time_limit = Some(Duration::ZERO)` and a pending tx
        /// available, the loop breaks on the first iteration before including any
        /// transaction. The sealed block has zero transactions, the iterator's first
        /// `next()` was polled, and no tx was marked invalid.
        #[test]
        fn loop_time_limit_seals_empty_block_early() {
            // Pre-Osaka spec keeps the path simple; branch 1 is not Osaka-gated.
            let tx = MockTransaction::eip1559().with_gas_limit(21_000);
            let txs = vec![validate(tx)];

            let (outcome, rec) =
                run_payload(prague_spec(), 30_000_000, Some(Duration::ZERO), None, txs);

            let outcome = outcome.expect("payload build should succeed");
            let payload = match outcome {
                BuildOutcome::Better { payload, .. } => payload,
                other => panic!("expected BuildOutcome::Better, got {other:?}"),
            };

            assert_eq!(
                payload.block().body().transactions.len(),
                0,
                "loop must break before including any tx"
            );
            assert_eq!(
                *rec.polled.lock().expect("polled lock poisoned"),
                1,
                "iterator should be polled exactly once before the early break"
            );
            assert!(
                rec.marked.lock().expect("marked lock poisoned").is_empty(),
                "no tx should be marked invalid on an early time-budget seal"
            );
        }

        /// AC #1: with Osaka active and a transaction whose consensus RLP length
        /// pushes the estimated block size above `MAX_RLP_BLOCK_SIZE`, the tx is
        /// marked invalid with `OversizedData` and excluded from the sealed block.
        #[test]
        fn osaka_oversized_tx_marked_invalid_and_excluded() {
            // ~8.5 MiB of calldata; well above MAX_RLP_BLOCK_SIZE (8 MiB). Small gas
            // limit so the L592 capacity check passes (the tx is never executed).
            let oversized_input = Bytes::from(vec![0u8; MAX_RLP_BLOCK_SIZE + 100_000]);
            let oversized = MockTransaction::eip1559()
                .with_gas_limit(21_000)
                .with_input(oversized_input);
            let oversized_tx = validate(oversized);
            let oversized_hash = *oversized_tx.hash();

            // Sanity: the consensus encoding really does exceed the limit, so the
            // estimate-with-overhead check trips.
            let consensus_len = oversized_tx.to_consensus().inner().length();
            assert!(
                consensus_len > MAX_RLP_BLOCK_SIZE,
                "test tx must encode larger than MAX_RLP_BLOCK_SIZE to hit the branch"
            );

            let (outcome, rec) =
                run_payload(osaka_spec(), 30_000_000, None, None, vec![oversized_tx]);

            let outcome = outcome.expect("payload build should succeed");
            let payload = match outcome {
                BuildOutcome::Better { payload, .. } => payload,
                other => panic!("expected BuildOutcome::Better, got {other:?}"),
            };

            assert_eq!(
                payload.block().body().transactions.len(),
                0,
                "oversized tx must be excluded from the sealed block"
            );

            let marked = rec.marked.lock().expect("marked lock poisoned");
            assert_eq!(marked.len(), 1, "exactly one tx should be marked invalid");
            assert_eq!(
                marked[0].0, oversized_hash,
                "the oversized tx is the one marked"
            );
            assert_eq!(
                marked[0].1,
                MarkedKind::OversizedData,
                "tx must be marked invalid with OversizedData, got {:?}",
                marked[0].1
            );
        }

        /// Regression guard: an undersized tx under Osaka is NOT marked
        /// `OversizedData` by the in-loop check (it proceeds to execution). This pins
        /// that the branch is size-gated, not unconditional.
        #[test]
        fn osaka_small_tx_not_marked_oversized() {
            let small = MockTransaction::eip1559()
                .with_gas_limit(21_000)
                .with_input(Bytes::from(vec![0u8; 32]));
            let small_tx = validate(small);

            let (outcome, rec) = run_payload(osaka_spec(), 30_000_000, None, None, vec![small_tx]);

            // The build itself must not error; we only assert the size branch did not
            // fire. (Execution against empty MockEthProvider state may skip the tx as
            // invalid for other reasons, which is fine — it must just not be
            // OversizedData.)
            assert!(outcome.is_ok(), "build should not error: {outcome:?}");
            assert!(
                rec.marked
                    .lock()
                    .expect("marked lock poisoned")
                    .iter()
                    .all(|(_, kind)| *kind != MarkedKind::OversizedData),
                "small tx must never be marked OversizedData"
            );
        }

        /// AC #4: when `best_payload` carries higher fees than the block being built
        /// (total_fees == 0 here, since no tx is executed), `is_better_payload`
        /// returns false and the builder returns `BuildOutcome::Aborted` without
        /// sealing.
        #[test]
        fn aborts_when_best_payload_has_higher_fees() {
            // A stand-in "best" payload with non-zero fees. The sealed block content
            // is irrelevant; only `fees()` is compared.
            let better_block = {
                let block = reth_ethereum::Block {
                    header: alloy_consensus::Header::default(),
                    body: Default::default(),
                };
                let sealed = reth_ethereum::primitives::SealedBlock::from(block);
                EthBuiltPayload::new(Arc::new(sealed), U256::from(1_000_000u64), None, None)
            };

            // No txs -> total_fees stays 0 -> not better than 1_000_000.
            let (outcome, _rec) =
                run_payload(prague_spec(), 30_000_000, None, Some(better_block), vec![]);

            let outcome = outcome.expect("payload build should succeed");
            match outcome {
                BuildOutcome::Aborted { fees, .. } => {
                    assert_eq!(
                        fees,
                        U256::ZERO,
                        "aborted fees should be the (zero) total_fees"
                    );
                }
                other => panic!("expected BuildOutcome::Aborted, got {other:?}"),
            }
        }
    }
}
