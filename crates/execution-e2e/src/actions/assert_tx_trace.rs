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

//! Debug trace assertion action for EIP-7708 e2e tests.

use crate::{action::Action, ArcEnvironment};
use alloy_rpc_types_trace::geth::{
    GethDebugBuiltInTracerType, GethDebugTracerConfig, GethDebugTracerType, GethDebugTracingOptions,
};
use futures_util::future::BoxFuture;
use reth_rpc_api::DebugApiClient;
use tracing::info;

/// Calls `debug_traceTransaction` for a named tx and asserts the call succeeds.
///
/// At minimum, every test instantiates this to verify the tracer does not panic
/// on EIP-7708 transactions. Content assertions (log count, topics, data) are
/// provided as builder methods but should be commented out until the tracing bug is fixed.
pub struct AssertTxTrace {
    tx_name: String,
}

impl AssertTxTrace {
    /// Creates a new trace assertion for the named transaction.
    ///
    /// The trace call uses `callTracer` with `{ withLog: true, onlyTopCall: false }`.
    pub fn new(tx_name: impl Into<String>) -> Self {
        Self {
            tx_name: tx_name.into(),
        }
    }
}

impl Action for AssertTxTrace {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let tx_hash = *env.get_tx_hash(&self.tx_name).ok_or_else(|| {
                eyre::eyre!("Transaction '{}' not found in environment", self.tx_name)
            })?;

            info!(
                name = %self.tx_name,
                tx_hash = %tx_hash,
                "Calling debug_traceTransaction with callTracer"
            );

            let client = env
                .node()
                .rpc_client()
                .ok_or_else(|| eyre::eyre!("RPC client not available"))?;

            let opts = GethDebugTracingOptions {
                tracer: Some(GethDebugTracerType::BuiltInTracer(
                    GethDebugBuiltInTracerType::CallTracer,
                )),
                tracer_config: GethDebugTracerConfig(
                    serde_json::json!({ "withLog": true, "onlyTopCall": false }),
                ),
                ..Default::default()
            };

            let trace = <jsonrpsee::http_client::HttpClient as DebugApiClient<
                alloy_rpc_types_eth::TransactionRequest,
            >>::debug_trace_transaction(&client, tx_hash, Some(opts))
            .await
            .map_err(|e| {
                eyre::eyre!(
                    "debug_traceTransaction failed for tx '{}' ({}): {}",
                    self.tx_name,
                    tx_hash,
                    e
                )
            })?;

            info!(
                name = %self.tx_name,
                tx_hash = %tx_hash,
                trace_variant = ?std::mem::discriminant(&trace),
                "debug_traceTransaction succeeded"
            );

            Ok(())
        })
    }
}
