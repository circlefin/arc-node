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

use std::time::Duration;

use backon::{Backoff, Retryable};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tracing::warn;

use crate::retry::NoRetry;
use crate::rpc::errors::EngineApiRpcError;
use crate::rpc::json_structs::{JsonRequestBody, JsonResponseBody};

/// A builder for creating and sending a JSON-RPC request.
pub struct RpcRequestBuilder<'a, B: Backoff = NoRetry> {
    client: &'a Client,
    url: &'a Url,
    method: &'a str,
    params: Option<Value>,
    timeout: Option<Duration>,
    bearer_auth: Option<String>,
    retry_policy: B,
}

impl<'a> RpcRequestBuilder<'a> {
    /// Creates a new builder instance.
    pub fn new(client: &'a Client, url: &'a Url, method: &'a str) -> Self {
        Self {
            client,
            url,
            method,
            params: None,
            timeout: None,
            bearer_auth: None,
            retry_policy: NoRetry,
        }
    }
}

impl<'a, B: Backoff> RpcRequestBuilder<'a, B> {
    /// Sets the parameters for the request.
    pub fn params(mut self, params: serde_json::Value) -> Self {
        self.params = Some(params);
        self
    }

    /// Sets a timeout for the request.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets the Bearer token for authorization.
    pub fn bearer_auth<S: Into<String>>(mut self, token: S) -> Self {
        self.bearer_auth = Some(token.into());
        self
    }

    /// Sets the retry policy for the request.
    /// This consumes the current builder and returns a new one with the specified policy.
    pub fn retry<NB: Backoff>(self, policy: NB) -> RpcRequestBuilder<'a, NB> {
        RpcRequestBuilder {
            client: self.client,
            url: self.url,
            method: self.method,
            params: self.params,
            timeout: self.timeout,
            bearer_auth: self.bearer_auth,
            retry_policy: policy,
        }
    }

    /// Builds and sends the JSON-RPC request.
    pub async fn send<D>(self) -> eyre::Result<D>
    where
        D: DeserializeOwned,
    {
        // Use provided params or default to an empty JSON array
        let params = self.params.unwrap_or_else(|| json!([]));

        let request_body = JsonRequestBody {
            jsonrpc: "2.0",
            method: self.method,
            params,
            id: Value::from(uuid::Uuid::new_v4().to_string()),
        };

        // Closure that sends the request and processes the response.
        // This will be retried according to the retry policy.
        let send_once = || async {
            let mut request_builder = self
                .client
                .post(self.url.clone())
                .header(CONTENT_TYPE, "application/json")
                .json(&request_body);

            // Apply timeout if one was provided
            if let Some(timeout) = self.timeout {
                request_builder = request_builder.timeout(timeout);
            }

            // Apply Bearer token if one was provided
            if let Some(token) = &self.bearer_auth {
                request_builder = request_builder.bearer_auth(token);
            }

            // Send the request
            let response = request_builder.send().await?.error_for_status()?;
            let response_body: JsonResponseBody = response.json().await?;

            match (response_body.result, response_body.error) {
                (result, None) => serde_json::from_value(result).map_err(Into::into),
                (_, Some(error)) => {
                    let engine_rpc_error = EngineApiRpcError::from(error);
                    Err(eyre::Report::new(engine_rpc_error).wrap_err("JSON-RPC request failed"))
                }
            }
        };

        // Use `backon::Retryable` to execute the closure with the given retry policy.
        // If the policy is `NoRetry`, it runs exactly once.
        send_once
            .retry(self.retry_policy)
            .notify(|e, dur| {
                warn!("RPC request failed: {e}, retrying in {dur:?}");
            })
            .await
    }
}
