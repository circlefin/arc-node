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

use alloy_consensus::Transaction;
use alloy_primitives::U256;
use alloy_primitives::{hex, TxHash};
use alloy_rlp::Encodable;
use eyre::Result;
use reth_basic_payload_builder::{
    is_better_payload, BuildArguments, BuildOutcome, HeaderForPayload, MissingPayloadBehaviour,
    PayloadBuilder as RethPayloadBuilder, PayloadConfig,
};
use reth_chainspec::{ChainSpecProvider, EthChainSpec, EthereumHardforks};
use reth_consensus_common::validation::MAX_RLP_BLOCK_SIZE;
use reth_errors::{BlockExecutionError, BlockValidationError, ConsensusError};
use reth_ethereum_payload_builder::EthereumBuilderConfig;
use reth_ethereum_primitives::{EthPrimitives, TransactionSigned};
use reth_evm::{
    execute::{BlockBuilder, BlockBuilderOutcome},
    ConfigureEvm, Evm, NextBlockEnvAttributes,
};
use reth_node_api::{NodeTypes, PrimitivesTy};
use reth_node_builder::{
    components::PayloadBuilderBuilder, node::FullNodeTypes, BuilderContext, PayloadBuilderConfig,
};
use reth_payload_builder::{BlobSidecars, EthBuiltPayload, EthPayloadBuilderAttributes};
use reth_payload_primitives::{PayloadBuilderAttributes, PayloadBuilderError};
use reth_primitives_traits::transaction::error::InvalidTransactionError;
use reth_revm::{database::StateProviderDatabase, db::State};
use reth_storage_api::StateProviderFactory;
use reth_transaction_pool::{
    error::InvalidPoolTransactionError, BestTransactions, BestTransactionsAttributes,
    PoolTransaction, TransactionPool, ValidPoolTransaction,
};
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
        PayloadBuilderAttributes = EthPayloadBuilderAttributes,
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

        if let Some(invalid_tx_list) = invalid_tx_list {
            error!(tx_hash = %tx_hash, "adding unprocessable transaction to invalid tx list");
            add_pending_txs_to_invalid_list(pool, invalid_tx_list, vec![tx_hash]);
        } else {
            error!(tx_hash = %tx_hash, "invalid tx list is disabled, cannot add unprocessable transaction");
        }
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

    if let Some(invalid_tx_list) = invalid_tx_list {
        error!("payload builder panicked, adding all PENDING TXs to invalid tx list");
        add_pending_txs_to_invalid_list(pool, invalid_tx_list, pending_hashes);
    } else {
        error!("payload builder panicked, but invalid tx list disabled");
    }
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

/// Introduced to improve testability of `add_pending_txs_to_invalid_list`
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

fn add_pending_txs_to_invalid_list<P: PendingPool>(
    pool: &P,
    invalid_tx_list: &InvalidTxList,
    hashes: Vec<TxHash>,
) {
    let before = pool.pending_len();

    if hashes.is_empty() {
        error!("add_pending_txs_to_invalid_list: no pending transactions to add");
    }

    invalid_tx_list.insert_many(hashes.iter().copied());
    let removed = pool.remove_transactions_and_descendants(hashes);
    warn!(
        removed,
        pending_before = before,
        pending_after = pool.pending_len(),
        "added pending txs to invalid tx list"
    );
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
        let end = (offset + BYTES_PER_LINE).min(bytes.len());
        let slice = &bytes[offset..end];
        out.push_str(&format!("{:04x}: {}\n", offset, hex::encode(slice)));
        offset = end;
        lines += 1;
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
    type Attributes = EthPayloadBuilderAttributes;
    type BuiltPayload = EthBuiltPayload;

    fn try_build(
        &self,
        args: BuildArguments<EthPayloadBuilderAttributes, EthBuiltPayload>,
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
        config: PayloadConfig<Self::Attributes>,
    ) -> Result<Self::BuiltPayload, PayloadBuilderError> {
        let args = BuildArguments::new(Default::default(), config, Default::default(), None);

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

/// Constructs an transaction payload using the best transactions from the pool.
/// It follows the upstream Ethereum payload building logic with a Arc-specific deadline for the main loop.
///
///
/// Given build arguments including an Ethereum client, transaction pool,
/// and configuration, this function creates a transaction payload. Returns
/// a result indicating success with the payload or an error in case of failure.
#[inline]
pub fn arc_ethereum_payload<EvmConfig, Client, Pool, F>(
    evm_config: EvmConfig,
    client: Client,
    _pool: Pool,
    builder_config: EthereumBuilderConfig,
    loop_time_limit: Option<Duration>,
    args: BuildArguments<EthPayloadBuilderAttributes, EthBuiltPayload>,
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
        config,
        cancel,
        best_payload,
    } = args;
    let PayloadConfig {
        parent_header,
        attributes,
    } = config;

    let total_start = Instant::now();

    let stage_start = Instant::now();
    let state_provider = client.state_by_block_hash(parent_header.hash())?;
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
                timestamp: attributes.timestamp(),
                suggested_fee_recipient: attributes.suggested_fee_recipient(),
                prev_randao: attributes.prev_randao(),
                gas_limit: builder_config.gas_limit(parent_header.gas_limit),
                parent_beacon_block_root: attributes.parent_beacon_block_root(),
                withdrawals: Some(attributes.withdrawals().clone()),
                extra_data: builder_config.extra_data,
            },
        )
        .map_err(PayloadBuilderError::other)?;

    let chain_spec = client.chain_spec();

    info!(target: "payload_builder", id=%attributes.id, parent_header = ?parent_header.hash(), parent_number = parent_header.number, "(arc) building new payload");
    let mut cumulative_gas_used = 0;
    let block_gas_limit: u64 = builder.evm_mut().block().gas_limit();
    let base_fee = builder.evm_mut().block().basefee();

    let mut best_txs = best_txs(BestTransactionsAttributes::new(
        base_fee,
        None, // Explicitly disable blob transactions by not providing a blob gas price.
    ));
    let mut total_fees = U256::ZERO;

    let stage_start = Instant::now();
    builder.apply_pre_execution_changes().map_err(|err| {
        warn!(target: "payload_builder", %err, "(arc) failed to apply pre-execution changes");
        PayloadBuilderError::Internal(err.into())
    })?;
    PayloadBuildMetrics::record_stage_pre_execution(stage_start.elapsed());

    let mut block_transactions_rlp_length = 0;
    let is_osaka = chain_spec.is_osaka_active_at_timestamp(attributes.timestamp);

    let withdrawals_rlp_length = attributes.withdrawals().length();

    let loop_started = Instant::now();

    while let Some(pool_tx) = best_txs.next() {
        // Break early if loop time budget exhausted
        if let Some(limit) = loop_time_limit {
            if loop_started.elapsed() >= limit {
                warn!(
                    elapsed_ms = loop_started.elapsed().as_millis() as u64,
                    "(arc) loop time budget reached; sealing early"
                );
                break;
            }
        }

        // ensure we still have capacity for this transaction
        if cumulative_gas_used + pool_tx.gas_limit() > block_gas_limit {
            // we can't fit this transaction into the block, so we need to mark it as invalid
            // which also removes all dependent transaction from the iterator before we can
            // continue
            best_txs.mark_invalid(
                &pool_tx,
                &InvalidPoolTransactionError::ExceedsGasLimit(pool_tx.gas_limit(), block_gas_limit),
            );
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

        let estimated_block_size_with_tx =
            block_transactions_rlp_length + tx_rlp_len + withdrawals_rlp_length + 1024; // 1Kb of overhead for the block header

        if is_osaka && estimated_block_size_with_tx > MAX_RLP_BLOCK_SIZE {
            best_txs.mark_invalid(
                &pool_tx,
                &InvalidPoolTransactionError::OversizedData {
                    size: estimated_block_size_with_tx,
                    limit: MAX_RLP_BLOCK_SIZE,
                },
            );
            continue;
        }

        let gas_used = match catch_unwind(AssertUnwindSafe(|| {
            builder.execute_transaction(tx.clone())
        })) {
            Ok(Ok(gas_used)) => gas_used,
            Ok(Err(BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                error,
                ..
            }))) => {
                if error.is_nonce_too_low() {
                    // if the nonce is too low, we can skip this transaction
                    trace!(target: "payload_builder", %error, ?tx, "(arc) skipping nonce too low transaction");
                } else {
                    // if the transaction is invalid, we can skip it and all of its
                    // descendants
                    trace!(target: "payload_builder", %error, ?tx, "(arc) skipping invalid transaction and its descendants");
                    best_txs.mark_invalid(
                        &pool_tx,
                        &InvalidPoolTransactionError::Consensus(
                            InvalidTransactionError::TxTypeNotSupported,
                        ),
                    );
                }
                continue;
            }
            // this is an error that we should treat as fatal for this attempt
            Ok(Err(err)) => return Err(PayloadBuilderError::evm(err)),
            // a single transaction caused a panic — wrap it so handle_build_res
            // can identify the offending tx and purge it from the mempool
            Err(_panic_payload) => {
                let tx_hash = *pool_tx.hash();
                return Err(PayloadBuilderError::other(UnprocessableTransactionError {
                    tx_hash,
                }));
            }
        };

        block_transactions_rlp_length += tx_rlp_len;

        // update and add to total fees
        let miner_fee = tx
            .effective_tip_per_gas(base_fee)
            .expect("fee is always valid; execution succeeded");
        total_fees += U256::from(miner_fee) * U256::from(gas_used);
        cumulative_gas_used += gas_used;
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
    let BlockBuilderOutcome {
        execution_result,
        block,
        ..
    } = builder.finish(state_provider.as_ref())?;
    PayloadBuildMetrics::record_stage_post_execution(builder_finish.elapsed());

    let stage_start = Instant::now();
    let requests = chain_spec
        .is_prague_active_at_timestamp(attributes.timestamp)
        .then_some(execution_result.requests);

    let sealed_block = Arc::new(block.sealed_block().clone());
    debug!(target: "payload_builder", id=%attributes.id, sealed_block_header = ?sealed_block.sealed_header(), "(arc) sealed built block");

    if is_osaka && sealed_block.rlp_length() > MAX_RLP_BLOCK_SIZE {
        PayloadBuildMetrics::record_stage_assembly_and_sealing(stage_start.elapsed());
        PayloadBuildMetrics::record_total_duration(total_start);
        return Err(PayloadBuilderError::other(ConsensusError::BlockTooLarge {
            rlp_length: sealed_block.rlp_length(),
            max_rlp_length: MAX_RLP_BLOCK_SIZE,
        }));
    }

    let payload = EthBuiltPayload::new(attributes.id, sealed_block, total_fees, requests)
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
    fn add_pending_txs_to_invalid_list_inserts_all() {
        let hashes: Vec<TxHash> = (0..3).map(TxHash::repeat_byte).collect();
        let pool = MockPendingPool::new(hashes.clone());
        let invalid_tx_list = InvalidTxList::new(16);
        add_pending_txs_to_invalid_list(&pool, &invalid_tx_list, hashes.clone());
        assert_eq!(hashes.len(), invalid_tx_list.len());
        for h in hashes {
            assert!(invalid_tx_list.contains(&h));
        }
    }

    #[test]
    fn add_pending_txs_to_invalid_list_empty_no_insert() {
        let pool = MockPendingPool::new(vec![]);
        let invalid_tx_list = InvalidTxList::new(16);
        add_pending_txs_to_invalid_list(&pool, &invalid_tx_list, vec![]);
        assert_eq!(0, invalid_tx_list.len());
    }

    #[test]
    fn add_pending_txs_to_invalid_list_removes_from_pool() {
        let hashes: Vec<TxHash> = (0..5).map(TxHash::repeat_byte).collect();
        let pool = MockPendingPool::new(hashes.clone());
        let invalid_tx_list = InvalidTxList::new(64);
        add_pending_txs_to_invalid_list(&pool, &invalid_tx_list, hashes.clone());
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
            use reth_payload_builder::{BlobSidecars, PayloadId};

            let block = reth_ethereum::Block {
                header: alloy_consensus::Header::default(),
                body: Default::default(),
            };
            let sealed = reth_ethereum::primitives::SealedBlock::from(block);
            EthBuiltPayload::new(PayloadId::new([0u8; 8]), Arc::new(sealed), U256::ZERO, None)
                .with_sidecars(BlobSidecars::Empty)
        }
    }

    impl RethPayloadBuilder for MockInnerBuilder {
        type Attributes = EthPayloadBuilderAttributes;
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

    fn empty_payload_config() -> PayloadConfig<EthPayloadBuilderAttributes> {
        let attributes = EthPayloadBuilderAttributes::new(
            Default::default(),
            reth_ethereum_engine_primitives::EthPayloadAttributes {
                timestamp: 1,
                prev_randao: Default::default(),
                suggested_fee_recipient: Default::default(),
                withdrawals: Some(vec![]),
                parent_beacon_block_root: Some(Default::default()),
            },
        );
        PayloadConfig {
            parent_header: Arc::new(reth_ethereum::primitives::SealedHeader::default()),
            attributes,
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
}
