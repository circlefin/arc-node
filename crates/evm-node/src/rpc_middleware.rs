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

use jsonrpsee::{
    core::middleware::{
        layer::Either, Batch, BatchEntry, BatchEntryErr, Notification, RpcServiceT,
    },
    types::{ErrorObject, ErrorObjectOwned, Request, ResponsePayload},
    MethodResponse,
};
use std::future::Future;
use tower::Layer;

const ETH_SUBSCRIBE_METHOD: &str = "eth_subscribe";
const PENDING_TX_SUBSCRIPTION_TYPE: &str = "newPendingTransactions";
const ETH_NEW_PENDING_TX_FILTER_METHOD: &str = "eth_newPendingTransactionFilter";
const ETH_GET_BLOCK_BY_NUMBER_METHOD: &str = "eth_getBlockByNumber";
const PENDING_BLOCK_TAG: &str = "pending";
const PENDING_TX_SUBSCRIPTION_ERROR_CODE: i32 = -32001;

/// Adds Arc-specific RPC middlewares
#[derive(Clone, Debug, Default)]
pub struct ArcRpcLayer {
    /// When true, `eth_subscribe("newPendingTransactions")`,
    /// `eth_newPendingTransactionFilter`, and `eth_getBlockByNumber("pending")`
    /// are blocked. When false (default), the filter is bypassed and these are
    /// allowed. Opt-in via `--arc.hide-pending-txs`.
    pub filter_pending_txs: bool,
}

impl ArcRpcLayer {
    /// Creates a new `ArcRpcLayer` with the given filter setting.
    pub fn new(filter_pending_txs: bool) -> Self {
        Self { filter_pending_txs }
    }
}

// S: Clone is required because NoPendingTransactionsRpcMiddleware clones
// the inner service in its `call` implementation.
impl<S> Layer<S> for ArcRpcLayer
where
    S: Clone,
{
    type Service = Either<NoPendingTransactionsRpcMiddleware<S>, S>;

    fn layer(&self, inner: S) -> Self::Service {
        if self.filter_pending_txs {
            Either::Left(NoPendingTransactionsRpcMiddleware { service: inner })
        } else {
            Either::Right(inner)
        }
    }
}

/// RPC middleware that prevents websocket subscriptions and HTTP filters for pending transactions.
#[derive(Clone, Default, Debug)]
pub struct NoPendingTransactionsRpcMiddleware<S> {
    service: S,
}

impl<S> NoPendingTransactionsRpcMiddleware<S> {
    /// Creates a new instance of the middleware.
    pub fn new(service: S) -> Self {
        Self { service }
    }
}

impl<S> RpcServiceT for NoPendingTransactionsRpcMiddleware<S>
where
    S: RpcServiceT<MethodResponse = MethodResponse, BatchResponse = MethodResponse>
        + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let service = self.service.clone();

        async move {
            // Error if the request is a pending transactions subscription or filter
            if let Err(err) = error_if_pending_tx_rpc(&req) {
                return MethodResponse::error(req.id(), err);
            }

            if is_pending_block_query(&req) {
                return null_response(&req);
            }

            service.call(req).await
        }
    }

    fn batch<'a>(&self, req: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        let batch = req
            .into_iter()
            .map(
                |request: Result<BatchEntry<'_>, BatchEntryErr<'_>>| match request {
                    Ok(BatchEntry::Call(request)) => {
                        // Error if the batch contains a pending transactions subscription or filter
                        error_if_pending_tx_rpc(&request)
                            .map_err(|err| BatchEntryErr::new(request.id(), err))?;
                        Ok(BatchEntry::Call(request))
                    }
                    _ => request,
                },
            )
            .collect::<Vec<_>>();

        // Forward the batch to the underlying service
        self.service.batch(Batch::from(batch))
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

    // ── Middleware active (--arc.hide-pending-txs) ──────────────────────
    //
    // When active, the middleware intercepts pending-state RPCs:
    // subscriptions, filters, and block queries.
    // The binary default is --arc.hide-pending-txs=false (middleware OFF).
    // Set --arc.hide-pending-txs on hardened nodes to activate.

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

    // -- batch requests --
    //
    // Pending-tx subscription/filter are blocked in batch.
    // Pending block interception is NOT applied in batch; falls back to
    // --rpc.pending-block=none.

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
    async fn test_enabled_batch_pending_block_passes_through() {
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
        assert_eq!(responses[1]["result"], "success");
    }

    // ── Middleware disabled (default, --arc.hide-pending-txs not set) ────
    //
    // The middleware is bypassed entirely. All requests pass through.

    #[tokio::test]
    async fn test_disabled_allows_pending_tx_subscription() {
        let layer = ArcRpcLayer {
            filter_pending_txs: false,
        };
        let middleware = layer.layer(MockRpcService);
        let params = RawValue::from_string(r#"["newPendingTransactions"]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_SUBSCRIBE_METHOD, params, 99);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=false should allow newPendingTransactions"
        );
    }

    #[tokio::test]
    async fn test_disabled_allows_pending_tx_filter() {
        let layer = ArcRpcLayer {
            filter_pending_txs: false,
        };
        let middleware = layer.layer(MockRpcService);
        let params = RawValue::from_string(r#"[]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_NEW_PENDING_TX_FILTER_METHOD, params, 101);
        let response = middleware.call(request).await;
        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=false should allow newPendingTransactionFilter"
        );
    }

    #[tokio::test]
    async fn test_disabled_allows_pending_block() {
        let layer = ArcRpcLayer {
            filter_pending_txs: false,
        };
        let middleware = layer.layer(MockRpcService);
        let params = RawValue::from_string(r#"["pending", false]"#.to_string()).unwrap();
        let request = create_request_with_params(ETH_GET_BLOCK_BY_NUMBER_METHOD, params, 1);
        let response = middleware.call(request).await;

        assert!(
            response.as_error_code().is_none(),
            "filter_pending_txs=false should allow getBlockByNumber(\"pending\")"
        );
    }

    // ── ArcRpcLayer::default() ──────────────────────────────────────────

    #[test]
    fn test_arc_rpc_layer_default_has_filter_disabled() {
        let layer = ArcRpcLayer::default();
        assert!(
            !layer.filter_pending_txs,
            "Default ArcRpcLayer should have filter disabled (opt-in via --arc.hide-pending-txs)"
        );
    }
}
