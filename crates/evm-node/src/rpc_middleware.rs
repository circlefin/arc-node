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

use alloy_consensus::TxEnvelope;
use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_network::eip2718::Decodable2718;
use alloy_primitives::Bytes;
use alloy_rpc_client::RpcClient;
use alloy_rpc_types_eth::{pubsub::SubscriptionKind, BlockOverrides, Bundle, TransactionRequest};
use alloy_rpc_types_trace::geth::GethDebugTracingCallOptions;
use alloy_transport::{RpcError, TransportErrorKind};
use jsonrpsee::{
    core::middleware::{layer::Either, Batch, BatchEntry, Notification, RpcServiceT},
    types::{
        error::ErrorCode, ErrorObject, ErrorObjectOwned, Id, Params, Request, ResponsePayload,
    },
    BatchResponseBuilder, MethodResponse,
};
use reth_rpc_server_types::constants::gas_oracle::RPC_DEFAULT_GAS_CAP;
use serde::{
    de::{DeserializeOwned, IgnoredAny},
    Deserialize,
};
use serde_json::value::RawValue;
use std::{
    collections::BTreeMap,
    future::Future,
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tower::Layer;

const ETH_SUBSCRIBE_METHOD: &str = "eth_subscribe";
const ETH_NEW_PENDING_TX_FILTER_METHOD: &str = "eth_newPendingTransactionFilter";
const ETH_GET_BLOCK_BY_NUMBER_METHOD: &str = "eth_getBlockByNumber";
const BLOCK_NUMBER_OBJECT_KEY: &str = "number";
const BLOCK_ID_OBJECT_KEY_SNAKE: &str = "block_id";
const BLOCK_ID_OBJECT_KEY_CAMEL: &str = "blockId";
const ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD: &str = "eth_getTransactionBySenderAndNonce";
const ETH_GET_BLOCK_RECEIPTS_METHOD: &str = "eth_getBlockReceipts";
const ETH_GET_BLOCK_TX_COUNT_BY_NUMBER_METHOD: &str = "eth_getBlockTransactionCountByNumber";
const ETH_GET_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD: &str = "eth_getTransactionByBlockNumberAndIndex";
const ETH_GET_RAW_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD: &str =
    "eth_getRawTransactionByBlockNumberAndIndex";
const ETH_GET_UNCLE_COUNT_BY_BLOCK_NUMBER_METHOD: &str = "eth_getUncleCountByBlockNumber";
const ETH_GET_HEADER_BY_NUMBER_METHOD: &str = "eth_getHeaderByNumber";
// jsonrpsee proc-macro field name for eth_getHeaderByNumber's BlockNumberOrTag param.
// Reth's trait declares `hash: BlockNumberOrTag` (copy-paste from getHeaderByHash) —
// see reth-rpc-eth-api/src/core.rs. If that arg is ever renamed, update this key and
// the object-form test cases; the array/positional form is unaffected.
const BLOCK_HEADER_NUMBER_OBJECT_KEY: &str = "hash";
// jsonrpsee proc-macro field name for eth_subscribe's SubscriptionKind param.
// Reth's trait declares `kind: SubscriptionKind` — see reth-rpc-eth-api/src/pubsub.rs.
const SUBSCRIPTION_KIND_OBJECT_KEY: &str = "kind";
// jsonrpsee proc-macro field name for the raw-tx submission payload param.
// Reth's trait declares `bytes: Bytes` — see reth-rpc-eth-api/src/core.rs.
const RAW_TX_BYTES_OBJECT_KEY: &str = "bytes";
const ETH_SEND_RAW_TRANSACTION_METHOD: &str = "eth_sendRawTransaction";
const ETH_SEND_RAW_TRANSACTION_SYNC_METHOD: &str = "eth_sendRawTransactionSync";
const ETH_CALL_MANY_METHOD: &str = "eth_callMany";
const ETH_CALL_METHOD: &str = "eth_call";
const TRACE_CALL_METHOD: &str = "trace_call";
const DEBUG_TRACE_CALL_METHOD: &str = "debug_traceCall";
const DEBUG_TRACE_CALL_MANY_METHOD: &str = "debug_traceCallMany";
const ETH_CREATE_ACCESS_LIST_METHOD: &str = "eth_createAccessList";
const PENDING_TX_SUBSCRIPTION_ERROR_CODE: i32 = -32001;
const BATCH_TOO_LARGE_ERROR_CODE: i32 = -32600;
// JSON-RPC "Invalid params": the request asks for more work than the cap allows,
// or its params cannot be read. A permanent client error, so a well-behaved
// caller will not retry and amplify the load.
const CALL_MANY_REJECTED_ERROR_CODE: i32 = -32602;
const CALL_MANY_REJECTED_METRIC: &str = "arc_rpc_call_many_rejected_total";
const RPC_GAS_CAP_REJECTED_ERROR_CODE: i32 = -32602;
const RPC_GAS_CAP_REJECTED_METRIC: &str = "arc_rpc_gas_cap_rejected_total";
const UNPROTECTED_TX_ERROR_CODE: i32 = -32000;
const UNPROTECTED_TX_ERROR_MSG: &str =
    "only replay-protected (EIP-155) transactions allowed over RPC";
const RELAY_UNAVAILABLE_ERROR_CODE: i32 = -32010;
const RELAY_UNAVAILABLE_ERROR_MSG: &str = "all transaction relay upstreams are unreachable";
const RELAY_REJECTED_ERROR_CODE: i32 = -32011;
// Host-free: a public RPC client must not learn upstream host, path, or query
// details from the message. The error category is logged instead.
const RELAY_REJECTED_ERROR_MSG: &str = "transaction relay upstream rejected the request";

/// Default maximum number of entries permitted in a JSON-RPC batch request.
pub const ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT: NonZeroUsize =
    NonZeroUsize::new(100).expect("literal is non-zero");

/// Default maximum number of `eth_callMany` calls permitted in one JSON-RPC request.
pub const ARC_RPC_MAX_CALL_MANY_CALLS_DEFAULT: NonZeroUsize =
    NonZeroUsize::new(10).expect("literal is non-zero");

/// Default maximum number of accounts a single `eth_callMany` may override.
pub const ARC_RPC_MAX_CALL_MANY_STATE_OVERRIDES_DEFAULT: NonZeroUsize =
    NonZeroUsize::new(100).expect("literal is non-zero");

/// Limits on `eth_callMany` work.
///
/// Both are [`NonZeroUsize`], so a zero that would block the method outright
/// cannot be expressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallManyLimits {
    /// Calls permitted in one JSON-RPC request, summed across every bundle and
    /// across every `eth_callMany` entry of a batch.
    pub max_calls: NonZeroUsize,
    /// Accounts a single `eth_callMany` may override. Unlike the call budget
    /// this is per entry, but an entry must spend at least one call to apply
    /// any override, so the call budget still bounds a batch's override work.
    pub max_state_overrides: NonZeroUsize,
}

impl Default for CallManyLimits {
    fn default() -> Self {
        Self {
            max_calls: ARC_RPC_MAX_CALL_MANY_CALLS_DEFAULT,
            max_state_overrides: ARC_RPC_MAX_CALL_MANY_STATE_OVERRIDES_DEFAULT,
        }
    }
}

/// Default transaction relay timeout.
pub const DEFAULT_TX_RELAY_TIMEOUT: Duration = Duration::from_secs(10);

/// Config for the Arc-specific RPC middleware stack.
#[derive(Clone, Debug)]
pub struct ArcRpcLayer {
    /// When true (default), blocks all RPC methods that can expose pending-block
    /// state: `eth_subscribe("newPendingTransactions")`, `eth_newPendingTransactionFilter`,
    /// and the block-content methods
    /// `eth_getBlockByNumber`, `eth_getBlockReceipts`,
    /// `eth_getBlockTransactionCountByNumber`, `eth_getTransactionByBlockNumberAndIndex`,
    /// `eth_getRawTransactionByBlockNumberAndIndex`, `eth_getUncleCountByBlockNumber`,
    /// and `eth_getHeaderByNumber` when called with the `"pending"` tag.
    /// `eth_getTransactionBySenderAndNonce` is forwarded, then pending
    /// answers are removed so committed sender+nonce lookups still work.
    /// When false, the filter is bypassed and these are allowed.
    /// CLI users opt out of the default via `--arc.expose-pending-txs`.
    pub filter_pending_txs: bool,
    /// When true, raw transaction submission RPCs accept pre-EIP-155
    /// (replay-unprotected) transactions. Defaults to false, matching Geth.
    /// P2P-received transactions and transactions included in blocks by other
    /// validators are not affected.
    pub allow_unprotected_txs: bool,
    /// Mirrors `--rpc.max-response-size` from the server config (in bytes).
    pub max_response_body_size: usize,
    /// Maximum number of entries permitted in a JSON-RPC batch request.
    pub max_batch_entries: NonZeroUsize,
    /// Limits on `eth_callMany` work.
    pub call_many_limits: CallManyLimits,
    /// Mirrors `--rpc.gascap`; zero disables the block gas-limit override check,
    /// as it disables Reth's cap.
    pub rpc_gas_cap: u64,
    /// Transaction relay upstreams, or `None` when relaying is disabled.
    pub tx_relays: Option<TxRelays>,
}

impl Default for ArcRpcLayer {
    fn default() -> Self {
        Self {
            filter_pending_txs: true,
            allow_unprotected_txs: false,
            max_response_body_size: usize::MAX,
            max_batch_entries: ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT,
            call_many_limits: CallManyLimits::default(),
            rpc_gas_cap: RPC_DEFAULT_GAS_CAP,
            tx_relays: None,
        }
    }
}

impl ArcRpcLayer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        filter_pending_txs: bool,
        allow_unprotected_txs: bool,
        max_response_body_size: usize,
        max_batch_entries: NonZeroUsize,
        call_many_limits: CallManyLimits,
        rpc_gas_cap: u64,
        tx_relays: Vec<String>,
        tx_relay_timeout: Duration,
    ) -> Self {
        Self {
            filter_pending_txs,
            allow_unprotected_txs,
            max_response_body_size,
            max_batch_entries,
            call_many_limits,
            rpc_gas_cap,
            tx_relays: TxRelays::new(&tx_relays, max_response_body_size, tx_relay_timeout),
        }
    }
}

/// Relaying is optional: the relay middleware when enabled, the inner service
/// otherwise.
type MaybeRelay<S> = Either<TxRelayMiddleware<S>, S>;

/// The pending-transaction filter is optional in the same way.
type MaybePendingFilter<S> =
    Either<NoPendingTransactionsRpcMiddleware<MaybeRelay<S>>, MaybeRelay<S>>;

// S: Clone is required because the middleware clones the inner service in its
// `call` implementation.
impl<S> Layer<S> for ArcRpcLayer
where
    S: Clone,
{
    type Service = BatchSizeLimitMiddleware<
        CallManyLimitMiddleware<
            RpcGasCapMiddleware<RejectUnprotectedTxsMiddleware<MaybePendingFilter<S>>>,
        >,
    >;

    fn layer(&self, inner: S) -> Self::Service {
        let relayed = match &self.tx_relays {
            Some(relays) => Either::Left(TxRelayMiddleware {
                service: inner,
                relays: relays.clone(),
            }),
            None => Either::Right(inner),
        };
        let pending_layer = if self.filter_pending_txs {
            Either::Left(NoPendingTransactionsRpcMiddleware {
                service: relayed,
                max_response_body_size: self.max_response_body_size,
            })
        } else {
            Either::Right(relayed)
        };
        let inner_layer = RejectUnprotectedTxsMiddleware {
            service: pending_layer,
            allow_unprotected_txs: self.allow_unprotected_txs,
            max_response_body_size: self.max_response_body_size,
        };
        // Inside the `eth_callMany` cap, which must see a batch whole: this
        // guard forwards a batch entry by entry.
        let gas_cap_layer = RpcGasCapMiddleware {
            service: inner_layer,
            rpc_gas_cap: self.rpc_gas_cap,
            max_response_body_size: self.max_response_body_size,
        };
        let service = CallManyLimitMiddleware {
            service: gas_cap_layer,
            limits: self.call_many_limits,
        };
        BatchSizeLimitMiddleware {
            service,
            max_entries: self.max_batch_entries,
        }
    }
}

/// RPC middleware that rejects JSON-RPC batches above `max_entries` before any
/// per-entry handler runs.
#[derive(Clone, Debug)]
pub struct BatchSizeLimitMiddleware<S> {
    service: S,
    max_entries: NonZeroUsize,
}

impl<S> BatchSizeLimitMiddleware<S> {
    pub fn new(service: S, max_entries: NonZeroUsize) -> Self {
        Self {
            service,
            max_entries,
        }
    }
}

impl<S> RpcServiceT for BatchSizeLimitMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        self.service.call(req)
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let service = self.service.clone();
        let max_entries = self.max_entries;
        async move {
            if req.len() > max_entries.get() {
                let err = ErrorObjectOwned::owned::<()>(
                    BATCH_TOO_LARGE_ERROR_CODE,
                    format!("batch size {} exceeds limit of {}", req.len(), max_entries),
                    None,
                );
                return MethodResponse::error(Id::Null, err);
            }
            service.batch(req).await
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.service.notification(n)
    }
}

/// RPC middleware that bounds the work one JSON-RPC request may ask of
/// `eth_callMany`.
///
/// Reth runs a request's bundles sequentially inside one blocking task holding
/// one database read transaction and one blocking-IO permit. A long call list
/// buys the caller no parallelism: it only extends how long that request
/// occupies its connection, its read transaction, and that permit. The permit
/// is the same one `eth_call` takes, and one permit covers every call in the
/// request, so the call cap is what limits how long the permit stays held.
/// Two limits apply, both checked ahead of the handler so a rejected request
/// costs no execution:
///
/// - calls, summed across every bundle and across every `eth_callMany` entry
///   of a JSON-RPC batch, so batching cannot multiply the budget by the entry
///   count;
/// - overridden accounts, because applying a state override reads each account
///   from state, the same cold read that makes the calls expensive on an
///   archive node.
///
/// A batch that exceeds the budget is rejected whole, like an oversized batch
/// one layer out. Rejecting individual entries instead would make the verdict
/// depend on entry order and would still pay for the entries that ran first.
#[derive(Clone, Debug)]
pub struct CallManyLimitMiddleware<S> {
    service: S,
    limits: CallManyLimits,
}

impl<S> CallManyLimitMiddleware<S> {
    pub fn new(service: S, limits: CallManyLimits) -> Self {
        Self { service, limits }
    }
}

impl<S> RpcServiceT for CallManyLimitMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let service = self.service.clone();
        let limits = self.limits;
        async move {
            if let Err(err) = check_call_many(std::iter::once(&req), limits) {
                return MethodResponse::error(req.id(), err);
            }
            service.call(req).await
        }
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let service = self.service.clone();
        let limits = self.limits;
        async move {
            // Notifications and malformed entries are skipped: jsonrpsee
            // answers a notification without dispatching it to the handler, so
            // one can execute nothing, and a malformed entry never reaches a
            // method at all.
            let calls = req.iter().filter_map(|entry| match entry {
                Ok(BatchEntry::Call(request)) => Some(request),
                _ => None,
            });
            if let Err(err) = check_call_many(calls, limits) {
                return MethodResponse::error(Id::Null, err);
            }
            service.batch(req).await
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.service.notification(n)
    }
}

/// An `eth_callMany` bundle list, parsed with alloy's own [`Bundle`] so the
/// cap's view of the wire format cannot drift from the handler's.
///
/// Serde reads a struct from a JSON array positionally as well as from an
/// object, so a bundle can arrive as `[transactions, blockOverride]`; a
/// hand-written mirror with the wrong field count would fail to parse that
/// form and reject requests reth would have run. `IgnoredAny` for the
/// transaction type keeps the cap permissive about call bodies — it only
/// counts, the handler validates — while every other field matches the
/// handler's own type by construction.
type CallManyBundles = Vec<Bundle<IgnoredAny>>;

/// Why the cap turned an `eth_callMany` request away. Kept as a closed set so
/// the metric's `reason` label cannot grow a new series by typo.
#[derive(Clone, Copy, Debug)]
enum CallManyRejection {
    CallCount,
    StateOverrides,
    UnreadableParams,
}

impl CallManyRejection {
    fn as_str(self) -> &'static str {
        match self {
            Self::CallCount => "call_count",
            Self::StateOverrides => "state_overrides",
            Self::UnreadableParams => "unreadable_params",
        }
    }
}

/// What one `eth_callMany` request costs, in the units the cap bounds.
struct CallManyCost {
    calls: usize,
    state_overrides: usize,
}

/// Validates every `eth_callMany` among `requests` against `limits`.
///
/// One JSON-RPC request gets one call budget, and a batch is one request whose
/// entries reth runs one after another, so the budget covers the whole batch.
/// Requests for other methods are free.
///
/// Params that cannot be read are rejected rather than forwarded. The shape
/// this guard parses is deliberately more permissive than the handler's, so a
/// payload it cannot read is one the handler would reject anyway, and
/// forwarding would let any future shape drift turn into a silent bypass.
fn check_call_many<'a>(
    requests: impl Iterator<Item = &'a Request<'a>>,
    limits: CallManyLimits,
) -> Result<(), ErrorObjectOwned> {
    let max_calls = limits.max_calls.get();
    let max_overrides = limits.max_state_overrides.get();
    let mut budget = max_calls;
    for req in requests {
        if req.method_name() != ETH_CALL_MANY_METHOD {
            continue;
        }
        let Some(cost) = measure_call_many(req.params()) else {
            return Err(reject_call_many(
                CallManyRejection::UnreadableParams,
                "eth_callMany parameters could not be read".to_string(),
            ));
        };
        if cost.state_overrides > max_overrides {
            return Err(reject_call_many(
                CallManyRejection::StateOverrides,
                format!(
                    "eth_callMany overrides {} accounts, exceeding the limit of {max_overrides} \
                     (--arc.rpc.max-call-many-state-overrides)",
                    cost.state_overrides
                ),
            ));
        }
        let Some(remaining) = budget.checked_sub(cost.calls) else {
            return Err(reject_call_many(
                CallManyRejection::CallCount,
                format!(
                    "eth_callMany call count {} exceeds the {budget} calls remaining of the \
                     {max_calls} allowed per request (--arc.rpc.max-call-many-calls)",
                    cost.calls
                ),
            ));
        };
        budget = remaining;
    }
    Ok(())
}

/// Builds the rejection and records why, so an operator can tell a client
/// sending oversized requests from one sending unreadable ones.
fn reject_call_many(reason: CallManyRejection, message: String) -> ErrorObjectOwned {
    metrics::counter!(CALL_MANY_REJECTED_METRIC, "reason" => reason.as_str()).increment(1);
    ErrorObjectOwned::owned::<()>(CALL_MANY_REJECTED_ERROR_CODE, message, None)
}

/// The `eth_callMany` arguments the cap measures.
///
/// Reth's trait declares `bundles: Vec<Bundle<TxReq>>` first and
/// `state_override: Option<StateOverride>` third — see
/// reth-rpc-eth-api/src/core.rs. jsonrpsee binds named params through a
/// derived struct that accepts each argument under its snake_case and
/// lowerCamelCase spelling, so this one declares the same alias and reads
/// them the same way. Setting both spellings fails as a duplicate field here
/// as it does in reth, rather than guessing which one was meant and possibly
/// undercounting.
#[derive(Deserialize)]
struct CallManyArgs {
    bundles: CallManyBundles,
    #[serde(alias = "stateOverride")]
    state_override: Option<StateOverrideKeys>,
}

/// Reads the bundle list and state-override map of an `eth_callMany` request.
///
/// Mirrors how jsonrpsee binds the handler's arguments: positional params are
/// read in declaration order and trailing extras are ignored; named params
/// are read through [`CallManyArgs`]. Returns `None` when the payload cannot
/// be read as an `eth_callMany` call at all.
fn measure_call_many(params: Params<'_>) -> Option<CallManyCost> {
    let CallManyArgs {
        bundles,
        state_override,
    } = if params.is_object() {
        params.parse().ok()?
    } else {
        let mut seq = params.sequence();
        let bundles = seq.next().ok()?;
        // state_context is not measured, but must be consumed to reach the overrides.
        seq.optional_next::<IgnoredAny>().ok()?;
        CallManyArgs {
            bundles,
            state_override: seq.optional_next().ok()?,
        }
    };
    // Saturating so an absurd count cannot wrap to a small one; it cannot
    // actually overflow, because the RPC request-size limit bounds the element
    // count far below usize::MAX.
    let calls = bundles.iter().fold(0usize, |total, bundle| {
        total.saturating_add(bundle.transactions.len())
    });
    Some(CallManyCost {
        // Charge at least one even when every bundle is empty. Reth still
        // resolves the target block and opens a read transaction, and when
        // `state_context.transactionIndex` names a position inside the block it
        // replays that block's transactions before looking at any bundle. None
        // of that is proportional to the call count, so a zero charge would let
        // a batch carry a hundred of them for free.
        calls: calls.max(1),
        state_overrides: state_override.map_or(0, |accounts| accounts.len()),
    })
}

/// A `StateOverride` map reduced to its keys, so the cap can count overridden
/// accounts without deserializing each override body.
type StateOverrideKeys = BTreeMap<String, IgnoredAny>;

/// RPC middleware that rejects simulation requests asking for more gas than
/// `--rpc.gascap`, where Reth does not apply the cap itself.
///
/// Reth gives a simulated call that omits `gas` and sets a non-zero fee the
/// smaller of the caller's allowance and the block gas limit, in place of the
/// RPC gas cap. A balance override lifts the allowance, which leaves the block
/// gas limit as the only bound, so an override of it must not exceed the cap.
/// `eth_createAccessList` runs the request's own `gas` uncapped, so that must
/// not exceed it either. `eth_estimateGas` and `trace_callMany` take no block
/// override, so the chain's block gas limit bounds them, and `eth_simulateV1`
/// checks its own overrides.
#[derive(Clone, Debug)]
pub struct RpcGasCapMiddleware<S> {
    service: S,
    rpc_gas_cap: u64,
    max_response_body_size: usize,
}

impl<S> RpcGasCapMiddleware<S> {
    pub fn new(service: S, rpc_gas_cap: u64) -> Self {
        Self {
            service,
            rpc_gas_cap,
            max_response_body_size: usize::MAX,
        }
    }
}

impl<S> RpcServiceT for RpcGasCapMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let service = self.service.clone();
        let rpc_gas_cap = self.rpc_gas_cap;
        async move {
            if let Err(err) = check_rpc_gas_cap(&req, rpc_gas_cap) {
                return MethodResponse::error(req.id(), err);
            }
            service.call(req).await
        }
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let service = self.service.clone();
        let rpc_gas_cap = self.rpc_gas_cap;
        let max_response_body_size = self.max_response_body_size;
        async move {
            forward_batch_with_guard(&service, req, max_response_body_size, |request| {
                check_rpc_gas_cap(request, rpc_gas_cap)
            })
            .await
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.service.notification(n)
    }
}

/// Rejects a request whose block gas-limit override, or `eth_createAccessList`
/// gas, exceeds `rpc_gas_cap`.
///
/// Params the guard cannot read are forwarded: it reads them with the
/// handler's own types, so the handler rejects the same payload with its own
/// error before executing anything.
fn check_rpc_gas_cap(req: &Request<'_>, rpc_gas_cap: u64) -> Result<(), ErrorObjectOwned> {
    if rpc_gas_cap == 0 {
        return Ok(());
    }
    let Some(slots) = gas_limit_slots(req.method_name()) else {
        return Ok(());
    };
    let Some(gas_limit) =
        read_gas_limit_args(req.params(), &slots).and_then(|args| args.max_gas_limit())
    else {
        return Ok(());
    };
    if gas_limit <= rpc_gas_cap {
        return Ok(());
    }
    metrics::counter!(RPC_GAS_CAP_REJECTED_METRIC).increment(1);
    Err(ErrorObjectOwned::owned::<()>(
        RPC_GAS_CAP_REJECTED_ERROR_CODE,
        format!(
            "{} gas limit {gas_limit} exceeds the RPC gas cap of {rpc_gas_cap} (--rpc.gascap)",
            req.method_name()
        ),
        None,
    ))
}

/// Positional index of each argument that can carry a gas limit the guard checks.
#[derive(Default)]
struct GasLimitSlots {
    block_overrides: Option<usize>,
    bundles: Option<usize>,
    opts: Option<usize>,
    request: Option<usize>,
}

/// Returns where `method` takes a gas limit to check, or `None` when it takes none.
///
/// Indices follow the trait declarations in reth-rpc-eth-api/src/core.rs and
/// reth-rpc-api/src/{trace,debug}.rs. `debug_traceCallMany` ignores
/// `opts.blockOverrides`, so only its bundles are read.
fn gas_limit_slots(method: &str) -> Option<GasLimitSlots> {
    let slots = match method {
        ETH_CREATE_ACCESS_LIST_METHOD => GasLimitSlots {
            request: Some(0),
            ..Default::default()
        },
        ETH_CALL_METHOD => GasLimitSlots {
            block_overrides: Some(3),
            ..Default::default()
        },
        TRACE_CALL_METHOD => GasLimitSlots {
            block_overrides: Some(4),
            ..Default::default()
        },
        ETH_CALL_MANY_METHOD | DEBUG_TRACE_CALL_MANY_METHOD => GasLimitSlots {
            bundles: Some(0),
            ..Default::default()
        },
        DEBUG_TRACE_CALL_METHOD => GasLimitSlots {
            opts: Some(2),
            ..Default::default()
        },
        _ => return None,
    };
    Some(slots)
}

/// Named-param spellings jsonrpsee binds to a `block_overrides` argument.
const BLOCK_OVERRIDES_KEYS: &[&str] = &["block_overrides", "blockOverrides"];

/// The arguments that can carry a gas limit, read with alloy's own types so the
/// guard's view of the wire format cannot drift from the handler's.
#[derive(Default)]
struct GasLimitArgs {
    block_overrides: Option<BlockOverrides>,
    bundles: Option<CallManyBundles>,
    opts: Option<GethDebugTracingCallOptions>,
    request: Option<TransactionRequest>,
}

impl GasLimitArgs {
    /// Returns the largest gas limit the arguments set, or `None` when none is set.
    fn max_gas_limit(&self) -> Option<u64> {
        let request_gas = self.request.as_ref().and_then(|request| request.gas);
        let bundle_overrides = self
            .bundles
            .iter()
            .flatten()
            .filter_map(|bundle| bundle.block_override.as_ref());
        let opts_override = self
            .opts
            .as_ref()
            .and_then(|opts| opts.block_overrides.as_ref());
        self.block_overrides
            .iter()
            .chain(bundle_overrides)
            .chain(opts_override)
            .filter_map(|overrides| overrides.gas_limit)
            .chain(request_gas)
            .max()
    }
}

/// Reads the arguments `slots` names, or `None` when the params cannot be read.
///
/// Only the arguments the method takes are read, so a malformed key the
/// handler ignores cannot hide an override. Positional params skip the
/// arguments before each slot, and a missing trailing one reads as absent, as
/// jsonrpsee binds them.
fn read_gas_limit_args(params: Params<'_>, slots: &GasLimitSlots) -> Option<GasLimitArgs> {
    if params.is_object() {
        // Borrowed `RawValue` leaves unread fields unparsed; `String` keys match
        // escaped names the same way the handler does.
        let named = params.parse::<BTreeMap<String, &RawValue>>().ok()?;
        return Some(GasLimitArgs {
            block_overrides: named_arg(&named, slots.block_overrides, BLOCK_OVERRIDES_KEYS)?,
            bundles: named_arg(&named, slots.bundles, &["bundles"])?,
            opts: named_arg(&named, slots.opts, &["opts"])?,
            request: named_arg(&named, slots.request, &["request"])?,
        });
    }
    let last = [
        slots.block_overrides,
        slots.bundles,
        slots.opts,
        slots.request,
    ]
    .into_iter()
    .flatten()
    .max()?;
    let mut seq = params.sequence();
    let mut args = GasLimitArgs::default();
    for index in 0..=last {
        let slot = Some(index);
        if slot == slots.block_overrides {
            args.block_overrides = seq.optional_next().ok()?;
        } else if slot == slots.bundles {
            args.bundles = seq.optional_next().ok()?;
        } else if slot == slots.opts {
            args.opts = seq.optional_next().ok()?;
        } else if slot == slots.request {
            args.request = seq.optional_next().ok()?;
        } else {
            seq.optional_next::<IgnoredAny>().ok()?;
        }
    }
    Some(args)
}

/// Reads the named argument under `keys`; `Some(None)` when the method does not
/// take it (`slot` is `None`) or the request omits it.
///
/// Returns `None` when the value cannot be read or is set under two spellings,
/// both of which the handler rejects.
fn named_arg<T: DeserializeOwned>(
    named: &BTreeMap<String, &RawValue>,
    slot: Option<usize>,
    keys: &[&str],
) -> Option<Option<T>> {
    if slot.is_none() {
        return Some(None);
    }
    let mut values = keys.iter().filter_map(|key| named.get(*key));
    let Some(value) = values.next() else {
        return Some(None);
    };
    if values.next().is_some() {
        return None;
    }
    serde_json::from_str(value.get()).ok()
}

/// Applies `guard` to every call in a batch, replacing the entries it rejects
/// with their error and forwarding the rest through `service` so inner
/// middleware still sees them.
async fn forward_batch_with_guard<S, F>(
    service: &S,
    batch: Batch<'_>,
    max_response_body_size: usize,
    guard: F,
) -> MethodResponse
where
    // Only `&S` is held across awaits, so `Sync` is all this needs.
    S: RpcServiceT<MethodResponse = MethodResponse, NotificationResponse = MethodResponse> + Sync,
    F: Fn(&Request<'_>) -> Result<(), ErrorObjectOwned> + Send,
{
    let mut builder = BatchResponseBuilder::new_with_limit(max_response_body_size);
    let mut got_notif = false;
    for entry in batch {
        match entry {
            Ok(BatchEntry::Call(request)) => {
                let response = match guard(&request) {
                    Ok(()) => service.call(request).await,
                    Err(err) => MethodResponse::error(request.id(), err),
                };
                if let Err(too_large) = builder.append(response) {
                    return too_large;
                }
            }
            Ok(BatchEntry::Notification(notification)) => {
                got_notif = true;
                service.notification(notification).await;
            }
            Err(err) => {
                let (error, id) = err.into_parts();
                if let Err(too_large) = builder.append(MethodResponse::error(id, error)) {
                    return too_large;
                }
            }
        }
    }
    if builder.is_empty() && got_notif {
        return MethodResponse::notification();
    }
    MethodResponse::from_batch(builder.finish())
}

/// RPC middleware that prevents websocket subscriptions and HTTP filters for pending transactions.
#[derive(Clone, Debug)]
pub struct NoPendingTransactionsRpcMiddleware<S> {
    service: S,
    max_response_body_size: usize,
}

impl<S> NoPendingTransactionsRpcMiddleware<S> {
    /// Creates a new instance of the middleware.
    pub fn new(service: S) -> Self {
        Self {
            service,
            max_response_body_size: usize::MAX,
        }
    }
}

impl<S> RpcServiceT for NoPendingTransactionsRpcMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let service = self.service.clone();
        async move { intercept_or_forward(&service, req).await }
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let service = self.service.clone();
        let max_response_body_size = self.max_response_body_size;
        async move {
            let mut builder = BatchResponseBuilder::new_with_limit(max_response_body_size);
            let mut got_notif = false;
            for entry in req {
                match entry {
                    Ok(BatchEntry::Call(request)) => {
                        let response = intercept_or_forward(&service, request).await;
                        if let Err(too_large) = builder.append(response) {
                            return too_large;
                        }
                    }
                    Ok(BatchEntry::Notification(notification)) => {
                        got_notif = true;
                        service.notification(notification).await;
                    }
                    Err(err) => {
                        let (error, id) = err.into_parts();
                        if let Err(too_large) = builder.append(MethodResponse::error(id, error)) {
                            return too_large;
                        }
                    }
                }
            }
            if builder.is_empty() && got_notif {
                return MethodResponse::notification();
            }
            MethodResponse::from_batch(builder.finish())
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.service.notification(n)
    }
}

/// Intercepts pending-tx RPCs (error or null) or forwards to the inner service.
///
/// `eth_getTransactionBySenderAndNonce` is forwarded (Reth answers from the
/// pool, then the canonical chain); pool-only answers are stripped afterward.
async fn intercept_or_forward<'a, S>(service: &S, req: Request<'a>) -> MethodResponse
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync,
{
    if let Err(err) = error_if_pending_tx_rpc(&req) {
        return MethodResponse::error(req.id(), err);
    }
    if is_pending_block_query(&req) {
        return null_response(&req);
    }
    let hide_pending = may_return_pending_tx(&req);
    let req_id = req.id().into_owned();
    let resp = service.call(req).await;
    if hide_pending {
        hide_pending_sender_nonce_result(req_id, resp)
    } else {
        resp
    }
}

/// RPC middleware that rejects raw transaction submission calls carrying a
/// pre-EIP-155 (replay-unprotected) transaction.
///
/// Only the RPC submission path is gated — peer-gossiped transactions and
/// transactions included in blocks by other validators still flow through the
/// txpool and execution layers unchanged.
#[derive(Clone, Debug)]
pub struct RejectUnprotectedTxsMiddleware<S> {
    service: S,
    allow_unprotected_txs: bool,
    max_response_body_size: usize,
}

impl<S> RejectUnprotectedTxsMiddleware<S> {
    pub fn new(service: S) -> Self {
        Self {
            service,
            allow_unprotected_txs: false,
            max_response_body_size: usize::MAX,
        }
    }
}

impl<S> RpcServiceT for RejectUnprotectedTxsMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let service = self.service.clone();
        let allow_unprotected_txs = self.allow_unprotected_txs;
        async move {
            if !allow_unprotected_txs {
                if let Err(err) = error_if_unprotected_send_raw_tx(&req) {
                    return MethodResponse::error(req.id(), err);
                }
            }
            service.call(req).await
        }
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let service = self.service.clone();
        let allow_unprotected_txs = self.allow_unprotected_txs;
        let max_response_body_size = self.max_response_body_size;
        async move {
            forward_batch_with_guard(&service, req, max_response_body_size, |request| {
                if allow_unprotected_txs {
                    return Ok(());
                }
                error_if_unprotected_send_raw_tx(request)
            })
            .await
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.service.notification(n)
    }
}

/// Returns an error if the request is a raw transaction submission call with a
/// pre-EIP-155 (replay-unprotected) payload.
///
/// Malformed payloads (bad hex, undecodable RLP) are forwarded unchanged so
/// the inner RPC handler can return its own precise error.
fn error_if_unprotected_send_raw_tx(req: &Request<'_>) -> Result<(), ErrorObjectOwned> {
    if !is_raw_transaction_submission(req.method_name()) {
        return Ok(());
    }
    let Some(bytes) = extract_raw_tx_bytes(req) else {
        return Ok(());
    };
    let Ok(envelope) = TxEnvelope::decode_2718_exact(bytes.as_ref()) else {
        return Ok(());
    };
    if envelope.is_replay_protected() {
        return Ok(());
    }
    Err(ErrorObjectOwned::owned::<()>(
        UNPROTECTED_TX_ERROR_CODE,
        UNPROTECTED_TX_ERROR_MSG,
        None,
    ))
}

fn is_raw_transaction_submission(method: &str) -> bool {
    method == ETH_SEND_RAW_TRANSACTION_METHOD || method == ETH_SEND_RAW_TRANSACTION_SYNC_METHOD
}

/// Extracts the raw transaction bytes from a raw-tx submission request.
fn extract_raw_tx_bytes(req: &Request<'_>) -> Option<Bytes> {
    extract_param::<Bytes>(req.params(), &[RAW_TX_BYTES_OBJECT_KEY])
}

/// Per-upstream relay clients plus the sticky-selection cursor.
///
/// Cheap to clone: clients and the sticky index are shared via `Arc`, so every
/// RPC transport that clones the layer observes the same failover state.
#[derive(Clone, Debug)]
pub struct TxRelays {
    clients: Arc<[RpcClient]>,
    sticky: Arc<AtomicUsize>,
    max_response_body_size: usize,
    request_timeout: Duration,
}

/// Result of forwarding a raw-tx submission across the upstream list.
enum RelayOutcome {
    /// An upstream accepted the tx; carries the JSON result to return.
    Ok(serde_json::Value),
    /// A reachable upstream returned a JSON-RPC error; return verbatim.
    UpstreamError(ErrorObjectOwned),
    /// An upstream rejected the request itself with a status no peer would accept
    /// either; the pass ends without trying the rest.
    Rejected,
    /// Every upstream failed a full pass.
    AllUnreachable,
}

impl TxRelays {
    /// Builds relay state from priority-ordered upstream URLs, or `None` when
    /// none are valid (relaying disabled). URLs are pre-validated at the CLI
    /// boundary; any that fail to parse here are skipped.
    ///
    /// `timeout` is applied as the connection timeout to every upstream and, in
    /// [`Self::forward`], as the request timeout for async submissions.
    pub fn new(urls: &[String], max_response_body_size: usize, timeout: Duration) -> Option<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(timeout)
            .build()
            .inspect_err(|e| {
                tracing::warn!(target: "rpc::relay", %e, "failed to build tx relay HTTP client; relaying disabled");
            })
            .ok()?;
        let clients: Vec<RpcClient> = urls
            .iter()
            .filter_map(|u| match reqwest::Url::parse(u) {
                Ok(url) => Some(RpcClient::new_http_with_client(http.clone(), url)),
                Err(e) => {
                    tracing::warn!(target: "rpc::relay", url = %u, %e, "skipping invalid tx relay URL");
                    None
                }
            })
            .collect();
        (!clients.is_empty()).then(|| Self {
            clients: clients.into(),
            sticky: Arc::new(AtomicUsize::new(0)),
            max_response_body_size,
            request_timeout: timeout,
        })
    }

    /// Forwards a raw-tx submission with sticky last-good, cyclic advance:
    /// start at the sticky upstream, advance past any that fail with a transport
    /// error, a timeout, or an HTTP status [`should_failover`] treats as an
    /// upstream fault, wrapping once through the list before giving up.
    ///
    /// Only an upstream that answers moves the cursor.
    async fn forward(&self, method: &str, bytes: &Bytes) -> RelayOutcome {
        let n = self.clients.len();
        let start = self.sticky.load(Ordering::Relaxed).min(n.saturating_sub(1));
        for (idx, client) in self.clients.iter().enumerate().cycle().skip(start).take(n) {
            let call = client.request::<_, serde_json::Value>(method.to_string(), (bytes.clone(),));
            let result = match tokio::time::timeout(self.request_timeout, call).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    metrics::counter!("arc_tx_relay_failovers_total").increment(1);
                    tracing::debug!(target: "rpc::relay", idx, "tx relay upstream timed out; advancing");
                    continue;
                }
            };
            match result {
                Ok(result) => {
                    self.sticky.store(idx, Ordering::Relaxed);
                    return RelayOutcome::Ok(result);
                }
                Err(RpcError::ErrorResp(payload)) => {
                    self.sticky.store(idx, Ordering::Relaxed);
                    let code = i32::try_from(payload.code)
                        .unwrap_or_else(|_| ErrorCode::InternalError.code());
                    return RelayOutcome::UpstreamError(ErrorObjectOwned::owned(
                        code,
                        payload.message.into_owned(),
                        payload.data,
                    ));
                }
                Err(err) if should_failover(&err) => {
                    metrics::counter!("arc_tx_relay_failovers_total").increment(1);
                    tracing::debug!(target: "rpc::relay", idx, error = %relay_error_category(&err), "tx relay upstream failed; advancing");
                }
                Err(err) => {
                    tracing::warn!(target: "rpc::relay", idx, error = %relay_error_category(&err), "tx relay upstream returned a non-retryable error");
                    return RelayOutcome::Rejected;
                }
            }
        }
        metrics::counter!("arc_tx_relay_exhausted_total").increment(1);
        RelayOutcome::AllUnreachable
    }
}

/// HTTP statuses that describe the request/upstream pairing, not the transaction:
/// a peer upstream given the same bytes would answer normally. 5xx is handled
/// separately by range, which also covers 501.
const UPSTREAM_FAULT_STATUSES: &[u16] = &[401, 403, 404, 405, 407, 408, 429];

/// Returns true when the error means the upstream is unusable and the relay
/// should advance: any transport failure, an HTTP 5xx, or a status in
/// [`UPSTREAM_FAULT_STATUSES`].
fn should_failover(err: &RpcError<TransportErrorKind>) -> bool {
    match err {
        RpcError::Transport(TransportErrorKind::HttpError(http)) => {
            http.status >= 500 || UPSTREAM_FAULT_STATUSES.contains(&http.status)
        }
        RpcError::ErrorResp(_) => false,
        _ => true,
    }
}

/// Coarse, URL-free category for a relay error, safe to log.
///
/// `RpcError`'s `Display` embeds the upstream URL (via reqwest), which can
/// carry API keys in its path or query; this exposes only the failure shape.
fn relay_error_category(err: &RpcError<TransportErrorKind>) -> String {
    match err {
        RpcError::Transport(TransportErrorKind::HttpError(http)) => {
            format!("http status {}", http.status)
        }
        RpcError::Transport(_) => "transport error".to_string(),
        RpcError::ErrorResp(_) => "json-rpc error response".to_string(),
        _ => "rpc error".to_string(),
    }
}

/// RPC middleware that relays raw-transaction submission to a prioritized list
/// of upstreams with failover, retaining each accepted tx in the local pool.
#[derive(Clone, Debug)]
pub struct TxRelayMiddleware<S> {
    service: S,
    relays: TxRelays,
}

impl<S> TxRelayMiddleware<S>
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync,
{
    /// Relays a raw-tx submission, then best-effort retains it in the local pool.
    /// Requests with unparseable params are forwarded to the local handler so it
    /// can return its own precise error.
    async fn relay(&self, req: Request<'_>) -> MethodResponse {
        let Some(bytes) = extract_raw_tx_bytes(&req) else {
            return self.service.call(req).await;
        };
        let id = req.id().into_owned();
        let method = req.method_name().to_string();
        match self.relays.forward(&method, &bytes).await {
            RelayOutcome::Ok(result) => {
                self.local_add(&bytes).await;
                MethodResponse::response(
                    id,
                    ResponsePayload::success(result).into(),
                    self.relays.max_response_body_size,
                )
            }
            RelayOutcome::UpstreamError(err) => MethodResponse::error(id, err),
            RelayOutcome::Rejected => MethodResponse::error(
                id,
                ErrorObjectOwned::owned::<()>(
                    RELAY_REJECTED_ERROR_CODE,
                    RELAY_REJECTED_ERROR_MSG,
                    None,
                ),
            ),
            RelayOutcome::AllUnreachable => MethodResponse::error(
                id,
                ErrorObjectOwned::owned::<()>(
                    RELAY_UNAVAILABLE_ERROR_CODE,
                    RELAY_UNAVAILABLE_ERROR_MSG,
                    None,
                ),
            ),
        }
    }

    /// Retains an accepted tx in the local pool via an async
    /// `eth_sendRawTransaction` to the inner service. Best-effort: the result,
    /// including any error, is intentionally ignored.
    async fn local_add(&self, bytes: &Bytes) {
        let Ok(params) = serde_json::value::to_raw_value(&(bytes,)) else {
            return;
        };
        let req = Request::owned(
            ETH_SEND_RAW_TRANSACTION_METHOD.to_string(),
            Some(params),
            Id::Null,
        );
        let _ = self.service.call(req).await;
    }
}

impl<S> RpcServiceT for TxRelayMiddleware<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            NotificationResponse = MethodResponse,
            BatchResponse = MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let this = self.clone();
        async move {
            if is_raw_transaction_submission(req.method_name()) {
                this.relay(req).await
            } else {
                this.service.call(req).await
            }
        }
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let this = self.clone();
        let max_response_body_size = self.relays.max_response_body_size;
        async move {
            let mut builder = BatchResponseBuilder::new_with_limit(max_response_body_size);
            let mut got_notif = false;
            for entry in req {
                match entry {
                    Ok(BatchEntry::Call(request)) => {
                        let response = if is_raw_transaction_submission(request.method_name()) {
                            this.relay(request).await
                        } else {
                            this.service.call(request).await
                        };
                        if let Err(too_large) = builder.append(response) {
                            return too_large;
                        }
                    }
                    Ok(BatchEntry::Notification(notification)) => {
                        got_notif = true;
                        this.service.notification(notification).await;
                    }
                    Err(err) => {
                        let (error, id) = err.into_parts();
                        if let Err(too_large) = builder.append(MethodResponse::error(id, error)) {
                            return too_large;
                        }
                    }
                }
            }
            if builder.is_empty() && got_notif {
                return MethodResponse::notification();
            }
            MethodResponse::from_batch(builder.finish())
        }
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.service.notification(n)
    }
}

/// Returns an error if the request is a pending-tx RPC (subscription or filter) that would leak pending transaction data.
fn error_if_pending_tx_rpc<'a>(req: &Request<'a>) -> Result<(), ErrorObject<'a>> {
    if req.method_name() == ETH_NEW_PENDING_TX_FILTER_METHOD {
        let error = ErrorObjectOwned::owned::<()>(
            PENDING_TX_SUBSCRIPTION_ERROR_CODE,
            "Pending transaction filters are not allowed",
            None,
        );
        return Err(error);
    }

    // Extract the binding's own type so every encoding it accepts is checked.
    if req.method_name() == ETH_SUBSCRIBE_METHOD
        && extract_param::<SubscriptionKind>(req.params(), &[SUBSCRIPTION_KIND_OBJECT_KEY])
            .is_some_and(|kind| kind == SubscriptionKind::NewPendingTransactions)
    {
        let error = ErrorObjectOwned::owned::<()>(
            PENDING_TX_SUBSCRIPTION_ERROR_CODE,
            "Subscriptions to pending transactions are not allowed",
            None,
        );
        return Err(error);
    }
    Ok(())
}

/// Returns true if the RPC result may include a pending transaction.
///
/// Currently `eth_getTransactionBySenderAndNonce`. Reth answers from the pool,
/// then the canonical chain, so returning `null` on the method name alone would
/// hide mined txs too. Callers forward, then
/// [`hide_pending_sender_nonce_result`] strips pending answers.
fn may_return_pending_tx(req: &Request<'_>) -> bool {
    req.method_name() == ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD
}

fn is_pending_block_method(method: &str) -> bool {
    method == ETH_GET_BLOCK_BY_NUMBER_METHOD
        || method == ETH_GET_BLOCK_RECEIPTS_METHOD
        || method == ETH_GET_BLOCK_TX_COUNT_BY_NUMBER_METHOD
        || method == ETH_GET_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD
        || method == ETH_GET_RAW_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD
        || method == ETH_GET_UNCLE_COUNT_BY_BLOCK_NUMBER_METHOD
        || method == ETH_GET_HEADER_BY_NUMBER_METHOD
}

/// Returns true if the request queries pending-block state via a block number/tag parameter.
///
/// The consensus engine may briefly expose a pending block via `provider().pending_block()`
/// even when `--rpc.pending-block=none` is set.  Intercepting at the middleware layer
/// guarantees a consistent `null` response regardless of consensus-engine state.
fn is_pending_block_query(req: &Request<'_>) -> bool {
    if !is_pending_block_method(req.method_name()) {
        return false;
    }
    let params = req.params();
    // eth_getBlockReceipts accepts BlockId: handles string tags and EIP-1898 object form
    // ({"blockNumber": "pending"}).  All other methods accept BlockNumberOrTag.
    if req.method_name() == ETH_GET_BLOCK_RECEIPTS_METHOD {
        return extract_param::<BlockId>(
            params,
            &[BLOCK_ID_OBJECT_KEY_SNAKE, BLOCK_ID_OBJECT_KEY_CAMEL],
        )
        .is_some_and(|id| id.is_pending());
    }
    // Object key for named params — coupled to jsonrpsee proc-macro field names.
    let key = if req.method_name() == ETH_GET_HEADER_BY_NUMBER_METHOD {
        BLOCK_HEADER_NUMBER_OBJECT_KEY
    } else {
        BLOCK_NUMBER_OBJECT_KEY
    };
    extract_param::<BlockNumberOrTag>(params, &[key]).is_some_and(|t| t.is_pending())
}

/// Extracts and deserializes the first positional or named RPC param.
///
/// For object-style params, tries each key in `keys` in order. For array-style
/// params, reads the first element. Returns `None` on missing field or parse failure.
///
/// `BlockNumberOrTag` deserialization lowercases tags internally, so case variants
/// ("Pending", "PENDING") deserialize correctly without extra normalization.
fn extract_param<T: DeserializeOwned>(params: Params<'_>, keys: &[&str]) -> Option<T> {
    if params.is_object() {
        // Borrowed `RawValue` leaves an unread field's JSON unparsed; `String` keys
        // keep escaped names ("byt\u0065s") matching the same field the handler binds.
        let obj = params.parse::<BTreeMap<String, &RawValue>>().ok()?;
        let val = keys.iter().find_map(|k| obj.get(*k))?;
        serde_json::from_str(val.get()).ok()
    } else {
        params.sequence().optional_next::<T>().ok().flatten()
    }
}

/// Builds a JSON-RPC success response containing `null`.
fn null_response(req: &Request<'_>) -> MethodResponse {
    null_response_with_id(req.id())
}

fn null_response_with_id(id: Id<'_>) -> MethodResponse {
    let payload = ResponsePayload::success(serde_json::Value::Null);
    MethodResponse::response(id, payload.into(), usize::MAX)
}

/// Replaces a sender+nonce success with `null` unless the result is a mined tx.
///
/// A mined tx is already in a block, so it carries a block number in RPC
/// quantity form. Pool-only answers, unknown nonces, and unexpected payload
/// shapes do not, and are hidden so a drifted inner response cannot leak
/// pending contents.
fn hide_pending_sender_nonce_result(req_id: Id<'_>, resp: MethodResponse) -> MethodResponse {
    if resp.as_error_code().is_some() {
        return resp;
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(resp.as_json().get()) else {
        return null_response_with_id(req_id);
    };
    if is_mined_transaction_result(&json["result"]) {
        return resp;
    }
    null_response_with_id(req_id)
}

fn is_mined_transaction_result(result: &serde_json::Value) -> bool {
    result
        .as_object()
        .and_then(|obj| obj.get("blockNumber"))
        .is_some_and(is_rpc_hex_quantity)
}

/// Ethereum JSON-RPC quantity: `0x` plus one or more hex digits.
fn is_rpc_hex_quantity(value: &serde_json::Value) -> bool {
    let Some(s) = value.as_str() else {
        return false;
    };
    let Some(digits) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_transport::mock::Asserter;
    use jsonrpsee::{
        core::middleware::BatchEntryErr,
        types::{Id, ResponsePayload},
        BatchResponseBuilder,
    };
    use serde_json::value::RawValue;
    use std::borrow::Cow;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Mock RPC service that always returns a success response
    #[derive(Clone, Debug)]
    struct MockRpcService;

    impl RpcServiceT for MockRpcService {
        type MethodResponse = MethodResponse;
        type NotificationResponse = MethodResponse;
        type BatchResponse = MethodResponse;

        // Silence clippy false positive, see <https://github.com/rust-lang/rust-clippy/issues/14372>
        #[allow(clippy::manual_async_fn)]
        fn call<'a>(
            &self,
            req: Request<'a>,
        ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
            async move {
                let payload =
                    ResponsePayload::success(serde_json::Value::String("success".to_string()));
                MethodResponse::response(req.id(), payload.into(), usize::MAX)
            }
        }

        // Silence clippy false positive, see <https://github.com/rust-lang/rust-clippy/issues/14372>
        #[allow(clippy::manual_async_fn)]
        fn batch<'a>(
            &self,
            req: Batch<'a>,
        ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
            let service = self.clone();
            async move {
                let mut response = BatchResponseBuilder::new_with_limit(usize::MAX);
                for r in req {
                    match r {
                        Ok(BatchEntry::Call(request)) => {
                            let payload = ResponsePayload::success(serde_json::Value::String(
                                "success".to_string(),
                            ));
                            response
                                .append(MethodResponse::response(
                                    request.id(),
                                    payload.into(),
                                    usize::MAX,
                                ))
                                .unwrap();
                        }
                        Ok(BatchEntry::Notification(notification)) => {
                            response
                                .append(service.notification(notification).await)
                                .unwrap();
                        }
                        Err(err) => {
                            let (error, id) = err.into_parts();
                            response.append(MethodResponse::error(id, error)).unwrap();
                        }
                    }
                }
                MethodResponse::from_batch(response.finish())
            }
        }

        // Silence clippy false positive, see <https://github.com/rust-lang/rust-clippy/issues/14372>
        #[allow(clippy::manual_async_fn)]
        fn notification<'a>(
            &self,
            _n: Notification<'a>,
        ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
            async move {
                let payload = ResponsePayload::success(serde_json::Value::String(
                    "notification_success".to_string(),
                ));
                MethodResponse::response(Id::Number(0), payload.into(), usize::MAX)
            }
        }
    }

    /// Inner service that returns a configured JSON-RPC success `result`.
    #[derive(Clone, Debug)]
    struct JsonResultRpcService(serde_json::Value);

    impl RpcServiceT for JsonResultRpcService {
        type MethodResponse = MethodResponse;
        type NotificationResponse = MethodResponse;
        type BatchResponse = MethodResponse;

        #[allow(clippy::manual_async_fn)]
        fn call<'a>(
            &self,
            req: Request<'a>,
        ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
            let result = self.0.clone();
            async move {
                let payload = ResponsePayload::success(result);
                MethodResponse::response(req.id(), payload.into(), usize::MAX)
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn batch<'a>(
            &self,
            req: Batch<'a>,
        ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
            let service = self.clone();
            async move {
                let mut response = BatchResponseBuilder::new_with_limit(usize::MAX);
                for r in req {
                    match r {
                        Ok(BatchEntry::Call(request)) => {
                            let payload = ResponsePayload::success(service.0.clone());
                            response
                                .append(MethodResponse::response(
                                    request.id(),
                                    payload.into(),
                                    usize::MAX,
                                ))
                                .unwrap();
                        }
                        Ok(BatchEntry::Notification(notification)) => {
                            response
                                .append(service.notification(notification).await)
                                .unwrap();
                        }
                        Err(err) => {
                            let (error, id) = err.into_parts();
                            response.append(MethodResponse::error(id, error)).unwrap();
                        }
                    }
                }
                MethodResponse::from_batch(response.finish())
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn notification<'a>(
            &self,
            _n: Notification<'a>,
        ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
            async move { MethodResponse::notification() }
        }
    }

    /// Inner service that always returns a JSON-RPC error.
    #[derive(Clone, Debug)]
    struct ErrorRpcService;

    impl RpcServiceT for ErrorRpcService {
        type MethodResponse = MethodResponse;
        type NotificationResponse = MethodResponse;
        type BatchResponse = MethodResponse;

        #[allow(clippy::manual_async_fn)]
        fn call<'a>(
            &self,
            req: Request<'a>,
        ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
            async move {
                MethodResponse::error(
                    req.id(),
                    ErrorObjectOwned::owned::<()>(-32000, "inner error", None),
                )
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn batch<'a>(
            &self,
            _req: Batch<'a>,
        ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
            async move { MethodResponse::notification() }
        }

        #[allow(clippy::manual_async_fn)]
        fn notification<'a>(
            &self,
            _n: Notification<'a>,
        ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
            async move { MethodResponse::notification() }
        }
    }

    fn create_request_with_params(
        method: &str,
        params: Box<RawValue>,
        id: u64,
    ) -> Request<'static> {
        Request::owned(method.to_string(), Some(params), Id::Number(id))
    }

    // ── Middleware active (default) ─────────────────────────────────────
    //
    // When active, the middleware intercepts pending-state RPCs:
    // subscriptions, filters, and block queries.
    // The binary default is middleware ON; users opt out via
    // --arc.expose-pending-txs on trusted/internal nodes.

    // -- pending txs: blocked --

    #[tokio::test]
    async fn test_enabled_blocks_pending_tx_subscription() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let params = RawValue::from_string(r#"["newPendingTransactions"]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_SUBSCRIBE_METHOD, params, 1);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_some(),
            "filter_pending_txs=true should block newPendingTransactions subscription"
        );
        assert_eq!(
            response.as_error_code().unwrap(),
            PENDING_TX_SUBSCRIPTION_ERROR_CODE
        );
    }

    // jsonrpsee accepts object params for subscriptions too, keyed by the trait's
    // arg name (`kind`), with the optional second arg omitted or explicitly null.
    #[tokio::test]
    async fn test_enabled_blocks_pending_tx_subscription_object_params() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let cases: &[&str] = &[
            r#"{"kind":"newPendingTransactions"}"#,
            r#"{"kind":"newPendingTransactions","params":null}"#,
        ];
        for params_json in cases {
            let params = RawValue::from_string(params_json.to_string()).unwrap();
            let request = create_request_with_params(ETH_SUBSCRIBE_METHOD, params, 1);
            let response = middleware.call(request).await;

            assert_eq!(
                response.as_error_code(),
                Some(PENDING_TX_SUBSCRIPTION_ERROR_CODE),
                "filter_pending_txs=true should block {params_json}"
            );
        }
    }

    // `{"newPendingTransactions":null}` parses to the same kind as the bare string.
    #[tokio::test]
    async fn test_enabled_blocks_pending_tx_subscription_map_form_kind() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let cases: &[&str] = &[
            r#"[{"newPendingTransactions":null}]"#,
            r#"{"kind":{"newPendingTransactions":null}}"#,
        ];
        for params_json in cases {
            let params = RawValue::from_string(params_json.to_string()).unwrap();
            let request = create_request_with_params(ETH_SUBSCRIBE_METHOD, params, 1);
            let response = middleware.call(request).await;

            assert_eq!(
                response.as_error_code(),
                Some(PENDING_TX_SUBSCRIPTION_ERROR_CODE),
                "filter_pending_txs=true should block {params_json}"
            );
        }
    }

    #[tokio::test]
    async fn test_enabled_blocks_pending_tx_filter() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let params = RawValue::from_string(r#"[]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_NEW_PENDING_TX_FILTER_METHOD, params, 1);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_some(),
            "filter_pending_txs=true should block eth_newPendingTransactionFilter"
        );
        assert_eq!(
            response.as_error_code().unwrap(),
            PENDING_TX_SUBSCRIPTION_ERROR_CODE
        );
    }

    // -- allowed subscriptions and methods --

    #[tokio::test]
    async fn test_enabled_allows_non_pending_subscriptions() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let cases: &[(&str, &str)] = &[
            (r#"["newHeads"]"#, "newHeads"),
            (r#"["logs"]"#, "logs"),
            (r#"["syncing"]"#, "syncing"),
            (r#"["NewPendingTransactions"]"#, "wrong-case pendingTx"),
            ("[]", "empty params"),
            ("[123]", "non-string params"),
            (r#"{"kind":"newHeads"}"#, "object newHeads"),
            (r#"{"kind":"logs"}"#, "object logs"),
            (r#"{"kind":"syncing"}"#, "object syncing"),
            (
                r#"{"kind":"NewPendingTransactions"}"#,
                "object wrong-case pendingTx",
            ),
            ("{}", "object missing kind"),
            (r#"{"kind":123}"#, "object non-string kind"),
        ];
        for (params_json, label) in cases {
            let params = RawValue::from_string(params_json.to_string()).unwrap();
            let request = create_request_with_params(ETH_SUBSCRIBE_METHOD, params, 1);
            let response = middleware.call(request).await;
            assert!(
                response.as_error_code().is_none(),
                "filter_pending_txs=true should allow {label}"
            );
        }
    }

    #[tokio::test]
    async fn test_enabled_allows_non_pending_methods() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let methods = &[
            "eth_blockNumber",
            "eth_getBalance",
            "eth_getTransactionByHash",
            "eth_call",
            "eth_newBlockFilter",
            "net_version",
        ];
        for method in methods {
            let params = RawValue::from_string("[]".to_string()).unwrap();
            let request = create_request_with_params(method, params, 1);
            let response = middleware.call(request).await;
            assert!(
                response.as_error_code().is_none(),
                "filter_pending_txs=true should allow {method}"
            );
        }
    }

    #[tokio::test]
    async fn test_enabled_allows_notifications() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let notification_params = Some(Cow::Owned(
            RawValue::from_string(r#"{"subscription":"0x1","result":"0x123"}"#.to_string())
                .unwrap(),
        ));
        let notification =
            Notification::new(Cow::Borrowed("eth_subscription"), notification_params);
        let response = middleware.notification(notification).await;

        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=true should allow notifications"
        );
    }

    // -- pending block: intercepted --
    //
    // eth_getBlockByNumber("pending") returns null (success, not error).
    // Other block tags ("latest", "0x1", etc.) pass through unchanged.

    #[tokio::test]
    async fn test_enabled_pending_block_returns_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let params = RawValue::from_string(r#"["pending", false]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_GET_BLOCK_BY_NUMBER_METHOD, params, 1);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=true should return success (not error) for pending block"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert!(
            json["result"].is_null(),
            "filter_pending_txs=true should return null for pending block"
        );
    }

    #[tokio::test]
    async fn test_enabled_pending_block_full_txs_returns_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let params = RawValue::from_string(r#"["pending", true]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_GET_BLOCK_BY_NUMBER_METHOD, params, 2);
        let response = middleware.call(request).await;

        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert!(
            json["result"].is_null(),
            "filter_pending_txs=true should return null for pending block with full txs"
        );
    }

    #[tokio::test]
    async fn test_enabled_latest_block_passes_through() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let params = RawValue::from_string(r#"["latest", false]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_GET_BLOCK_BY_NUMBER_METHOD, params, 3);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=true should allow getBlockByNumber(\"latest\")"
        );
    }

    #[tokio::test]
    async fn test_enabled_numbered_block_passes_through() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let params = RawValue::from_string(r#"["0x1", false]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_GET_BLOCK_BY_NUMBER_METHOD, params, 4);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=true should allow getBlockByNumber(\"0x1\")"
        );
    }

    // -- pool pending tx lookup: intercepted --
    //
    // eth_getTransactionBySenderAndNonce is forwarded. Pool-only and unknown
    // nonces become null (success, not error). A mined tx is returned as-is.

    const SENDER_NONCE_ARRAY_PARAMS: &str =
        r#"["0x1234567890abcdef1234567890abcdef12345678", "0x0"]"#;
    const SENDER_NONCE_OBJECT_PARAMS: &str =
        r#"{"address":"0x1234567890abcdef1234567890abcdef12345678","nonce":"0x0"}"#;

    fn mined_tx_json() -> serde_json::Value {
        serde_json::json!({
            "hash": "0xaaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999",
            "blockHash": "0x1111111111111111111111111111111111111111111111111111111111111111",
            "blockNumber": "0x1",
            "from": "0x1234567890abcdef1234567890abcdef12345678",
            "nonce": "0x0",
        })
    }

    fn pending_tx_json() -> serde_json::Value {
        serde_json::json!({
            "hash": "0xaaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999",
            "blockHash": null,
            "blockNumber": null,
            "from": "0x1234567890abcdef1234567890abcdef12345678",
            "nonce": "0x0",
        })
    }

    fn tx_with_block_number(block_number: serde_json::Value) -> serde_json::Value {
        let mut tx = mined_tx_json();
        tx["blockNumber"] = block_number;
        tx
    }

    #[tokio::test]
    async fn test_enabled_sender_nonce_lookup_hides_pending_keeps_mined() {
        let cases: &[(&str, &str, serde_json::Value, bool)] = &[
            (
                "committed nonce, array params",
                SENDER_NONCE_ARRAY_PARAMS,
                mined_tx_json(),
                false,
            ),
            (
                "committed nonce, object params",
                SENDER_NONCE_OBJECT_PARAMS,
                mined_tx_json(),
                false,
            ),
            (
                "pool-only nonce, array params",
                SENDER_NONCE_ARRAY_PARAMS,
                pending_tx_json(),
                true,
            ),
            (
                "pool-only nonce, object params",
                SENDER_NONCE_OBJECT_PARAMS,
                pending_tx_json(),
                true,
            ),
            (
                "unknown nonce, array params",
                SENDER_NONCE_ARRAY_PARAMS,
                serde_json::Value::Null,
                true,
            ),
            (
                "unknown nonce, object params",
                SENDER_NONCE_OBJECT_PARAMS,
                serde_json::Value::Null,
                true,
            ),
            (
                "malformed blockNumber bool",
                SENDER_NONCE_ARRAY_PARAMS,
                tx_with_block_number(serde_json::json!(true)),
                true,
            ),
            (
                "malformed blockNumber json number",
                SENDER_NONCE_ARRAY_PARAMS,
                tx_with_block_number(serde_json::json!(1)),
                true,
            ),
            (
                "malformed blockNumber decimal string",
                SENDER_NONCE_ARRAY_PARAMS,
                tx_with_block_number(serde_json::json!("1")),
                true,
            ),
            (
                "malformed blockNumber empty quantity",
                SENDER_NONCE_ARRAY_PARAMS,
                tx_with_block_number(serde_json::json!("0x")),
                true,
            ),
            (
                "malformed blockNumber non-hex",
                SENDER_NONCE_ARRAY_PARAMS,
                tx_with_block_number(serde_json::json!("0xg")),
                true,
            ),
        ];
        for (label, params_json, inner, expect_null) in cases {
            let middleware =
                NoPendingTransactionsRpcMiddleware::new(JsonResultRpcService(inner.clone()));
            let params = RawValue::from_string(params_json.to_string()).unwrap();
            let request =
                create_request_with_params(ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD, params, 1);
            let response = middleware.call(request).await;

            assert!(
                response.as_error_code().is_none(),
                "{label}: should return success, not error"
            );
            let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
            if *expect_null {
                assert!(
                    json["result"].is_null(),
                    "{label}: pending sender+nonce lookup should return null"
                );
                assert_eq!(
                    json["id"], 1,
                    "{label}: hidden result must keep the request id"
                );
            } else {
                assert_eq!(
                    json["result"], *inner,
                    "{label}: mined sender+nonce lookup should pass through"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_enabled_sender_nonce_lookup_empty_params_returns_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let params = RawValue::from_string("[]".to_string()).unwrap();
        let request = create_request_with_params(ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD, params, 1);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "should return success, not error"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert!(
            json["result"].is_null(),
            "pending inner payload should return null"
        );
        assert_eq!(json["id"], 1, "hidden result must keep the request id");
    }

    #[tokio::test]
    async fn test_enabled_sender_nonce_lookup_forwards_inner_error() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(ErrorRpcService);
        let params = RawValue::from_string(SENDER_NONCE_ARRAY_PARAMS.to_string()).unwrap();
        let request = create_request_with_params(ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD, params, 7);
        let response = middleware.call(request).await;

        assert_eq!(
            response.as_error_code(),
            Some(-32000),
            "inner error should pass through"
        );
    }

    // -- batch requests --
    //
    // Batch entries are intercepted per-entry, consistent with the single-request path:
    // subscriptions/filters → error, pending block → null, pending tx → null.

    #[tokio::test]
    async fn test_enabled_batch_blocks_pending_tx_subscription() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(create_request_with_params(
                "eth_blockNumber",
                RawValue::from_string("[]".to_string()).unwrap(),
                1,
            ))),
            Ok(BatchEntry::Call(create_request_with_params(
                ETH_SUBSCRIBE_METHOD,
                RawValue::from_string(r#"["newPendingTransactions"]"#.to_string()).unwrap(),
                2,
            ))),
        ]);
        let response = middleware.batch(batch).await;
        let json = response.into_json();
        let responses: Vec<serde_json::Value> = serde_json::from_str(json.get()).unwrap();

        assert!(responses[0].get("result").is_some());
        assert!(responses[1].get("error").is_some());
        let error_code = responses[1]["error"]["code"].as_i64().unwrap();
        assert_eq!(error_code, PENDING_TX_SUBSCRIPTION_ERROR_CODE as i64);
    }

    #[tokio::test]
    async fn test_enabled_batch_blocks_pending_tx_filter() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(create_request_with_params(
                "eth_blockNumber",
                RawValue::from_string("[]".to_string()).unwrap(),
                1,
            ))),
            Ok(BatchEntry::Call(create_request_with_params(
                ETH_NEW_PENDING_TX_FILTER_METHOD,
                RawValue::from_string(r#"[]"#.to_string()).unwrap(),
                2,
            ))),
        ]);
        let response = middleware.batch(batch).await;
        let json = response.into_json();
        let responses: Vec<serde_json::Value> = serde_json::from_str(json.get()).unwrap();

        assert!(responses[0].get("result").is_some());
        assert_eq!(
            responses[1]["error"]["code"].as_i64().unwrap(),
            PENDING_TX_SUBSCRIPTION_ERROR_CODE as i64
        );
    }

    #[tokio::test]
    async fn test_enabled_batch_pending_block_returns_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(create_request_with_params(
                "eth_blockNumber",
                RawValue::from_string("[]".to_string()).unwrap(),
                1,
            ))),
            Ok(BatchEntry::Call(create_request_with_params(
                ETH_GET_BLOCK_BY_NUMBER_METHOD,
                RawValue::from_string(r#"["pending", false]"#.to_string()).unwrap(),
                2,
            ))),
        ]);
        let response = middleware.batch(batch).await;
        let json = response.into_json();
        let responses: Vec<serde_json::Value> = serde_json::from_str(json.get()).unwrap();

        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["result"], "success");
        assert!(
            responses[1]["result"].is_null(),
            "batch pending block should return null"
        );
    }

    #[tokio::test]
    async fn test_enabled_batch_new_method_pending_returns_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(create_request_with_params(
                "eth_blockNumber",
                RawValue::from_string("[]".to_string()).unwrap(),
                1,
            ))),
            Ok(BatchEntry::Call(create_request_with_params(
                ETH_GET_BLOCK_RECEIPTS_METHOD,
                RawValue::from_string(r#"["pending"]"#.to_string()).unwrap(),
                2,
            ))),
        ]);
        let response = middleware.batch(batch).await;
        let json = response.into_json();
        let responses: Vec<serde_json::Value> = serde_json::from_str(json.get()).unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["result"], "success");
        assert!(
            responses[1]["result"].is_null(),
            "batch new-method pending should return null"
        );
    }

    #[tokio::test]
    async fn test_enabled_batch_pool_lookup_returns_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(create_request_with_params(
                "eth_blockNumber",
                RawValue::from_string("[]".to_string()).unwrap(),
                1,
            ))),
            Ok(BatchEntry::Call(create_request_with_params(
                ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD,
                RawValue::from_string(
                    r#"["0x1234567890abcdef1234567890abcdef12345678", "0x0"]"#.to_string(),
                )
                .unwrap(),
                2,
            ))),
        ]);
        let response = middleware.batch(batch).await;
        let json = response.into_json();
        let responses: Vec<serde_json::Value> = serde_json::from_str(json.get()).unwrap();

        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["result"], "success");
        assert!(
            responses[1]["result"].is_null(),
            "batch pool lookup should return null"
        );
        assert_eq!(
            responses[1]["id"], 2,
            "hidden batch entry must keep the request id"
        );
    }

    #[tokio::test]
    async fn test_enabled_batch_mixed_interceptions() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let batch = Batch::from(vec![
            // 0: normal method → success
            Ok(BatchEntry::Call(create_request_with_params(
                "eth_blockNumber",
                RawValue::from_string("[]".to_string()).unwrap(),
                1,
            ))),
            // 1: pool lookup → null
            Ok(BatchEntry::Call(create_request_with_params(
                ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD,
                RawValue::from_string("[]".to_string()).unwrap(),
                2,
            ))),
            // 2: pending block → null
            Ok(BatchEntry::Call(create_request_with_params(
                ETH_GET_BLOCK_BY_NUMBER_METHOD,
                RawValue::from_string(r#"["pending", false]"#.to_string()).unwrap(),
                3,
            ))),
            // 3: pending tx subscription → error
            Ok(BatchEntry::Call(create_request_with_params(
                ETH_SUBSCRIBE_METHOD,
                RawValue::from_string(r#"["newPendingTransactions"]"#.to_string()).unwrap(),
                4,
            ))),
        ]);
        let response = middleware.batch(batch).await;
        let json = response.into_json();
        let responses: Vec<serde_json::Value> = serde_json::from_str(json.get()).unwrap();

        assert_eq!(responses.len(), 4);
        assert_eq!(
            responses[0]["result"], "success",
            "normal method should succeed"
        );
        assert!(
            responses[1]["result"].is_null(),
            "pool lookup should return null"
        );
        assert!(
            responses[2]["result"].is_null(),
            "pending block should return null"
        );
        assert!(
            responses[3].get("error").is_some(),
            "pending tx subscription should error"
        );
    }

    #[tokio::test]
    async fn test_enabled_batch_notification_excluded_from_response() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(create_request_with_params(
                "eth_blockNumber",
                RawValue::from_string("[]".to_string()).unwrap(),
                1,
            ))),
            Ok(BatchEntry::Notification(Notification::new(
                Cow::Borrowed("eth_subscription"),
                Some(Cow::Owned(
                    RawValue::from_string(r#"{"subscription":"0x1","result":"0x123"}"#.to_string())
                        .unwrap(),
                )),
            ))),
            Ok(BatchEntry::Call(create_request_with_params(
                "eth_chainId",
                RawValue::from_string("[]".to_string()).unwrap(),
                2,
            ))),
        ]);
        let response = middleware.batch(batch).await;
        let json = response.into_json();
        let responses: Vec<serde_json::Value> = serde_json::from_str(json.get()).unwrap();

        assert_eq!(
            responses.len(),
            2,
            "notification must not produce a response entry"
        );
        assert_eq!(responses[0]["result"], "success");
        assert_eq!(responses[1]["result"], "success");
    }

    // ── Middleware disabled (--arc.expose-pending-txs) ──────────────────
    //
    // The middleware is bypassed entirely. All requests pass through.

    #[tokio::test]
    async fn test_disabled_allows_pending_tx_subscription() {
        let layer = ArcRpcLayer {
            filter_pending_txs: false,
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let params = RawValue::from_string(r#"["newPendingTransactions"]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_SUBSCRIBE_METHOD, params, 99);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=false should allow newPendingTransactions"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(
            json["result"], "success",
            "filter_pending_txs=false should forward to inner service"
        );
    }

    #[tokio::test]
    async fn test_disabled_allows_pending_tx_filter() {
        let layer = ArcRpcLayer {
            filter_pending_txs: false,
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let params = RawValue::from_string(r#"[]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_NEW_PENDING_TX_FILTER_METHOD, params, 101);
        let response = middleware.call(request).await;
        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=false should allow newPendingTransactionFilter"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(
            json["result"], "success",
            "filter_pending_txs=false should forward to inner service"
        );
    }

    #[tokio::test]
    async fn test_disabled_allows_pool_lookup() {
        let layer = ArcRpcLayer {
            filter_pending_txs: false,
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let params = RawValue::from_string(
            r#"["0x1234567890abcdef1234567890abcdef12345678", "0x0"]"#.to_string(),
        )
        .unwrap();
        let request =
            create_request_with_params(ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD, params, 102);
        let response = middleware.call(request).await;
        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=false should allow eth_getTransactionBySenderAndNonce"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(
            json["result"], "success",
            "filter_pending_txs=false should forward to inner service"
        );
    }

    #[tokio::test]
    async fn test_disabled_allows_pending_block() {
        let layer = ArcRpcLayer {
            filter_pending_txs: false,
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let params = RawValue::from_string(r#"["pending", false]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_GET_BLOCK_BY_NUMBER_METHOD, params, 1);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=false should allow getBlockByNumber(\"pending\")"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(
            json["result"], "success",
            "filter_pending_txs=false should forward to inner service"
        );
    }

    #[tokio::test]
    async fn test_disabled_allows_new_pending_block_methods() {
        let layer = ArcRpcLayer {
            filter_pending_txs: false,
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let methods = [
            ETH_GET_BLOCK_RECEIPTS_METHOD,
            ETH_GET_BLOCK_TX_COUNT_BY_NUMBER_METHOD,
            ETH_GET_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD,
            ETH_GET_RAW_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD,
            ETH_GET_UNCLE_COUNT_BY_BLOCK_NUMBER_METHOD,
        ];
        for method in methods {
            let params = RawValue::from_string(r#"["pending"]"#.to_string()).unwrap();
            let request = create_request_with_params(method, params, 1);
            let response = middleware.call(request).await;
            assert!(
                response.as_error_code().is_none(),
                "filter_pending_txs=false should allow {method}"
            );
            let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
            assert_eq!(
                json["result"], "success",
                "filter_pending_txs=false should forward {method}"
            );
        }
    }

    // ── ArcRpcLayer::default() ──────────────────────────────────────────

    #[test]
    fn test_arc_rpc_layer_default_has_filter_enabled() {
        let layer = ArcRpcLayer::default();
        assert!(
            layer.filter_pending_txs,
            "Default ArcRpcLayer should have filter enabled (opt out via --arc.expose-pending-txs)"
        );
    }

    #[test]
    fn test_arc_rpc_layer_default_max_batch_entries() {
        let layer = ArcRpcLayer::default();
        assert_eq!(layer.max_batch_entries, ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT);
    }

    // ── BatchSizeLimitMiddleware ────────────────────────────────────────

    fn make_call_entry(method: &str, id: u64) -> BatchEntry<'static> {
        BatchEntry::Call(create_request_with_params(
            method,
            RawValue::from_string("[]".to_string()).unwrap(),
            id,
        ))
    }

    #[tokio::test]
    async fn test_batch_size_limit_at_limit_passes_through() {
        let middleware = BatchSizeLimitMiddleware::new(MockRpcService, nz(3));
        let batch = Batch::from(vec![
            Ok(make_call_entry("eth_blockNumber", 1)),
            Ok(make_call_entry("eth_blockNumber", 2)),
            Ok(make_call_entry("eth_blockNumber", 3)),
        ]);
        let response = middleware.batch(batch).await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(responses.len(), 3);
        for r in &responses {
            assert_eq!(r["result"], "success");
        }
    }

    #[tokio::test]
    async fn test_batch_size_limit_below_limit_passes_through() {
        let middleware = BatchSizeLimitMiddleware::new(MockRpcService, nz(10));
        let batch = Batch::from(vec![
            Ok(make_call_entry("eth_blockNumber", 1)),
            Ok(make_call_entry("eth_chainId", 2)),
        ]);
        let response = middleware.batch(batch).await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(responses.len(), 2);
    }

    #[tokio::test]
    async fn test_batch_size_limit_above_limit_rejected() {
        let middleware = BatchSizeLimitMiddleware::new(MockRpcService, nz(2));
        let batch = Batch::from(vec![
            Ok(make_call_entry("eth_blockNumber", 1)),
            Ok(make_call_entry("eth_blockNumber", 2)),
            Ok(make_call_entry("eth_blockNumber", 3)),
        ]);
        let response = middleware.batch(batch).await;
        assert_eq!(
            response.as_error_code(),
            Some(BATCH_TOO_LARGE_ERROR_CODE),
            "oversized batch should be rejected with BATCH_TOO_LARGE_ERROR_CODE"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert!(json["id"].is_null(), "batch rejection should use Id::Null");
        let message = json["error"]["message"].as_str().unwrap_or("");
        assert!(
            message.contains("batch size 3") && message.contains("limit of 2"),
            "error message should name the offending size and limit; got: {message}"
        );
    }

    #[tokio::test]
    async fn test_batch_size_limit_single_call_passes_through() {
        let middleware = BatchSizeLimitMiddleware::new(MockRpcService, nz(1));
        let params = RawValue::from_string("[]".to_string()).unwrap();
        let request = create_request_with_params("eth_blockNumber", params, 1);
        let response = middleware.call(request).await;
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(json["result"], "success");
    }

    #[tokio::test]
    async fn test_batch_size_limit_composes_with_pending_filter() {
        // ArcRpcLayer composes BatchSizeLimit ∘ NoPendingTransactions. Verify that
        // an oversized batch is rejected before the pending-tx filter iterates entries.
        let layer = ArcRpcLayer {
            filter_pending_txs: true,
            max_batch_entries: nz(2),
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let batch = Batch::from(vec![
            Ok(make_call_entry("eth_blockNumber", 1)),
            Ok(make_call_entry("eth_blockNumber", 2)),
            Ok(make_call_entry("eth_blockNumber", 3)),
        ]);
        let response = middleware.batch(batch).await;
        assert_eq!(response.as_error_code(), Some(BATCH_TOO_LARGE_ERROR_CODE));
    }

    #[tokio::test]
    async fn test_batch_size_limit_applies_when_pending_filter_disabled() {
        // The batch cap must apply even when --arc.expose-pending-txs disables
        // the pending-tx filter (the other half of the composition).
        let layer = ArcRpcLayer {
            filter_pending_txs: false,
            max_batch_entries: nz(2),
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let batch = Batch::from(vec![
            Ok(make_call_entry("eth_blockNumber", 1)),
            Ok(make_call_entry("eth_blockNumber", 2)),
            Ok(make_call_entry("eth_blockNumber", 3)),
        ]);
        let response = middleware.batch(batch).await;
        assert_eq!(response.as_error_code(), Some(BATCH_TOO_LARGE_ERROR_CODE));
    }

    // ── CallManyLimitMiddleware ─────────────────────────────────────────

    /// Shorthand for a non-zero test literal.
    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("test literal is non-zero")
    }

    fn limits(max_calls: usize) -> CallManyLimits {
        CallManyLimits {
            max_calls: nz(max_calls),
            ..Default::default()
        }
    }

    fn limits_with_overrides(max_calls: usize, max_state_overrides: usize) -> CallManyLimits {
        CallManyLimits {
            max_calls: nz(max_calls),
            max_state_overrides: nz(max_state_overrides),
        }
    }

    fn call_many_middleware(max_calls: usize) -> CallManyLimitMiddleware<MockRpcService> {
        CallManyLimitMiddleware::new(MockRpcService, limits(max_calls))
    }

    /// Builds `eth_callMany` params as a positional array whose bundles hold
    /// `calls_per_bundle[i]` calls each.
    fn call_many_params(calls_per_bundle: &[usize]) -> Box<RawValue> {
        let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
        let bundles: Vec<serde_json::Value> = calls_per_bundle
            .iter()
            .map(|n| serde_json::json!({"transactions": vec![call.clone(); *n]}))
            .collect();
        RawValue::from_string(serde_json::json!([bundles]).to_string()).unwrap()
    }

    fn call_many_request(calls_per_bundle: &[usize]) -> Request<'static> {
        create_request_with_params(ETH_CALL_MANY_METHOD, call_many_params(calls_per_bundle), 1)
    }

    fn call_many_entry(calls_per_bundle: &[usize], id: u64) -> BatchEntry<'static> {
        BatchEntry::Call(create_request_with_params(
            ETH_CALL_MANY_METHOD,
            call_many_params(calls_per_bundle),
            id,
        ))
    }

    /// Asserts a rejection carrying the expected code and message fragments.
    fn assert_call_many_rejected(response: MethodResponse, fragments: &[&str]) {
        assert_eq!(
            response.as_error_code(),
            Some(CALL_MANY_REJECTED_ERROR_CODE),
            "eth_callMany over a limit should be rejected as invalid params"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        let message = json["error"]["message"].as_str().unwrap_or("").to_string();
        for fragment in fragments {
            assert!(
                message.contains(fragment),
                "error message should contain {fragment:?}; got: {message}"
            );
        }
    }

    #[tokio::test]
    async fn test_call_many_at_limit_passes_through() {
        let response = call_many_middleware(3).call(call_many_request(&[3])).await;
        assert_eq!(result_of(response), serde_json::json!("success"));
    }

    #[tokio::test]
    async fn test_call_many_above_limit_rejected() {
        let response = call_many_middleware(3).call(call_many_request(&[4])).await;
        assert_call_many_rejected(
            response,
            &[
                "call count 4",
                "3 allowed per request",
                "--arc.rpc.max-call-many-calls",
            ],
        );
    }

    #[tokio::test]
    async fn test_call_many_boundary_at_smallest_cap() {
        // One is the tightest cap an operator can configure, so pin both sides.
        assert_eq!(
            result_of(call_many_middleware(1).call(call_many_request(&[1])).await),
            serde_json::json!("success")
        );
        assert_call_many_rejected(
            call_many_middleware(1).call(call_many_request(&[2])).await,
            &["call count 2", "1 allowed per request"],
        );
    }

    #[tokio::test]
    async fn test_call_many_sums_calls_across_bundles() {
        // The budget is on the request total, not per bundle: no single bundle
        // here exceeds 5, but together they do.
        let response = call_many_middleware(5)
            .call(call_many_request(&[3, 3]))
            .await;
        assert_call_many_rejected(response, &["call count 6", "5 allowed per request"]);
    }

    #[tokio::test]
    async fn test_call_many_empty_bundles_cost_the_minimum() {
        // Reth skips the bundle loop when a bundle has no transactions, so the
        // counted calls are zero — but the request still resolves a block and
        // opens a read transaction, so it costs one. Three of them need three.
        let response = call_many_middleware(3)
            .batch(Batch::from(vec![
                Ok(call_many_entry(&[0], 1)),
                Ok(call_many_entry(&[0], 2)),
                Ok(call_many_entry(&[0], 3)),
            ]))
            .await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(responses.len(), 3, "three entries fit a budget of three");
        assert_call_many_rejected(
            call_many_middleware(2)
                .batch(Batch::from(vec![
                    Ok(call_many_entry(&[0], 1)),
                    Ok(call_many_entry(&[0], 2)),
                    Ok(call_many_entry(&[0], 3)),
                ]))
                .await,
            &["call count 1", "0 calls remaining"],
        );
    }

    #[tokio::test]
    async fn test_call_many_object_params_measured() {
        // Named params bind the bundle list to `bundles`; the cap must see it.
        let params = RawValue::from_string(
            serde_json::json!({"bundles": [{"transactions": [{}, {}]}]}).to_string(),
        )
        .unwrap();
        let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
        assert_call_many_rejected(
            call_many_middleware(1).call(request).await,
            &["call count 2", "1 allowed per request"],
        );
    }

    #[tokio::test]
    async fn test_call_many_array_form_bundle_is_measured() {
        // Serde also accepts a struct in array form, so alloy's Bundle parses
        // `[transactions, blockOverride]`. CallManyBundle must match that arity
        // or the request slips past the cap unmeasured.
        let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
        let params = RawValue::from_string(
            serde_json::json!([[[vec![call; 5], serde_json::Value::Null]]]).to_string(),
        )
        .unwrap();
        let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
        assert_call_many_rejected(
            call_many_middleware(2).call(request).await,
            &["call count 5", "2 allowed per request"],
        );
    }

    #[tokio::test]
    async fn test_call_many_array_form_bundle_with_override_is_measured() {
        // The same shape carrying a real block override, not a null placeholder.
        let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
        let params = RawValue::from_string(
            serde_json::json!([[[vec![call; 4], {"number": "0x1"}]]]).to_string(),
        )
        .unwrap();
        let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
        assert_call_many_rejected(
            call_many_middleware(2).call(request).await,
            &["call count 4"],
        );
    }

    #[tokio::test]
    async fn test_call_many_trailing_params_ignored() {
        // jsonrpsee stops reading after the declared args, so extra positional
        // params must not make the cap give up and forward.
        let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
        let params = RawValue::from_string(
            serde_json::json!([
                [{"transactions": vec![call; 3]}],
                serde_json::Value::Null,
                serde_json::Value::Null,
                "unexpected extra"
            ])
            .to_string(),
        )
        .unwrap();
        let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
        assert_call_many_rejected(
            call_many_middleware(1).call(request).await,
            &["call count 3"],
        );
    }

    #[tokio::test]
    async fn test_call_many_unreadable_params_rejected() {
        // Fail closed: the cap's shape is laxer than the handler's, so params it
        // cannot read are params the handler would reject anyway.
        let params = RawValue::from_string(r#"["not-a-bundle-list"]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
        assert_call_many_rejected(
            call_many_middleware(1).call(request).await,
            &["could not be read"],
        );
    }

    #[tokio::test]
    async fn test_call_many_state_overrides_counted() {
        let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
        let overrides: serde_json::Map<String, serde_json::Value> = (0..4u8)
            .map(|i| {
                (
                    format!("0x00000000000000000000000000000000000000{i:02x}"),
                    serde_json::json!({"balance": "0x1"}),
                )
            })
            .collect();
        let params = RawValue::from_string(
            serde_json::json!([
                [{"transactions": [call]}],
                serde_json::Value::Null,
                overrides
            ])
            .to_string(),
        )
        .unwrap();
        let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
        let middleware = CallManyLimitMiddleware::new(MockRpcService, limits_with_overrides(10, 3));
        assert_call_many_rejected(
            middleware.call(request).await,
            &[
                "overrides 4 accounts",
                "limit of 3",
                "--arc.rpc.max-call-many-state-overrides",
            ],
        );
    }

    #[test]
    fn test_call_many_bundle_accepts_every_handler_shape() {
        // The cap parses alloy's own `Bundle`, so its arity tracks the
        // handler's by construction. What still needs pinning is that the
        // transaction type stays permissive: the cap counts calls, the handler
        // validates them, and a bundle the handler accepts must never fail here.
        let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
        let shapes = [
            serde_json::json!([{"transactions": [call.clone()]}]),
            serde_json::json!([{"transactions": [call.clone()], "blockOverride": {"number": "0x1"}}]),
            serde_json::json!([[[call.clone()], serde_json::Value::Null]]),
            serde_json::json!([[[call.clone()], {"number": "0x1"}]]),
            serde_json::json!([[[call.clone()]]]),
            serde_json::json!([{"transactions": []}]),
            // A call body the handler would reject still has to be counted,
            // so the handler can return its own error.
            serde_json::json!([{"transactions": [{"to": "not-an-address"}]}]),
        ];
        for shape in shapes {
            let json = shape.to_string();
            let ours: CallManyBundles = serde_json::from_str(&json)
                .unwrap_or_else(|err| panic!("cap must read {json}: {err}"));
            let counted: usize = ours.iter().map(|b| b.transactions.len()).sum();
            if let Ok(alloy) = serde_json::from_str::<Vec<alloy_rpc_types_eth::Bundle>>(&json) {
                let expected: usize = alloy.iter().map(|b| b.transactions.len()).sum();
                assert_eq!(
                    counted, expected,
                    "cap must count every call the handler would run, for {json}"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_call_many_state_overrides_named_params_counted() {
        // jsonrpsee binds each argument under both its snake_case and
        // lowerCamelCase spelling, so a caller may send either. Missing one
        // would leave the override cap silently inert for that caller.
        for key in ["state_override", "stateOverride"] {
            let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
            let overrides: serde_json::Map<String, serde_json::Value> = (0..4u8)
                .map(|i| {
                    (
                        format!("0x00000000000000000000000000000000000000{i:02x}"),
                        serde_json::json!({"balance": "0x1"}),
                    )
                })
                .collect();
            let params = RawValue::from_string(
                serde_json::json!({
                    "bundles": [{"transactions": [call]}],
                    key: overrides,
                })
                .to_string(),
            )
            .unwrap();
            let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
            let middleware =
                CallManyLimitMiddleware::new(MockRpcService, limits_with_overrides(10, 3));
            assert_call_many_rejected(
                middleware.call(request).await,
                &["overrides 4 accounts", "limit of 3"],
            );
        }
    }

    #[tokio::test]
    async fn test_call_many_state_overrides_at_limit_pass() {
        // Exactly at the cap must pass, so an off-by-one cannot start rejecting
        // requests that sit on the operator's configured value.
        let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
        let overrides: serde_json::Map<String, serde_json::Value> = (0..3u8)
            .map(|i| {
                (
                    format!("0x00000000000000000000000000000000000000{i:02x}"),
                    serde_json::json!({"balance": "0x1"}),
                )
            })
            .collect();
        let params = RawValue::from_string(
            serde_json::json!([
                [{"transactions": [call]}],
                serde_json::Value::Null,
                overrides
            ])
            .to_string(),
        )
        .unwrap();
        let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
        let middleware = CallManyLimitMiddleware::new(MockRpcService, limits_with_overrides(10, 3));
        assert_eq!(
            result_of(middleware.call(request).await),
            serde_json::json!("success")
        );
    }

    #[tokio::test]
    async fn test_call_many_zero_call_request_costs_the_minimum() {
        // Reth resolves the target block and opens a read transaction even when
        // every bundle is empty, so a zero-call request is not free. Three of
        // them cannot fit a budget of two.
        let batch = Batch::from(vec![
            Ok(call_many_entry(&[0], 1)),
            Ok(call_many_entry(&[0], 2)),
            Ok(call_many_entry(&[0], 3)),
        ]);
        assert_call_many_rejected(
            call_many_middleware(2).batch(batch).await,
            &["call count 1", "0 calls remaining"],
        );
    }

    #[tokio::test]
    async fn test_call_many_replay_only_request_is_charged() {
        // An empty bundle with an out-of-range transactionIndex asks reth to
        // replay the whole target block. It must not be free.
        let params = RawValue::from_string(
            serde_json::json!([
                [{"transactions": []}],
                {"blockNumber": "0x1", "transactionIndex": 999999}
            ])
            .to_string(),
        )
        .unwrap();
        let batch = Batch::from(vec![
            Ok(call_many_entry(&[1], 1)),
            Ok(BatchEntry::Call(create_request_with_params(
                ETH_CALL_MANY_METHOD,
                params,
                2,
            ))),
        ]);
        assert_call_many_rejected(
            call_many_middleware(1).batch(batch).await,
            &["call count 1", "0 calls remaining"],
        );
    }

    #[tokio::test]
    async fn test_call_many_state_overrides_under_limit_pass() {
        let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
        let params = RawValue::from_string(
            serde_json::json!([
                [{"transactions": [call]}],
                serde_json::Value::Null,
                {"0x0000000000000000000000000000000000000001": {"balance": "0x1"}}
            ])
            .to_string(),
        )
        .unwrap();
        let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
        let middleware = CallManyLimitMiddleware::new(MockRpcService, limits_with_overrides(10, 3));
        assert_eq!(
            result_of(middleware.call(request).await),
            serde_json::json!("success")
        );
    }

    #[tokio::test]
    async fn test_call_many_cap_ignores_other_methods() {
        // eth_call carries a single call and must never be measured against the cap.
        let params = RawValue::from_string(
            serde_json::json!([{"transactions": [{}, {}, {}]}, "latest"]).to_string(),
        )
        .unwrap();
        let request = create_request_with_params("eth_call", params, 1);
        let response = call_many_middleware(1).call(request).await;
        assert_eq!(result_of(response), serde_json::json!("success"));
    }

    #[tokio::test]
    async fn test_call_many_oversized_entry_rejects_the_whole_batch() {
        // Rejecting only the offending entry would make the verdict depend on
        // entry order and would still pay for whatever ran before it, so the
        // batch is refused as a unit, like an oversized batch one layer out.
        let batch = Batch::from(vec![
            Ok(call_many_entry(&[5], 1)),
            Ok(make_call_entry("eth_blockNumber", 2)),
        ]);
        let response = call_many_middleware(2).batch(batch).await;
        assert_eq!(
            response.as_error_code(),
            Some(CALL_MANY_REJECTED_ERROR_CODE)
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert!(json["id"].is_null(), "batch rejection should use Id::Null");
        let message = json["error"]["message"].as_str().unwrap_or("");
        assert!(
            message.contains("call count 5") && message.contains("2 calls remaining"),
            "error should name the offending entry and the budget; got: {message}"
        );
    }

    #[tokio::test]
    async fn test_call_many_budget_is_shared_across_a_batch() {
        // Each entry fits the limit on its own; together they do not, and
        // batching must not multiply the budget by the entry count.
        let batch = Batch::from(vec![
            Ok(call_many_entry(&[4], 1)),
            Ok(call_many_entry(&[4], 2)),
            Ok(call_many_entry(&[4], 3)),
        ]);
        assert_call_many_rejected(
            call_many_middleware(10).batch(batch).await,
            &[
                "call count 4",
                "2 calls remaining",
                "10 allowed per request",
            ],
        );
    }

    #[tokio::test]
    async fn test_call_many_batch_within_budget_passes_through() {
        // The complement: a batch whose entries sum to the limit is forwarded
        // intact, so the cap cannot be satisfied by rejecting everything.
        let batch = Batch::from(vec![
            Ok(call_many_entry(&[4], 1)),
            Ok(call_many_entry(&[4], 2)),
            Ok(make_call_entry("eth_blockNumber", 3)),
        ]);
        let response = call_many_middleware(8).batch(batch).await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(responses.len(), 3);
        for entry in &responses {
            assert_eq!(entry["result"], "success");
        }
    }

    #[test]
    fn test_call_many_rejection_reasons_are_stable() {
        // These strings are a Prometheus label vocabulary; renaming one splits
        // a dashboard's series in two without breaking anything else.
        assert_eq!(CallManyRejection::CallCount.as_str(), "call_count");
        assert_eq!(
            CallManyRejection::StateOverrides.as_str(),
            "state_overrides"
        );
        assert_eq!(
            CallManyRejection::UnreadableParams.as_str(),
            "unreadable_params"
        );
    }

    #[tokio::test]
    async fn test_batch_size_limit_runs_before_the_call_many_budget() {
        // Ordering matters: an oversized batch must be turned away by entry
        // count before anything parses its params. Swapping the two layers
        // would answer -32602 here instead of -32600.
        let layer = ArcRpcLayer {
            max_batch_entries: nz(2),
            call_many_limits: limits(100),
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let batch = Batch::from(vec![
            Ok(call_many_entry(&[1], 1)),
            Ok(call_many_entry(&[1], 2)),
            Ok(call_many_entry(&[1], 3)),
        ]);
        assert_eq!(
            middleware.batch(batch).await.as_error_code(),
            Some(BATCH_TOO_LARGE_ERROR_CODE)
        );
    }

    #[tokio::test]
    async fn test_call_many_notification_is_skipped_by_the_pre_pass() {
        // The pre-pass only measures `Ok(BatchEntry::Call(_))`, so a
        // notification-form eth_callMany does not spend budget and cannot make
        // the batch fail. That is safe because jsonrpsee answers a
        // notification without dispatching it to a handler, so it executes
        // nothing — if that ever changes, this becomes an unmetered path.
        let batch = Batch::from(vec![
            Ok(BatchEntry::Notification(Notification::new(
                ETH_CALL_MANY_METHOD.into(),
                Some(Cow::Owned(call_many_params(&[500]))),
            ))),
            Ok(make_call_entry("eth_chainId", 1)),
        ]);
        let response = call_many_middleware(1).batch(batch).await;
        assert_ne!(
            response.as_error_code(),
            Some(CALL_MANY_REJECTED_ERROR_CODE),
            "a notification carries no budget, so it must not fail the batch"
        );
    }

    #[tokio::test]
    async fn test_call_many_duplicate_override_spellings_rejected() {
        // Both spellings bind the same argument, so reth calls it a duplicate
        // field. The cap must not pick one and undercount.
        let call = serde_json::json!({"to": "0x0000000000000000000000000000000000000001"});
        let params = RawValue::from_string(
            serde_json::json!({
                "bundles": [{"transactions": [call]}],
                "state_override": {"0x0000000000000000000000000000000000000001": {"balance": "0x1"}},
                "stateOverride": {"0x0000000000000000000000000000000000000002": {"balance": "0x1"}},
            })
            .to_string(),
        )
        .unwrap();
        let request = create_request_with_params(ETH_CALL_MANY_METHOD, params, 1);
        assert_call_many_rejected(
            call_many_middleware(10).call(request).await,
            &["could not be read"],
        );
    }

    #[test]
    fn test_arc_rpc_layer_default_call_many_limits() {
        let layer = ArcRpcLayer::default();
        assert_eq!(
            layer.call_many_limits.max_calls,
            ARC_RPC_MAX_CALL_MANY_CALLS_DEFAULT
        );
        assert_eq!(
            layer.call_many_limits.max_state_overrides,
            ARC_RPC_MAX_CALL_MANY_STATE_OVERRIDES_DEFAULT
        );
    }

    #[tokio::test]
    async fn test_call_many_cap_applies_through_layer() {
        // The cap must survive composition into the full ArcRpcLayer stack.
        let layer = ArcRpcLayer {
            call_many_limits: limits(2),
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        assert_call_many_rejected(
            middleware.call(call_many_request(&[3])).await,
            &["call count 3", "2 allowed per request"],
        );
    }

    #[tokio::test]
    async fn test_call_many_notifications_never_reach_the_handler() {
        // The budget skips notifications because jsonrpsee answers one without
        // dispatching it to a handler. Run the real server so that a jsonrpsee
        // upgrade that starts dispatching them fails here instead of opening
        // an unmetered path.
        let executed = Arc::new(AtomicUsize::new(0));
        let mut module = jsonrpsee::RpcModule::new(Arc::clone(&executed));
        module
            .register_method(ETH_CALL_MANY_METHOD, |_, executed, _| {
                executed.fetch_add(1, Ordering::SeqCst);
                "ok"
            })
            .unwrap();
        let layer = ArcRpcLayer {
            call_many_limits: limits(1),
            ..Default::default()
        };
        let server = jsonrpsee::server::Server::builder()
            .set_rpc_middleware(
                jsonrpsee::server::middleware::rpc::RpcServiceBuilder::new().layer(layer),
            )
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", server.local_addr().unwrap());
        let handle = server.start(module);

        let params = |calls: usize| -> serde_json::Value {
            serde_json::from_str(call_many_params(&[calls]).get()).unwrap()
        };
        let over_budget_notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": ETH_CALL_MANY_METHOD,
            "params": params(5),
        });
        let in_budget_call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": ETH_CALL_MANY_METHOD,
            "params": params(1),
        });
        let client = reqwest::Client::new();
        let post = |body: serde_json::Value| {
            client
                .post(&url)
                .header("content-type", "application/json")
                .body(body.to_string())
                .send()
        };

        post(over_budget_notification.clone()).await.unwrap();
        assert_eq!(
            executed.load(Ordering::SeqCst),
            0,
            "a standalone notification must not execute eth_callMany"
        );

        let body = post(serde_json::json!([
            over_budget_notification,
            in_budget_call
        ]))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
        let response: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            response[0]["result"], "ok",
            "the in-budget call should run; got {response}"
        );
        assert_eq!(
            executed.load(Ordering::SeqCst),
            1,
            "only the batch's call entry should execute, not its notification"
        );

        handle.stop().unwrap();
    }

    // ── RpcGasCapMiddleware ─────────────────────────────────

    const TEST_GAS_CAP: u64 = 30_000_000;
    const AT_CAP_HEX: &str = "0x1c9c380";
    const ABOVE_CAP_HEX: &str = "0x1c9c381";

    /// Builds a params frame carrying a gas limit of the given hex.
    type GasLimitShape = fn(&str) -> String;

    /// Every argument position, in each params form, where a handler runs a gas
    /// limit that Reth does not cap.
    const GAS_LIMIT_SHAPES: [(&str, &str, GasLimitShape); 13] = [
        (
            "eth_createAccessList positional",
            ETH_CREATE_ACCESS_LIST_METHOD,
            |gas| format!(r#"[{{"gas":"{gas}"}},"latest"]"#),
        ),
        (
            "eth_createAccessList named",
            ETH_CREATE_ACCESS_LIST_METHOD,
            |gas| format!(r#"{{"request":{{"gas":"{gas}"}}}}"#),
        ),
        ("eth_call positional", ETH_CALL_METHOD, |gas| {
            format!(r#"[{{}},"latest",null,{{"gasLimit":"{gas}"}}]"#)
        }),
        ("eth_call named", ETH_CALL_METHOD, |gas| {
            format!(r#"{{"request":{{}},"block_overrides":{{"gasLimit":"{gas}"}}}}"#)
        }),
        ("eth_call named camelCase", ETH_CALL_METHOD, |gas| {
            format!(r#"{{"request":{{}},"blockOverrides":{{"gasLimit":"{gas}"}}}}"#)
        }),
        ("trace_call positional", TRACE_CALL_METHOD, |gas| {
            format!(r#"[{{}},["trace"],"latest",null,{{"gasLimit":"{gas}"}}]"#)
        }),
        ("trace_call named", TRACE_CALL_METHOD, |gas| {
            format!(
                r#"{{"call":{{}},"trace_types":["trace"],"block_overrides":{{"gasLimit":"{gas}"}}}}"#
            )
        }),
        (
            "eth_callMany positional, second bundle",
            ETH_CALL_MANY_METHOD,
            |gas| {
                format!(
                    r#"[[{{"transactions":[{{}}]}},{{"transactions":[{{}}],"blockOverride":{{"gasLimit":"{gas}"}}}}]]"#
                )
            },
        ),
        ("eth_callMany named", ETH_CALL_MANY_METHOD, |gas| {
            format!(
                r#"{{"bundles":[{{"transactions":[{{}}],"blockOverride":{{"gasLimit":"{gas}"}}}}]}}"#
            )
        }),
        (
            "debug_traceCall positional",
            DEBUG_TRACE_CALL_METHOD,
            |gas| format!(r#"[{{}},"latest",{{"blockOverrides":{{"gasLimit":"{gas}"}}}}]"#),
        ),
        ("debug_traceCall named", DEBUG_TRACE_CALL_METHOD, |gas| {
            format!(r#"{{"request":{{}},"opts":{{"blockOverrides":{{"gasLimit":"{gas}"}}}}}}"#)
        }),
        (
            "debug_traceCallMany positional",
            DEBUG_TRACE_CALL_MANY_METHOD,
            |gas| {
                format!(
                    r#"[[{{"transactions":[{{}}],"blockOverride":{{"gasLimit":"{gas}"}}}}],null,null]"#
                )
            },
        ),
        (
            "debug_traceCallMany named",
            DEBUG_TRACE_CALL_MANY_METHOD,
            |gas| {
                format!(
                    r#"{{"bundles":[{{"transactions":[{{}}],"blockOverride":{{"gasLimit":"{gas}"}}}}]}}"#
                )
            },
        ),
    ];

    fn gas_cap_middleware(rpc_gas_cap: u64) -> RpcGasCapMiddleware<MockRpcService> {
        RpcGasCapMiddleware::new(MockRpcService, rpc_gas_cap)
    }

    fn raw_request(method: &str, params: &str, id: u64) -> Request<'static> {
        create_request_with_params(
            method,
            RawValue::from_string(params.to_string()).unwrap(),
            id,
        )
    }

    /// Asserts the guard's rejection, naming the method, the gas limit and the flag.
    fn assert_gas_cap_rejected(response: MethodResponse, method: &str, label: &str) {
        assert_eq!(
            response.as_error_code(),
            Some(RPC_GAS_CAP_REJECTED_ERROR_CODE),
            "{label}: a gas limit above the cap must be rejected"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        let message = json["error"]["message"].as_str().unwrap_or("").to_string();
        for fragment in [method, "30000001", "30000000", "--rpc.gascap"] {
            assert!(
                message.contains(fragment),
                "{label}: error message should contain {fragment:?}; got: {message}"
            );
        }
    }

    #[tokio::test]
    async fn test_rpc_gas_cap_above_cap_rejected_in_every_shape() {
        for (label, method, build_params) in GAS_LIMIT_SHAPES {
            let request = raw_request(method, &build_params(ABOVE_CAP_HEX), 1);
            let response = gas_cap_middleware(TEST_GAS_CAP).call(request).await;
            assert_gas_cap_rejected(response, method, label);
        }
    }

    #[tokio::test]
    async fn test_rpc_gas_cap_at_cap_passes_in_every_shape() {
        for (label, method, build_params) in GAS_LIMIT_SHAPES {
            let request = raw_request(method, &build_params(AT_CAP_HEX), 1);
            let response = gas_cap_middleware(TEST_GAS_CAP).call(request).await;
            assert_eq!(
                result_of(response),
                serde_json::json!("success"),
                "{label}: a gas limit at the cap must reach the handler"
            );
        }
    }

    #[tokio::test]
    async fn test_rpc_gas_cap_check_disabled_by_zero_cap() {
        for (label, method, build_params) in GAS_LIMIT_SHAPES {
            let request = raw_request(method, &build_params(ABOVE_CAP_HEX), 1);
            let response = gas_cap_middleware(0).call(request).await;
            assert_eq!(
                result_of(response),
                serde_json::json!("success"),
                "{label}: a zero cap means Reth is uncapped, so the guard must not apply"
            );
        }
    }

    #[tokio::test]
    async fn test_rpc_gas_cap_ignored_where_the_handler_ignores_it() {
        let cases = [
            (
                "debug_traceCallMany opts",
                DEBUG_TRACE_CALL_MANY_METHOD,
                format!(
                    r#"[[{{"transactions":[{{}}]}}],null,{{"blockOverrides":{{"gasLimit":"{ABOVE_CAP_HEX}"}}}}]"#
                ),
            ),
            (
                "eth_call fifth positional",
                ETH_CALL_METHOD,
                format!(r#"[{{}},"latest",null,null,{{"gasLimit":"{ABOVE_CAP_HEX}"}}]"#),
            ),
            (
                "eth_call key of another method",
                ETH_CALL_METHOD,
                format!(
                    r#"{{"request":{{}},"opts":{{"blockOverrides":{{"gasLimit":"{ABOVE_CAP_HEX}"}}}}}}"#
                ),
            ),
            (
                "method without block overrides",
                "eth_estimateGas",
                format!(r#"[{{}},"latest",null,{{"gasLimit":"{ABOVE_CAP_HEX}"}}]"#),
            ),
            (
                "eth_createAccessList block override",
                ETH_CREATE_ACCESS_LIST_METHOD,
                format!(r#"[{{}},"latest",null,{{"gasLimit":"{ABOVE_CAP_HEX}"}}]"#),
            ),
            (
                "eth_call request gas, which Reth caps itself",
                ETH_CALL_METHOD,
                format!(r#"[{{"gas":"{ABOVE_CAP_HEX}"}},"latest"]"#),
            ),
        ];
        for (label, method, params) in cases {
            let response = gas_cap_middleware(TEST_GAS_CAP)
                .call(raw_request(method, &params, 1))
                .await;
            assert_eq!(
                result_of(response),
                serde_json::json!("success"),
                "{label}: a gas limit the handler caps or never applies must not be rejected"
            );
        }
    }

    #[tokio::test]
    async fn test_rpc_gas_cap_not_hidden_by_unread_malformed_key() {
        // The handler ignores a key its method does not take, so a malformed one
        // must not make the guard give up on the override it does take.
        let params = format!(
            r#"{{"request":{{}},"opts":1,"bundles":"x","block_overrides":{{"gasLimit":"{ABOVE_CAP_HEX}"}}}}"#
        );
        let response = gas_cap_middleware(TEST_GAS_CAP)
            .call(raw_request(ETH_CALL_METHOD, &params, 1))
            .await;
        assert_gas_cap_rejected(response, ETH_CALL_METHOD, "eth_call named");
    }

    #[tokio::test]
    async fn test_rpc_gas_cap_unreadable_params_forwarded() {
        // The guard reads the handler's own types, so the handler rejects these too.
        let cases = [
            (
                "non-hex gasLimit",
                r#"[{},"latest",null,{"gasLimit":"lots"}]"#,
            ),
            (
                "both spellings",
                r#"{"request":{},"block_overrides":{},"blockOverrides":{"gasLimit":"0x1c9c381"}}"#,
            ),
            ("scalar params", r#""eth_call""#),
        ];
        for (label, params) in cases {
            let response = gas_cap_middleware(TEST_GAS_CAP)
                .call(raw_request(ETH_CALL_METHOD, params, 1))
                .await;
            assert_eq!(
                result_of(response),
                serde_json::json!("success"),
                "{label}: unreadable params must reach the handler"
            );
        }
    }

    #[tokio::test]
    async fn test_rpc_gas_cap_rejects_only_the_offending_batch_entry() {
        let over = format!(r#"[{{}},"latest",null,{{"gasLimit":"{ABOVE_CAP_HEX}"}}]"#);
        let under = format!(r#"[{{}},"latest",null,{{"gasLimit":"{AT_CAP_HEX}"}}]"#);
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(raw_request(ETH_CALL_METHOD, &under, 1))),
            Ok(BatchEntry::Call(raw_request(ETH_CALL_METHOD, &over, 2))),
            Ok(make_call_entry("eth_blockNumber", 3)),
        ]);
        let response = gas_cap_middleware(TEST_GAS_CAP).batch(batch).await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["result"], "success");
        assert_eq!(
            responses[1]["error"]["code"],
            RPC_GAS_CAP_REJECTED_ERROR_CODE
        );
        assert_eq!(responses[1]["id"], 2);
        assert_eq!(responses[2]["result"], "success");
    }

    #[tokio::test]
    async fn test_arc_rpc_layer_applies_its_rpc_gas_cap() {
        let layer = ArcRpcLayer {
            rpc_gas_cap: TEST_GAS_CAP,
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let (label, method, build_params) = GAS_LIMIT_SHAPES[0];
        let response = middleware
            .call(raw_request(method, &build_params(ABOVE_CAP_HEX), 1))
            .await;
        assert_gas_cap_rejected(response, method, label);
    }

    #[tokio::test]
    async fn test_arc_rpc_layer_call_many_budget_still_sees_the_whole_batch() {
        // The gas guard forwards a batch per entry, so it must sit inside the
        // eth_callMany cap: two entries within the budget each exceed it together.
        let layer = ArcRpcLayer {
            call_many_limits: limits(3),
            ..Default::default()
        };
        let response = layer
            .layer(MockRpcService)
            .batch(Batch::from(vec![
                Ok(call_many_entry(&[2], 1)),
                Ok(call_many_entry(&[2], 2)),
            ]))
            .await;
        assert_call_many_rejected(response, &["call count 2", "3 allowed per request"]);
    }

    // ── forward_batch_with_guard ────────────────────────────────────────
    //
    // RejectUnprotectedTxsMiddleware is the layer that decomposes a batch
    // per entry, so it is what exercises the shared helper's arms.

    #[tokio::test]
    async fn test_guarded_batch_excludes_notifications_from_the_response() {
        let batch = Batch::from(vec![
            Ok(make_call_entry("eth_blockNumber", 1)),
            Ok(BatchEntry::Notification(Notification::new(
                "eth_blockNumber".into(),
                None,
            ))),
        ]);
        let response = RejectUnprotectedTxsMiddleware::new(MockRpcService)
            .batch(batch)
            .await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(
            responses.len(),
            1,
            "a notification produces no response entry"
        );
        assert_eq!(responses[0]["result"], "success");
    }

    #[tokio::test]
    async fn test_guarded_batch_of_only_notifications_returns_no_response() {
        let batch = Batch::from(vec![Ok(BatchEntry::Notification(Notification::new(
            "eth_blockNumber".into(),
            None,
        )))]);
        let response = RejectUnprotectedTxsMiddleware::new(MockRpcService)
            .batch(batch)
            .await;
        assert!(
            response.is_notification(),
            "a notifications-only batch must not produce a JSON-RPC response body"
        );
    }

    #[tokio::test]
    async fn test_guarded_batch_preserves_malformed_entry_errors() {
        // A malformed entry keeps its place and its id so responses still line
        // up with the requests that produced them.
        let malformed = BatchEntryErr::new(
            Id::Number(3),
            ErrorObjectOwned::owned::<()>(ErrorCode::InvalidRequest.code(), "bad entry", None),
        );
        let batch = Batch::from(vec![Err(malformed), Ok(make_call_entry("eth_chainId", 7))]);
        let response = RejectUnprotectedTxsMiddleware::new(MockRpcService)
            .batch(batch)
            .await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(responses.len(), 2);
        assert!(responses[0]["error"].is_object());
        assert_eq!(responses[1]["result"], "success");
        assert_eq!(responses[1]["id"], 7);
    }

    // ── RejectUnprotectedTxsMiddleware ──────────────────────────────────

    use alloy_consensus::{SignableTransaction, TxLegacy};
    use alloy_network::eip2718::Encodable2718;
    use alloy_primitives::{hex, Address, Signature, TxKind, U256};

    /// Dummy non-zero signature scalars. Sufficient for envelope decoding; we
    /// never recover the signer or verify the signature in middleware.
    fn dummy_signature() -> Signature {
        Signature::new(U256::from(1u64), U256::from(1u64), false)
    }

    fn legacy_tx(chain_id: Option<u64>) -> TxLegacy {
        TxLegacy {
            chain_id,
            nonce: 0,
            gas_price: 1_000_000_000,
            gas_limit: 21_000,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Default::default(),
        }
    }

    fn encode_legacy_bytes(chain_id: Option<u64>) -> Vec<u8> {
        let tx = legacy_tx(chain_id);
        let signed = tx.into_signed(dummy_signature());
        let envelope: TxEnvelope = signed.into();
        envelope.encoded_2718()
    }

    fn encode_legacy_raw(chain_id: Option<u64>) -> String {
        format!("0x{}", hex::encode(encode_legacy_bytes(chain_id)))
    }

    fn raw_tx_request_with_params(method: &str, params_json: String, id: u64) -> Request<'static> {
        let params = RawValue::from_string(params_json).unwrap();
        create_request_with_params(method, params, id)
    }

    fn send_raw_tx_request_with_params(params_json: String, id: u64) -> Request<'static> {
        raw_tx_request_with_params(ETH_SEND_RAW_TRANSACTION_METHOD, params_json, id)
    }

    fn send_raw_tx_request(raw_hex: &str, id: u64) -> Request<'static> {
        send_raw_tx_request_with_params(format!("[\"{raw_hex}\"]"), id)
    }

    fn send_raw_tx_sync_request(raw_hex: &str, id: u64) -> Request<'static> {
        raw_tx_request_with_params(
            ETH_SEND_RAW_TRANSACTION_SYNC_METHOD,
            format!("[\"{raw_hex}\"]"),
            id,
        )
    }

    fn send_raw_tx_object_request(raw_hex: &str, id: u64) -> Request<'static> {
        send_raw_tx_request_with_params(format!("{{\"bytes\":\"{raw_hex}\"}}"), id)
    }

    /// A submission padded with one unused positional param.
    fn send_raw_tx_padded_request(raw_hex: &str, id: u64) -> Request<'static> {
        send_raw_tx_request_with_params(format!("[\"{raw_hex}\",0]"), id)
    }

    fn send_raw_tx_bytes_request(bytes: &[u8], id: u64) -> Request<'static> {
        let bytes_json = serde_json::to_string(bytes).unwrap();
        send_raw_tx_request_with_params(format!("[{bytes_json}]"), id)
    }

    fn send_raw_tx_object_bytes_request(bytes: &[u8], id: u64) -> Request<'static> {
        let bytes_json = serde_json::to_string(bytes).unwrap();
        send_raw_tx_request_with_params(format!("{{\"bytes\":{bytes_json}}}"), id)
    }

    fn assert_unprotected_tx_rejected(response: MethodResponse) {
        assert_eq!(
            response.as_error_code(),
            Some(UNPROTECTED_TX_ERROR_CODE),
            "pre-EIP-155 tx must be rejected with UNPROTECTED_TX_ERROR_CODE"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(json["error"]["message"], UNPROTECTED_TX_ERROR_MSG);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_request(&encode_legacy_raw(None), 1);
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction_sync() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_sync_request(&encode_legacy_raw(None), 1);
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction_object_hex_params() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_object_request(&encode_legacy_raw(None), 1);
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction_positional_bytes_params() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let bytes = encode_legacy_bytes(None);
        let request = send_raw_tx_bytes_request(&bytes, 1);
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction_object_bytes_params() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let bytes = encode_legacy_bytes(None);
        let request = send_raw_tx_object_bytes_request(&bytes, 1);
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction_escaped_object_key() {
        // The handler binds the unescaped field name, so the guard must too.
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_request_with_params(
            format!("{{\"byt\\u0065s\":\"{}\"}}", encode_legacy_raw(None)),
            1,
        );
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction_object_surplus_field() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_request_with_params(
            format!(
                "{{\"bytes\":\"{}\",\"junk\":{{\"a\":[1,2,{{\"b\":null}}]}}}}",
                encode_legacy_raw(None)
            ),
            1,
        );
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction_padded_params() {
        // A surplus positional param must not skip the replay-protection check.
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_padded_request(&encode_legacy_raw(None), 1);
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction_sync_padded_params() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = raw_tx_request_with_params(
            ETH_SEND_RAW_TRANSACTION_SYNC_METHOD,
            format!("[\"{}\",0]", encode_legacy_raw(None)),
            1,
        );
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_rejects_pre_eip155_send_raw_transaction_multiple_padded_params() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_request_with_params(
            format!("[\"{}\",0,{{\"a\":1}},null]", encode_legacy_raw(None)),
            1,
        );
        let response = middleware.call(request).await;

        assert_unprotected_tx_rejected(response);
    }

    #[tokio::test]
    async fn test_allows_eip155_send_raw_transaction() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_request(&encode_legacy_raw(Some(1337)), 2);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "EIP-155-protected legacy tx must be forwarded to the inner service"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(json["result"], "success");
    }

    #[tokio::test]
    async fn test_allows_eip155_send_raw_transaction_padded_params() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_padded_request(&encode_legacy_raw(Some(1337)), 2);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "protected tx with a surplus param must be forwarded to the inner service"
        );
    }

    #[tokio::test]
    async fn test_allows_eip155_send_raw_transaction_object_params() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_object_request(&encode_legacy_raw(Some(1337)), 2);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "protected tx in object form must be forwarded to the inner service"
        );
    }

    #[tokio::test]
    async fn test_empty_params_forwarded() {
        // No payload to police; the inner handler returns its own error.
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_request_with_params("[]".to_string(), 3);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "empty params must be forwarded to inner handler"
        );
    }

    #[tokio::test]
    async fn test_malformed_raw_tx_forwarded() {
        // Bad hex / undecodable RLP must reach the inner handler so it can
        // return its own precise error — middleware must not pre-empt.
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let request = send_raw_tx_request("0xdeadbeef", 4);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "undecodable raw tx must be forwarded to inner handler"
        );
    }

    #[tokio::test]
    async fn test_raw_tx_with_trailing_bytes_forwarded() {
        // Trailing bytes make the payload malformed. The middleware must not
        // convert that into its replay-protection error.
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let mut bytes = encode_legacy_bytes(None);
        bytes.push(0);
        let raw_hex = format!("0x{}", hex::encode(bytes));
        let request = send_raw_tx_request(&raw_hex, 5);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "raw tx with trailing bytes must be forwarded to inner handler"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(json["result"], "success");
    }

    #[tokio::test]
    async fn test_batch_partial_reject() {
        // A mixed batch: only the unprotected entry must be rejected; the
        // protected and unrelated entries must reach the inner handler.
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let unprotected = send_raw_tx_request(&encode_legacy_raw(None), 1);
        let protected = send_raw_tx_request(&encode_legacy_raw(Some(1337)), 2);
        let other = create_request_with_params(
            "eth_blockNumber",
            RawValue::from_string("[]".to_string()).unwrap(),
            3,
        );
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(unprotected)),
            Ok(BatchEntry::Call(protected)),
            Ok(BatchEntry::Call(other)),
        ]);
        let response = middleware.batch(batch).await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();

        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["error"]["code"], UNPROTECTED_TX_ERROR_CODE);
        assert_eq!(responses[0]["error"]["message"], UNPROTECTED_TX_ERROR_MSG);
        assert_eq!(responses[1]["result"], "success");
        assert_eq!(responses[2]["result"], "success");
    }

    #[tokio::test]
    async fn test_batch_partial_rejects_send_raw_transaction_sync() {
        let middleware = RejectUnprotectedTxsMiddleware::new(MockRpcService);
        let unprotected = send_raw_tx_sync_request(&encode_legacy_raw(None), 1);
        let other = create_request_with_params(
            "eth_blockNumber",
            RawValue::from_string("[]".to_string()).unwrap(),
            2,
        );
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(unprotected)),
            Ok(BatchEntry::Call(other)),
        ]);
        let response = middleware.batch(batch).await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();

        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["error"]["code"], UNPROTECTED_TX_ERROR_CODE);
        assert_eq!(responses[0]["error"]["message"], UNPROTECTED_TX_ERROR_MSG);
        assert_eq!(responses[1]["result"], "success");
    }

    #[tokio::test]
    async fn test_default_layer_rejects_unprotected_and_pending_entries_in_batch() {
        let layer = ArcRpcLayer::default();
        let middleware = layer.layer(MockRpcService);
        let unprotected = send_raw_tx_request(&encode_legacy_raw(None), 1);
        let pending_subscription = create_request_with_params(
            ETH_SUBSCRIBE_METHOD,
            RawValue::from_string(r#"["newPendingTransactions"]"#.to_string()).unwrap(),
            2,
        );
        let other = create_request_with_params(
            "eth_blockNumber",
            RawValue::from_string("[]".to_string()).unwrap(),
            3,
        );
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(unprotected)),
            Ok(BatchEntry::Call(pending_subscription)),
            Ok(BatchEntry::Call(other)),
        ]);
        let response = middleware.batch(batch).await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();

        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["error"]["code"], UNPROTECTED_TX_ERROR_CODE);
        assert_eq!(responses[0]["error"]["message"], UNPROTECTED_TX_ERROR_MSG);
        assert_eq!(
            responses[1]["error"]["code"],
            PENDING_TX_SUBSCRIPTION_ERROR_CODE
        );
        assert_eq!(responses[2]["result"], "success");
    }

    #[tokio::test]
    async fn test_flag_off_via_layer_allows_unprotected() {
        // With --arc.rpc.allow-unprotected-txs, the rejecting middleware is
        // bypassed entirely and pre-EIP-155 txs reach the inner service.
        let layer = ArcRpcLayer {
            allow_unprotected_txs: true,
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let request = send_raw_tx_request(&encode_legacy_raw(None), 10);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "allow_unprotected_txs=true must let pre-EIP-155 txs through"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(json["result"], "success");
    }

    #[tokio::test]
    async fn test_flag_off_via_layer_allows_unprotected_in_batch() {
        // The allow flag is checked inside the shared batch guard, so the batch
        // path needs its own coverage: an inverted check there would pass every
        // single-call test while still rejecting batched submissions.
        let layer = ArcRpcLayer {
            allow_unprotected_txs: true,
            ..Default::default()
        };
        let middleware = layer.layer(MockRpcService);
        let batch = Batch::from(vec![
            Ok(BatchEntry::Call(send_raw_tx_request(
                &encode_legacy_raw(None),
                10,
            ))),
            Ok(make_call_entry("eth_chainId", 11)),
        ]);
        let response = middleware.batch(batch).await;
        let responses: Vec<serde_json::Value> =
            serde_json::from_str(response.into_json().get()).unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[0]["result"], "success",
            "allow_unprotected_txs=true must let pre-EIP-155 txs through in a batch"
        );
        assert_eq!(responses[1]["result"], "success");
    }

    // ── Case-insensitive "pending" tag + additional block-content methods ──
    //
    // All six block-content methods accept a block number/tag as first param.
    // When that tag is "pending" (any case) the middleware returns null.

    #[tokio::test]
    async fn test_pending_block_methods_all_tags_return_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let cases = [
            (ETH_GET_BLOCK_BY_NUMBER_METHOD, "pending"),
            (ETH_GET_BLOCK_BY_NUMBER_METHOD, "Pending"),
            (ETH_GET_BLOCK_BY_NUMBER_METHOD, "PENDING"),
            (ETH_GET_BLOCK_RECEIPTS_METHOD, "pending"),
            (ETH_GET_BLOCK_RECEIPTS_METHOD, "Pending"),
            (ETH_GET_BLOCK_RECEIPTS_METHOD, "PENDING"),
            (ETH_GET_BLOCK_TX_COUNT_BY_NUMBER_METHOD, "pending"),
            (ETH_GET_BLOCK_TX_COUNT_BY_NUMBER_METHOD, "Pending"),
            (ETH_GET_BLOCK_TX_COUNT_BY_NUMBER_METHOD, "PENDING"),
            (ETH_GET_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD, "pending"),
            (ETH_GET_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD, "Pending"),
            (ETH_GET_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD, "PENDING"),
            (ETH_GET_RAW_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD, "pending"),
            (ETH_GET_RAW_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD, "Pending"),
            (ETH_GET_RAW_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD, "PENDING"),
            (ETH_GET_UNCLE_COUNT_BY_BLOCK_NUMBER_METHOD, "pending"),
            (ETH_GET_UNCLE_COUNT_BY_BLOCK_NUMBER_METHOD, "Pending"),
            (ETH_GET_UNCLE_COUNT_BY_BLOCK_NUMBER_METHOD, "PENDING"),
            (ETH_GET_HEADER_BY_NUMBER_METHOD, "pending"),
            (ETH_GET_HEADER_BY_NUMBER_METHOD, "Pending"),
            (ETH_GET_HEADER_BY_NUMBER_METHOD, "PENDING"),
        ];
        for (method, tag) in cases {
            let params = RawValue::from_string(format!("[\"{tag}\"]")).unwrap();
            let request = create_request_with_params(method, params, 1);
            let response = middleware.call(request).await;
            assert!(
                response.as_error_code().is_none(),
                "{method} with \"{tag}\" should not error"
            );
            let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
            assert!(
                json["result"].is_null(),
                "{method} with \"{tag}\" should return null"
            );
        }
    }

    #[tokio::test]
    async fn test_additional_pending_block_methods_non_pending_tag_passes_through() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let cases: &[(&str, &str)] = &[
            (ETH_GET_BLOCK_RECEIPTS_METHOD, r#"["latest"]"#),
            (ETH_GET_BLOCK_TX_COUNT_BY_NUMBER_METHOD, r#"["latest"]"#),
            (
                ETH_GET_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD,
                r#"["0x1", "0x0"]"#,
            ),
            (
                ETH_GET_RAW_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD,
                r#"["0x1", "0x0"]"#,
            ),
            (ETH_GET_UNCLE_COUNT_BY_BLOCK_NUMBER_METHOD, r#"["latest"]"#),
            (ETH_GET_HEADER_BY_NUMBER_METHOD, r#"["latest"]"#),
        ];
        for (method, params_json) in cases {
            let params = RawValue::from_string(params_json.to_string()).unwrap();
            let request = create_request_with_params(method, params, 1);
            let response = middleware.call(request).await;

            assert!(
                response.as_error_code().is_none(),
                "{method} with non-pending tag should pass through"
            );
            let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
            assert_eq!(
                json["result"], "success",
                "{method} with non-pending tag should return inner service response"
            );
        }
    }

    // ── Object-style params bypass ──────────────────────────────────────
    //
    // jsonrpsee handlers accept both array and object params. The middleware
    // must intercept object-style pending-block queries too.

    #[tokio::test]
    async fn test_object_params_pending_block_returns_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let cases: &[(&str, &str)] = &[
            // Five methods use "number" key
            (
                ETH_GET_BLOCK_BY_NUMBER_METHOD,
                r#"{"number": "pending", "full": false}"#,
            ),
            (
                ETH_GET_BLOCK_TX_COUNT_BY_NUMBER_METHOD,
                r#"{"number": "pending"}"#,
            ),
            (
                ETH_GET_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD,
                r#"{"number": "pending", "index": "0x0"}"#,
            ),
            (
                ETH_GET_RAW_TX_BY_BLOCK_NUMBER_AND_INDEX_METHOD,
                r#"{"number": "pending", "index": "0x0"}"#,
            ),
            (
                ETH_GET_UNCLE_COUNT_BY_BLOCK_NUMBER_METHOD,
                r#"{"number": "pending"}"#,
            ),
            // eth_getBlockReceipts uses "block_id" (snake) or "blockId" (camel)
            (ETH_GET_BLOCK_RECEIPTS_METHOD, r#"{"block_id": "pending"}"#),
            (ETH_GET_BLOCK_RECEIPTS_METHOD, r#"{"blockId": "pending"}"#),
            // eth_getHeaderByNumber uses "hash" (Reth's proc-macro arg name)
            (ETH_GET_HEADER_BY_NUMBER_METHOD, r#"{"hash": "pending"}"#),
            // Case-insensitive variant via object params — guards that BlockNumberOrTag deserialization lowercases tags
            (
                ETH_GET_BLOCK_BY_NUMBER_METHOD,
                r#"{"number": "PENDING", "full": false}"#,
            ),
        ];
        for (method, params_json) in cases {
            let params = RawValue::from_string(params_json.to_string()).unwrap();
            let request = create_request_with_params(method, params, 1);
            let response = middleware.call(request).await;
            assert!(
                response.as_error_code().is_none(),
                "{method} with object params should not error"
            );
            let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
            assert!(
                json["result"].is_null(),
                "{method} with object params {{...\"pending\"...}} should return null"
            );
        }
    }

    #[tokio::test]
    async fn test_object_params_non_pending_passes_through() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let cases: &[(&str, &str)] = &[
            (
                ETH_GET_BLOCK_BY_NUMBER_METHOD,
                r#"{"number": "0x1", "full": false}"#,
            ),
            (ETH_GET_BLOCK_RECEIPTS_METHOD, r#"{"block_id": "latest"}"#),
            (ETH_GET_HEADER_BY_NUMBER_METHOD, r#"{"hash": "latest"}"#),
            // Unknown field → passes through (Reth returns parse error, not middleware)
            (ETH_GET_BLOCK_BY_NUMBER_METHOD, r#"{"garbage": "pending"}"#),
        ];
        for (method, params_json) in cases {
            let params = RawValue::from_string(params_json.to_string()).unwrap();
            let request = create_request_with_params(method, params, 1);
            let response = middleware.call(request).await;
            assert!(
                response.as_error_code().is_none(),
                "{method} with non-pending object params should pass through"
            );
            let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
            assert_eq!(
                json["result"], "success",
                "{method} with non-pending object params should reach inner service"
            );
        }
    }

    // ── EIP-1898 BlockId object form ────────────────────────────────────
    //
    // eth_getBlockReceipts accepts BlockId, which allows an EIP-1898 object
    // form in addition to the plain string form.  {"blockNumber": "pending"}
    // deserializes to BlockId::Number(Pending) and must also be intercepted.

    #[tokio::test]
    async fn test_eip1898_block_receipts_pending_returns_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let cases: &[&str] = &[
            // Array-style: element is an EIP-1898 object
            r#"[{"blockNumber": "pending"}]"#,
            // Named params (snake_case key) with EIP-1898 value
            r#"{"block_id": {"blockNumber": "pending"}}"#,
            // Named params (camelCase key) with EIP-1898 value
            r#"{"blockId": {"blockNumber": "pending"}}"#,
        ];
        for params_json in cases {
            let params = RawValue::from_string(params_json.to_string()).unwrap();
            let request = create_request_with_params(ETH_GET_BLOCK_RECEIPTS_METHOD, params, 1);
            let response = middleware.call(request).await;
            assert!(
                response.as_error_code().is_none(),
                "eth_getBlockReceipts EIP-1898 pending should not error"
            );
            let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
            assert!(
                json["result"].is_null(),
                "eth_getBlockReceipts with EIP-1898 pending ({params_json}) should return null"
            );
        }
    }

    #[tokio::test]
    async fn test_eip1898_block_receipts_non_pending_passes_through() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let cases: &[&str] = &[
            r#"[{"blockNumber": "0x1"}]"#,
            r#"{"block_id": {"blockNumber": "0x1"}}"#,
        ];
        for params_json in cases {
            let params = RawValue::from_string(params_json.to_string()).unwrap();
            let request = create_request_with_params(ETH_GET_BLOCK_RECEIPTS_METHOD, params, 1);
            let response = middleware.call(request).await;
            assert!(
                response.as_error_code().is_none(),
                "eth_getBlockReceipts EIP-1898 non-pending should pass through"
            );
            let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
            assert_eq!(
                json["result"], "success",
                "eth_getBlockReceipts with EIP-1898 non-pending ({params_json}) should reach inner service"
            );
        }
    }

    // ── Transaction relay middleware ────────────────────────────────────

    /// Inner service that counts `call` invocations, for asserting local pool adds.
    #[derive(Clone, Debug)]
    struct CountingRpcService(Arc<AtomicUsize>);

    impl RpcServiceT for CountingRpcService {
        type MethodResponse = MethodResponse;
        type NotificationResponse = MethodResponse;
        type BatchResponse = MethodResponse;

        #[allow(clippy::manual_async_fn)]
        fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
            let counter = self.0.clone();
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                let payload = ResponsePayload::success(serde_json::Value::String("local".into()));
                MethodResponse::response(req.id(), payload.into(), usize::MAX)
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
            let service = self.clone();
            async move {
                let mut builder = BatchResponseBuilder::new_with_limit(usize::MAX);
                for entry in req {
                    match entry {
                        Ok(BatchEntry::Call(request)) => {
                            service.0.fetch_add(1, Ordering::Relaxed);
                            let payload =
                                ResponsePayload::success(serde_json::Value::String("local".into()));
                            builder
                                .append(MethodResponse::response(
                                    request.id(),
                                    payload.into(),
                                    usize::MAX,
                                ))
                                .unwrap();
                        }
                        Ok(BatchEntry::Notification(n)) => {
                            let _ = service.notification(n).await;
                        }
                        Err(err) => {
                            let (error, id) = err.into_parts();
                            builder.append(MethodResponse::error(id, error)).unwrap();
                        }
                    }
                }
                MethodResponse::from_batch(builder.finish())
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn notification<'a>(
            &self,
            _n: Notification<'a>,
        ) -> impl Future<Output = MethodResponse> + Send + 'a {
            async move { MethodResponse::notification() }
        }
    }

    fn mock_upstream(result: &str) -> RpcClient {
        let asserter = Asserter::new();
        asserter.push_success(&result);
        RpcClient::mocked(asserter)
    }

    fn failing_upstream(msg: &str) -> RpcClient {
        let asserter = Asserter::new();
        asserter.push_failure_msg(msg.to_string());
        RpcClient::mocked(asserter)
    }

    /// Client pointed at a closed port: connecting yields an immediate transport error.
    fn unreachable_upstream() -> RpcClient {
        RpcClient::new_http("http://127.0.0.1:1".parse().unwrap())
    }

    /// Client pointed at a one-shot listener that answers with `status` and an
    /// empty body. `Asserter` can only produce JSON-RPC error payloads, so an
    /// HTTP-status failure needs a real socket.
    async fn status_upstream(status: u16) -> RpcClient {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            // Drain the request before answering, so reqwest sees the response
            // rather than a reset connection.
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status} Status\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
        });
        RpcClient::new_http(url.parse().unwrap())
    }

    /// Client pointed at a listener that accepts the connection and never answers,
    /// so the relay's own request timeout fires.
    async fn silent_upstream() -> RpcClient {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let Ok((_stream, _)) = listener.accept().await else {
                return;
            };
            std::future::pending::<()>().await;
        });
        RpcClient::new_http(url.parse().unwrap())
    }

    fn relays_with(clients: Vec<RpcClient>) -> TxRelays {
        relays_with_timeout(clients, DEFAULT_TX_RELAY_TIMEOUT)
    }

    fn relays_with_timeout(clients: Vec<RpcClient>, request_timeout: Duration) -> TxRelays {
        TxRelays {
            clients: clients.into(),
            sticky: Arc::new(AtomicUsize::new(0)),
            max_response_body_size: usize::MAX,
            request_timeout,
        }
    }

    fn result_of(resp: MethodResponse) -> serde_json::Value {
        let json: serde_json::Value = serde_json::from_str(resp.into_json().get()).unwrap();
        json["result"].clone()
    }

    const RAW_TX_HEX: &str = "0xdeadbeef";

    #[test]
    fn should_failover_classifies_http_status() {
        // Properties of the request/upstream pairing: a peer upstream given the
        // same bytes would answer, so advance. 501 falls in the 5xx range.
        for status in [401, 403, 404, 405, 407, 408, 429, 500, 501, 502, 503] {
            assert!(
                should_failover(&TransportErrorKind::http_error(status, String::new())),
                "HTTP {status} must fail over to the next upstream"
            );
        }
        // Properties of the request itself: no peer would accept it either, so
        // end the pass rather than replay it across every upstream.
        for status in [400, 402, 406, 409, 413, 414, 415, 422, 431] {
            assert!(
                !should_failover(&TransportErrorKind::http_error(status, String::new())),
                "HTTP {status} must end the pass without failover"
            );
        }
    }

    #[tokio::test]
    async fn relay_fails_over_past_upstream_fault_status() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mw = TxRelayMiddleware {
            service: CountingRpcService(counter.clone()),
            relays: relays_with_timeout(
                vec![
                    silent_upstream().await,
                    status_upstream(403).await,
                    mock_upstream("0xthird"),
                ],
                Duration::from_millis(500),
            ),
        };
        let resp = mw.call(send_raw_tx_request(RAW_TX_HEX, 1)).await;
        assert_eq!(
            result_of(resp),
            serde_json::json!("0xthird"),
            "upstream 1 timing out and upstream 2 answering 403 must not strand the \
             submission before upstream 3"
        );
        assert_eq!(
            mw.relays.sticky.load(Ordering::Relaxed),
            2,
            "cursor must name the upstream that delivered"
        );
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn relay_request_level_status_does_not_move_cursor() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mw = TxRelayMiddleware {
            service: CountingRpcService(counter.clone()),
            // Cursor starts at 0; upstream 0 fails over, so the 400 arrives from
            // upstream 1 — an index the cursor would have to move to.
            relays: relays_with(vec![unreachable_upstream(), status_upstream(400).await]),
        };
        let resp = mw.call(send_raw_tx_request(RAW_TX_HEX, 1)).await;
        assert_eq!(resp.as_error_code(), Some(RELAY_REJECTED_ERROR_CODE));
        assert_eq!(
            mw.relays.sticky.load(Ordering::Relaxed),
            0,
            "a non-delivering upstream must not become the last-good cursor"
        );
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "rejected tx must not be added locally"
        );
    }

    #[tokio::test]
    async fn relay_rejection_error_is_distinct_and_host_free() {
        let mw = TxRelayMiddleware {
            service: CountingRpcService(Arc::new(AtomicUsize::new(0))),
            relays: relays_with(vec![status_upstream(400).await]),
        };
        let resp = mw.call(send_raw_tx_request(RAW_TX_HEX, 1)).await;
        assert_eq!(
            resp.as_error_code(),
            Some(RELAY_REJECTED_ERROR_CODE),
            "a rejection must be distinguishable from a delivery failure"
        );
        assert_ne!(RELAY_REJECTED_ERROR_CODE, RELAY_UNAVAILABLE_ERROR_CODE);
        let json: serde_json::Value = serde_json::from_str(resp.into_json().get()).unwrap();
        let message = json["error"]["message"].as_str().unwrap();
        assert_eq!(message, RELAY_REJECTED_ERROR_MSG);
        assert!(
            !message.contains("127.0.0.1") && !message.contains("http"),
            "client-visible message must not carry upstream host details: {message}"
        );
    }

    #[test]
    fn relay_error_category_never_leaks_upstream_url() {
        // Relay URLs can carry API keys in path or query; the logged category
        // must expose only the failure shape, never the URL or response body.
        const SECRET: &str = "relay-secret-sentinel";
        let leaky_body = format!("https://upstream/v3/{SECRET}?api_key={SECRET}");

        let category = relay_error_category(&TransportErrorKind::http_error(503, leaky_body));
        assert_eq!(category, "http status 503");
        assert!(!category.contains(SECRET));
    }

    #[tokio::test]
    async fn relay_forwards_to_upstream_and_adds_locally() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mw = TxRelayMiddleware {
            service: CountingRpcService(counter.clone()),
            relays: relays_with(vec![mock_upstream("0xupstream")]),
        };
        let resp = mw.call(send_raw_tx_request(RAW_TX_HEX, 1)).await;
        assert_eq!(result_of(resp), serde_json::json!("0xupstream"));
        assert_eq!(
            counter.load(Ordering::Relaxed),
            1,
            "tx should be added to local pool"
        );
        assert_eq!(mw.relays.sticky.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn relay_intercepts_sync_method() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mw = TxRelayMiddleware {
            service: CountingRpcService(counter.clone()),
            relays: relays_with(vec![mock_upstream("0xreceipt")]),
        };
        let resp = mw.call(send_raw_tx_sync_request(RAW_TX_HEX, 1)).await;
        assert_eq!(result_of(resp), serde_json::json!("0xreceipt"));
        assert_eq!(
            counter.load(Ordering::Relaxed),
            1,
            "sync submission should still add locally"
        );
    }

    #[tokio::test]
    async fn relay_fails_over_on_transport_error() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mw = TxRelayMiddleware {
            service: CountingRpcService(counter.clone()),
            relays: relays_with(vec![unreachable_upstream(), mock_upstream("0xsecond")]),
        };
        let resp = mw.call(send_raw_tx_request(RAW_TX_HEX, 1)).await;
        assert_eq!(result_of(resp), serde_json::json!("0xsecond"));
        assert_eq!(
            mw.relays.sticky.load(Ordering::Relaxed),
            1,
            "sticky should advance to the good upstream"
        );
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn relay_sticky_starts_at_cursor() {
        let mw = TxRelayMiddleware {
            service: CountingRpcService(Arc::new(AtomicUsize::new(0))),
            relays: relays_with(vec![mock_upstream("0xfirst"), mock_upstream("0xsecond")]),
        };
        mw.relays.sticky.store(1, Ordering::Relaxed);
        let resp = mw.call(send_raw_tx_request(RAW_TX_HEX, 1)).await;
        assert_eq!(
            result_of(resp),
            serde_json::json!("0xsecond"),
            "should start at the sticky upstream, not index 0"
        );
    }

    #[tokio::test]
    async fn relay_advance_wraps_past_end() {
        let mw = TxRelayMiddleware {
            service: CountingRpcService(Arc::new(AtomicUsize::new(0))),
            relays: relays_with(vec![mock_upstream("0xfirst"), unreachable_upstream()]),
        };
        mw.relays.sticky.store(1, Ordering::Relaxed);
        let resp = mw.call(send_raw_tx_request(RAW_TX_HEX, 1)).await;
        assert_eq!(
            result_of(resp),
            serde_json::json!("0xfirst"),
            "advance from last upstream should wrap to index 0"
        );
        assert_eq!(mw.relays.sticky.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn relay_all_unreachable_errors_without_local_add() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mw = TxRelayMiddleware {
            service: CountingRpcService(counter.clone()),
            relays: relays_with(vec![unreachable_upstream(), unreachable_upstream()]),
        };
        let resp = mw.call(send_raw_tx_request(RAW_TX_HEX, 1)).await;
        assert_eq!(resp.as_error_code(), Some(RELAY_UNAVAILABLE_ERROR_CODE));
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "no local add when all upstreams fail"
        );
    }

    #[tokio::test]
    async fn relay_returns_upstream_error_verbatim_without_failover() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mw = TxRelayMiddleware {
            service: CountingRpcService(counter.clone()),
            relays: relays_with(vec![
                failing_upstream("nonce too low"),
                mock_upstream("0xsecond"),
            ]),
        };
        let resp = mw.call(send_raw_tx_request(RAW_TX_HEX, 1)).await;
        assert!(
            resp.as_error_code().is_some(),
            "reachable-upstream error must be returned, not failed over"
        );
        let json: serde_json::Value = serde_json::from_str(resp.into_json().get()).unwrap();
        assert_eq!(json["error"]["message"], "nonce too low");
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "rejected tx must not be added locally"
        );
        assert_eq!(
            mw.relays.sticky.load(Ordering::Relaxed),
            0,
            "must stick to the reachable upstream"
        );
    }

    #[tokio::test]
    async fn relay_extracts_object_form_params() {
        let mw = TxRelayMiddleware {
            service: CountingRpcService(Arc::new(AtomicUsize::new(0))),
            relays: relays_with(vec![mock_upstream("0xok")]),
        };
        let resp = mw.call(send_raw_tx_object_request(RAW_TX_HEX, 1)).await;
        assert_eq!(result_of(resp), serde_json::json!("0xok"));
    }

    #[tokio::test]
    async fn relay_extracts_padded_positional_params() {
        // A surplus param must not silently downgrade the submission to local-only.
        let counter = Arc::new(AtomicUsize::new(0));
        let mw = TxRelayMiddleware {
            service: CountingRpcService(counter.clone()),
            relays: relays_with(vec![mock_upstream("0xok")]),
        };
        let resp = mw.call(send_raw_tx_padded_request(RAW_TX_HEX, 1)).await;
        assert_eq!(result_of(resp), serde_json::json!("0xok"));
        assert_eq!(
            counter.load(Ordering::Relaxed),
            1,
            "relayed tx should still be added locally"
        );
    }

    #[tokio::test]
    async fn relay_passes_through_non_raw_tx_methods() {
        let counter = Arc::new(AtomicUsize::new(0));
        // Upstream is unreachable; a non-relayed method must never touch it.
        let mw = TxRelayMiddleware {
            service: CountingRpcService(counter.clone()),
            relays: relays_with(vec![unreachable_upstream()]),
        };
        let params = RawValue::from_string("[]".to_string()).unwrap();
        let req = create_request_with_params("eth_blockNumber", params, 1);
        let resp = mw.call(req).await;
        assert_eq!(result_of(resp), serde_json::json!("local"));
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn arc_rpc_layer_relays_when_configured() {
        let layer = ArcRpcLayer::new(
            true,
            false,
            usize::MAX,
            ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT,
            CallManyLimits::default(),
            RPC_DEFAULT_GAS_CAP,
            vec!["http://127.0.0.1:1".to_string()],
            DEFAULT_TX_RELAY_TIMEOUT,
        );
        let middleware = layer.layer(MockRpcService);
        // EIP-155-protected tx clears the replay guard; the sole upstream is unreachable.
        let resp = middleware
            .call(send_raw_tx_request(&encode_legacy_raw(Some(1)), 1))
            .await;
        assert_eq!(resp.as_error_code(), Some(RELAY_UNAVAILABLE_ERROR_CODE));
    }

    #[tokio::test]
    async fn arc_rpc_layer_without_relays_passes_through() {
        let layer = ArcRpcLayer::new(
            true,
            false,
            usize::MAX,
            ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT,
            CallManyLimits::default(),
            RPC_DEFAULT_GAS_CAP,
            Vec::new(),
            DEFAULT_TX_RELAY_TIMEOUT,
        );
        assert!(layer.tx_relays.is_none());
        let middleware = layer.layer(MockRpcService);
        let resp = middleware
            .call(send_raw_tx_request(&encode_legacy_raw(Some(1)), 1))
            .await;
        assert_eq!(result_of(resp), serde_json::json!("success"));
    }
}
