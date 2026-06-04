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
use alloy_network::eip2718::Decodable2718;
use alloy_primitives::Bytes;
use jsonrpsee::{
    core::middleware::{layer::Either, Batch, BatchEntry, Notification, RpcServiceT},
    types::{ErrorObject, ErrorObjectOwned, Id, Request, ResponsePayload},
    BatchResponseBuilder, MethodResponse,
};
use std::future::Future;
use tower::Layer;

const ETH_SUBSCRIBE_METHOD: &str = "eth_subscribe";
const PENDING_TX_SUBSCRIPTION_TYPE: &str = "newPendingTransactions";
const ETH_NEW_PENDING_TX_FILTER_METHOD: &str = "eth_newPendingTransactionFilter";
const ETH_GET_BLOCK_BY_NUMBER_METHOD: &str = "eth_getBlockByNumber";
const PENDING_BLOCK_TAG: &str = "pending";
const ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD: &str = "eth_getTransactionBySenderAndNonce";
const ETH_SEND_RAW_TRANSACTION_METHOD: &str = "eth_sendRawTransaction";
const ETH_SEND_RAW_TRANSACTION_SYNC_METHOD: &str = "eth_sendRawTransactionSync";
const PENDING_TX_SUBSCRIPTION_ERROR_CODE: i32 = -32001;
const BATCH_TOO_LARGE_ERROR_CODE: i32 = -32600;
const UNPROTECTED_TX_ERROR_CODE: i32 = -32000;
const UNPROTECTED_TX_ERROR_MSG: &str =
    "only replay-protected (EIP-155) transactions allowed over RPC";

/// Default maximum number of entries permitted in a JSON-RPC batch request.
pub const ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT: usize = 100;

/// Config for the Arc-specific RPC middleware stack.
#[derive(Clone, Debug)]
pub struct ArcRpcLayer {
    /// When true (default), `eth_subscribe("newPendingTransactions")`,
    /// `eth_newPendingTransactionFilter`, `eth_getBlockByNumber("pending")`,
    /// and `eth_getTransactionBySenderAndNonce` are blocked.
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
    pub max_batch_entries: usize,
}

impl Default for ArcRpcLayer {
    fn default() -> Self {
        Self {
            filter_pending_txs: true,
            allow_unprotected_txs: false,
            max_response_body_size: usize::MAX,
            max_batch_entries: ARC_RPC_MAX_BATCH_ENTRIES_DEFAULT,
        }
    }
}

impl ArcRpcLayer {
    pub fn new(
        filter_pending_txs: bool,
        allow_unprotected_txs: bool,
        max_response_body_size: usize,
        max_batch_entries: usize,
    ) -> Self {
        Self {
            filter_pending_txs,
            allow_unprotected_txs,
            max_response_body_size,
            max_batch_entries,
        }
    }
}

// S: Clone is required because the middleware clones the inner service in its
// `call` implementation.
impl<S> Layer<S> for ArcRpcLayer
where
    S: Clone,
{
    type Service = BatchSizeLimitMiddleware<
        RejectUnprotectedTxsMiddleware<Either<NoPendingTransactionsRpcMiddleware<S>, S>>,
    >;

    fn layer(&self, inner: S) -> Self::Service {
        let pending_layer = if self.filter_pending_txs {
            Either::Left(NoPendingTransactionsRpcMiddleware {
                service: inner,
                max_response_body_size: self.max_response_body_size,
            })
        } else {
            Either::Right(inner)
        };
        let service = RejectUnprotectedTxsMiddleware {
            service: pending_layer,
            allow_unprotected_txs: self.allow_unprotected_txs,
            max_response_body_size: self.max_response_body_size,
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
    max_entries: usize,
}

impl<S> BatchSizeLimitMiddleware<S> {
    pub fn new(service: S, max_entries: usize) -> Self {
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
            if req.len() > max_entries {
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
async fn intercept_or_forward<'a, S>(service: &S, req: Request<'a>) -> MethodResponse
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync,
{
    if let Err(err) = error_if_pending_tx_rpc(&req) {
        return MethodResponse::error(req.id(), err);
    }
    if is_pool_pending_tx_lookup(&req) || is_pending_block_query(&req) {
        return null_response(&req);
    }
    service.call(req).await
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
        // Build the batch response here so unprotected-tx rejection can be partial;
        // forwarded entries still use `service.call()`, preserving inner middleware.
        async move {
            let mut builder = BatchResponseBuilder::new_with_limit(max_response_body_size);
            let mut got_notif = false;
            for entry in req {
                match entry {
                    Ok(BatchEntry::Call(request)) => {
                        let response = if !allow_unprotected_txs {
                            if let Err(err) = error_if_unprotected_send_raw_tx(&request) {
                                MethodResponse::error(request.id(), err)
                            } else {
                                service.call(request).await
                            }
                        } else {
                            service.call(request).await
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
fn error_if_unprotected_send_raw_tx<'a>(req: &Request<'a>) -> Result<(), ErrorObject<'a>> {
    if !is_raw_transaction_submission(req.method_name()) {
        return Ok(());
    }
    #[derive(serde::Deserialize)]
    struct SendRawTransactionParams {
        bytes: Bytes,
    }

    let bytes = if req.params().is_object() {
        let Ok(params) = req.params().parse::<SendRawTransactionParams>() else {
            return Ok(());
        };
        params.bytes
    } else {
        let Ok((bytes,)) = req.params().parse::<(Bytes,)>() else {
            return Ok(());
        };
        bytes
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

    if req.method_name() == ETH_SUBSCRIBE_METHOD {
        // Parse parameters to check if it's for newPendingTransactions
        if let Ok(Some(subscription_type)) = req.params().sequence().optional_next::<String>() {
            if subscription_type == PENDING_TX_SUBSCRIPTION_TYPE {
                let error = ErrorObjectOwned::owned::<()>(
                    PENDING_TX_SUBSCRIPTION_ERROR_CODE,
                    "Subscriptions to pending transactions are not allowed",
                    None,
                );
                return Err(error);
            }
        }
    }
    Ok(())
}

/// Returns true if the request queries the transaction pool directly for pending tx data.
fn is_pool_pending_tx_lookup(req: &Request<'_>) -> bool {
    req.method_name() == ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD
}

/// Returns true if the request is `eth_getBlockByNumber("pending", ...)`.
///
/// The consensus engine may briefly expose a pending block via `provider().pending_block()`
/// even when `--rpc.pending-block=none` is set.  Intercepting at the middleware layer
/// guarantees a consistent `null` response regardless of consensus-engine state.
fn is_pending_block_query(req: &Request<'_>) -> bool {
    if req.method_name() != ETH_GET_BLOCK_BY_NUMBER_METHOD {
        return false;
    }
    if let Ok(Some(block_tag)) = req.params().sequence().optional_next::<String>() {
        return block_tag == PENDING_BLOCK_TAG;
    }
    false
}

/// Builds a JSON-RPC success response containing `null`.
fn null_response(req: &Request<'_>) -> MethodResponse {
    let payload = ResponsePayload::success(serde_json::Value::Null);
    MethodResponse::response(req.id(), payload.into(), usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::{
        types::{Id, ResponsePayload},
        BatchResponseBuilder,
    };
    use serde_json::value::RawValue;
    use std::borrow::Cow;

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
    // eth_getTransactionBySenderAndNonce returns null (success, not error)
    // because it directly queries the pool for pending tx contents.

    #[tokio::test]
    async fn test_enabled_pool_lookup_returns_null() {
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
            "should return null for pool lookup"
        );
    }

    #[tokio::test]
    async fn test_enabled_pool_lookup_with_params_returns_null() {
        let middleware = NoPendingTransactionsRpcMiddleware::new(MockRpcService);
        let params = RawValue::from_string(
            r#"["0x1234567890abcdef1234567890abcdef12345678", "0x0"]"#.to_string(),
        )
        .unwrap();
        let request = create_request_with_params(ETH_GET_TX_BY_SENDER_AND_NONCE_METHOD, params, 2);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "should return success, not error"
        );
        let json: serde_json::Value = serde_json::from_str(response.into_json().get()).unwrap();
        assert!(
            json["result"].is_null(),
            "should return null for pool lookup with params"
        );
    }

    // -- batch requests --
    //
    // Batch entries are intercepted per-entry, consistent with the single-request path:
    // subscriptions/filters → error, pool lookups/pending block → null.

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
        let middleware = BatchSizeLimitMiddleware::new(MockRpcService, 3);
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
        let middleware = BatchSizeLimitMiddleware::new(MockRpcService, 10);
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
        let middleware = BatchSizeLimitMiddleware::new(MockRpcService, 2);
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
        let middleware = BatchSizeLimitMiddleware::new(MockRpcService, 1);
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
            max_batch_entries: 2,
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
            max_batch_entries: 2,
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
}
