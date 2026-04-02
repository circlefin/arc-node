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

use color_eyre::eyre::{self, Result};
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tracing::warn;
use tungstenite::error::{Error as WsError, ProtocolError};
use tungstenite::Utf8Bytes;
use url::Url;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone)]
pub(crate) struct WsClientBuilder {
    pub url: Url,
    connect_timeout: Option<Duration>,
    request_timeout: Duration,
}

impl WsClientBuilder {
    pub fn new(url: Url, timeout: Duration) -> Self {
        Self {
            url,
            connect_timeout: None,
            request_timeout: timeout,
        }
    }

    pub fn with_connect_timeout(self, timeout: Duration) -> Self {
        Self {
            connect_timeout: Some(timeout),
            ..self
        }
    }
}

impl WsClientBuilder {
    pub async fn build(self) -> Result<WsClient> {
        WsClient::new(self.url, self.request_timeout, self.connect_timeout).await
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct WsClient {
    pub url: Url,
    request_timeout: Duration,
    connect_timeout: Option<Duration>,
    ws: WsStream,
    next_id: u64,
}

impl WsClient {
    /// Establish a WebSocket JSON-RPC connection to the given `url`.
    ///
    /// If `connect_timeout` is `Some`, the connection will be retried until the timeout is reached.
    /// If `connect_timeout` is `None`, the connection will be retried forever.
    pub async fn new(
        url: Url,
        timeout: Duration,
        connect_timeout: Option<Duration>,
    ) -> Result<Self> {
        let ws = Self::connect_with_retry(&url, connect_timeout).await?;
        Ok(Self {
            url,
            request_timeout: timeout,
            connect_timeout,
            ws,
            next_id: 1,
        })
    }

    /// Attempt to connect with retry logic.
    ///
    /// If `connect_timeout` is `Some`, retries until the timeout is reached.
    /// If `connect_timeout` is `None`, retries forever.
    async fn connect_with_retry(url: &Url, connect_timeout: Option<Duration>) -> Result<WsStream> {
        let start_time = Instant::now();
        loop {
            match connect_async(url.as_str()).await {
                Ok((ws, _resp)) => return Ok(ws),
                Err(WsError::Url(e)) => {
                    // Non-recoverable error due to invalid URL, fail fast.
                    return Err(eyre::eyre!("Invalid WebSocket URL: {e}"));
                }
                Err(e) => {
                    // Check if we've exceeded the timeout (if one is set)
                    if let Some(timeout_duration) = connect_timeout {
                        if start_time.elapsed() >= timeout_duration {
                            return Err(eyre::eyre!("connection timeout: {e}"));
                        }
                    }

                    warn!("WebSocket connection failed: {e}. Retrying...");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// Send a JSON-RPC request over WebSocket.
    ///
    /// The send is bounded by `request_timeout` so that a half-open TCP
    /// connection (e.g. remote paused or network-disconnected) does not block
    /// the sender indefinitely. On timeout the error is an `Io` variant so
    /// that callers can treat it as a retriable connection error.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_request_id();
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let msg = Message::Text(Utf8Bytes::from(request.to_string()));
        match timeout(self.request_timeout, self.ws.send(msg)).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(WsError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "WebSocket send timed out",
                ))
                .into());
            }
        }
        Ok(id)
    }

    /// Send a JSON-RPC request over WebSocket and wait for the matching response
    pub async fn request_response<D: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<D> {
        let req_id = self.request(method, params).await?;
        self.wait_for_response(req_id).await
    }

    /// Wait for the matching response to the given request id
    pub async fn wait_for_response<D: DeserializeOwned>(&mut self, id: u64) -> Result<D> {
        let start_time = Instant::now();
        while start_time.elapsed() < self.request_timeout {
            let msg = timeout(self.request_timeout, self.ws.next())
                .await
                .map_err(|_| eyre::eyre!("operation timed out"))?;

            let Some(msg) = msg.transpose()? else {
                continue;
            };
            match classify_ws_message(msg)? {
                WsMessageAction::Response(body) => {
                    if body.id == json!(id) {
                        if let Some(JsonError { code, message }) = body.error {
                            return Err(eyre::eyre!("Server Error {}: {}", code, message));
                        }
                        return serde_json::from_value(body.result).map_err(Into::into);
                    }
                    // Not our response; keep waiting.
                }
                WsMessageAction::Ping(p) => {
                    self.ws.send(Message::Pong(p.into())).await?;
                }
                WsMessageAction::Closed => {
                    return Err(WsError::ConnectionClosed.into());
                }
                WsMessageAction::Notification(_) | WsMessageAction::Skip => {}
            }
        }
        Err(eyre::eyre!("timeout waiting for response"))
    }

    /// Send an `eth_subscribe` request and return the subscription ID.
    ///
    /// Example: `subscribe(json!(["newHeads"]))` subscribes to new block
    /// headers. The returned string is the subscription ID assigned by
    /// the server.
    pub async fn subscribe(&mut self, params: Value) -> Result<String> {
        self.request_response("eth_subscribe", params).await
    }

    /// Wait for the next `eth_subscription` notification.
    ///
    /// Reads messages from the WebSocket, skipping non-subscription
    /// messages (regular RPC responses, pings, etc.). Returns the
    /// deserialized `params.result` payload from the notification.
    ///
    /// Times out after `request_timeout`.
    pub async fn next_notification<D: DeserializeOwned>(&mut self) -> Result<D> {
        let start_time = Instant::now();
        while start_time.elapsed() < self.request_timeout {
            let msg = timeout(self.request_timeout, self.ws.next())
                .await
                .map_err(|_| eyre::eyre!("timeout waiting for notification"))?;

            let Some(msg) = msg.transpose()? else {
                continue;
            };
            match classify_ws_message(msg)? {
                WsMessageAction::Notification(result) => {
                    return serde_json::from_value(result).map_err(Into::into);
                }
                WsMessageAction::Ping(p) => {
                    self.ws.send(Message::Pong(p.into())).await?;
                }
                WsMessageAction::Closed => {
                    return Err(WsError::ConnectionClosed.into());
                }
                WsMessageAction::Response(_) | WsMessageAction::Skip => {}
            }
        }
        Err(eyre::eyre!("timeout waiting for notification"))
    }

    /// Reconnect the WebSocket connection.
    /// This is useful when the connection breaks and needs to be re-established.
    pub async fn reconnect(&mut self) -> Result<()> {
        self.ws = Self::connect_with_retry(&self.url, self.connect_timeout).await?;
        self.next_id = 1;
        Ok(())
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonResponseBody {
    pub jsonrpc: String,
    #[serde(default)]
    pub error: Option<JsonError>,
    #[serde(default)]
    pub result: serde_json::Value,
    pub id: serde_json::Value,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct JsonError {
    pub code: i64,
    pub message: String,
}

/// Classification of a raw WebSocket message.
///
/// Used by `wait_for_response` and `next_notification` to avoid
/// duplicating message-handling logic.
#[derive(Debug)]
enum WsMessageAction {
    /// An `eth_subscription` notification with the raw
    /// `params.result` value.
    Notification(Value),
    /// A JSON-RPC response.
    Response(JsonResponseBody),
    /// A ping that needs a pong reply.
    Ping(Vec<u8>),
    /// The server closed the connection.
    Closed,
    /// Irrelevant message — caller should read the next one.
    Skip,
}

/// Classify a single WebSocket message for the read loop.
///
/// Both [`WsClient::wait_for_response`] and [`WsClient::next_notification`]
/// delegate here so that the parsing logic is testable without a live
/// connection.
fn classify_ws_message(msg: Message) -> Result<WsMessageAction> {
    match msg {
        Message::Text(txt) => {
            let Ok(v) = serde_json::from_str::<Value>(&txt) else {
                return Ok(WsMessageAction::Skip);
            };
            if v.get("method").and_then(|m| m.as_str()) == Some("eth_subscription") {
                let result = v
                    .get("params")
                    .and_then(|p| p.get("result"))
                    .cloned()
                    .ok_or_else(|| {
                        eyre::eyre!("missing params.result in subscription notification")
                    })?;
                return Ok(WsMessageAction::Notification(result));
            }
            if let Ok(body) = serde_json::from_value::<JsonResponseBody>(v) {
                return Ok(WsMessageAction::Response(body));
            }
            Ok(WsMessageAction::Skip)
        }
        Message::Ping(p) => Ok(WsMessageAction::Ping(p.into())),
        Message::Close(_) => Ok(WsMessageAction::Closed),
        // Matches Message::Binary, Message::Pong, and Message::Frame, none of which
        // we care about.
        _ => Ok(WsMessageAction::Skip),
    }
}

/// Check if an error is a connection error that may be retryable.
pub(crate) fn is_connection_error(error: &eyre::Report) -> bool {
    if let Some(ws_err) = error.downcast_ref::<WsError>() {
        matches!(
            ws_err,
            WsError::Io(_)
                | WsError::ConnectionClosed
                | WsError::AlreadyClosed
                | WsError::Tls(_)
                | WsError::Protocol(
                    ProtocolError::SendAfterClosing
                        | ProtocolError::ReceivedAfterClosing
                        | ProtocolError::ResetWithoutClosingHandshake
                )
        )
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::Utf8Bytes;

    /// Helper: build a `Message::Text` from a JSON string.
    fn text_msg(s: &str) -> Message {
        Message::Text(Utf8Bytes::from(s.to_string()))
    }

    #[test]
    fn classify_subscription_notification() {
        let msg = text_msg(
            r#"{
                "jsonrpc":"2.0",
                "method":"eth_subscription",
                "params":{
                    "subscription":"0xabc",
                    "result":{"number":"0x1"}
                }
            }"#,
        );
        let action = classify_ws_message(msg).unwrap();
        match action {
            WsMessageAction::Notification(v) => {
                assert_eq!(v, json!({"number": "0x1"}));
            }
            other => panic!("expected Notification, got {other:?}"),
        }
    }

    #[test]
    fn classify_subscription_missing_result() {
        let msg = text_msg(
            r#"{
                "jsonrpc":"2.0",
                "method":"eth_subscription",
                "params":{"subscription":"0xabc"}
            }"#,
        );
        let err = classify_ws_message(msg).unwrap_err();
        assert!(
            err.to_string().contains("missing params.result"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn classify_rpc_response() {
        let msg = text_msg(
            r#"{
                "jsonrpc":"2.0",
                "id":1,
                "result":"0x123456789abcdef"
            }"#,
        );
        let action = classify_ws_message(msg).unwrap();
        match action {
            WsMessageAction::Response(body) => {
                assert_eq!(body.id, json!(1));
                assert_eq!(body.result, json!("0x123456789abcdef"));
                assert!(body.error.is_none());
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn classify_rpc_error_response() {
        let msg = text_msg(
            r#"{
                "jsonrpc":"2.0",
                "id":42,
                "error":{"code":-32600,"message":"invalid request"}
            }"#,
        );
        let action = classify_ws_message(msg).unwrap();
        match action {
            WsMessageAction::Response(body) => {
                assert_eq!(body.id, json!(42));
                let err = body.error.expect("expected error");
                assert_eq!(err.code, -32600);
                assert_eq!(err.message, "invalid request");
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn classify_non_json_text() {
        let msg = text_msg("not json");
        let action = classify_ws_message(msg).unwrap();
        assert!(
            matches!(action, WsMessageAction::Skip),
            "expected Skip, got {action:?}"
        );
    }

    #[test]
    fn classify_unknown_json() {
        let msg = text_msg(r#"{"foo":"bar"}"#);
        let action = classify_ws_message(msg).unwrap();
        assert!(
            matches!(action, WsMessageAction::Skip),
            "expected Skip, got {action:?}"
        );
    }

    #[test]
    fn classify_ping() {
        let payload = b"hello".to_vec();
        let msg = Message::Ping(payload.clone().into());
        let action = classify_ws_message(msg).unwrap();
        match action {
            WsMessageAction::Ping(p) => assert_eq!(p, payload),
            other => panic!("expected Ping, got {other:?}"),
        }
    }

    #[test]
    fn classify_close() {
        let msg = Message::Close(None);
        let action = classify_ws_message(msg).unwrap();
        assert!(
            matches!(action, WsMessageAction::Closed),
            "expected Closed, got {action:?}"
        );
    }

    #[test]
    fn classify_binary_is_skip() {
        let msg = Message::Binary(vec![0u8; 4].into());
        let action = classify_ws_message(msg).unwrap();
        assert!(
            matches!(action, WsMessageAction::Skip),
            "expected Skip, got {action:?}"
        );
    }

    #[test]
    fn classify_pong_is_skip() {
        let msg = Message::Pong(b"pong".to_vec().into());
        let action = classify_ws_message(msg).unwrap();
        assert!(
            matches!(action, WsMessageAction::Skip),
            "expected Skip, got {action:?}"
        );
    }
}
