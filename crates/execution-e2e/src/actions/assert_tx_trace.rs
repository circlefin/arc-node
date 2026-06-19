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
    GethDebugBuiltInTracerType, GethDebugTracerConfig, GethDebugTracerType,
    GethDebugTracingOptions, GethDefaultTracingOptions, GethTrace,
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

/// Calls `debug_traceTransaction` with the default struct logger and asserts the
/// gas cost of the last occurrence of an opcode.
pub struct AssertLastOpcodeGasCost {
    tx_name: String,
    opcode: String,
    expected_gas_cost: u64,
}

impl AssertLastOpcodeGasCost {
    /// Creates a new opcode gas-cost assertion for the named transaction.
    pub fn new(tx_name: impl Into<String>, opcode: impl Into<String>, gas_cost: u64) -> Self {
        Self {
            tx_name: tx_name.into(),
            opcode: opcode.into(),
            expected_gas_cost: gas_cost,
        }
    }
}

impl Action for AssertLastOpcodeGasCost {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            let tx_hash = *env.get_tx_hash(&self.tx_name).ok_or_else(|| {
                eyre::eyre!("Transaction '{}' not found in environment", self.tx_name)
            })?;

            info!(
                name = %self.tx_name,
                tx_hash = %tx_hash,
                opcode = %self.opcode,
                expected_gas_cost = self.expected_gas_cost,
                "Calling debug_traceTransaction with struct logger"
            );

            let client = env
                .node()
                .rpc_client()
                .ok_or_else(|| eyre::eyre!("RPC client not available"))?;

            let opts = GethDebugTracingOptions {
                config: GethDefaultTracingOptions::default()
                    .with_enable_memory(false)
                    .disable_stack()
                    .disable_storage(),
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

            let GethTrace::Default(frame) = trace else {
                return Err(eyre::eyre!(
                    "Expected default struct-log trace for tx '{}'",
                    self.tx_name
                ));
            };

            let Some(log) = frame
                .struct_logs
                .iter()
                .rev()
                .find(|log| log.opcode() == self.opcode)
            else {
                return Err(eyre::eyre!(
                    "Tx '{}': opcode '{}' not found in trace",
                    self.tx_name,
                    self.opcode
                ));
            };

            if log.gas_cost != self.expected_gas_cost {
                return Err(eyre::eyre!(
                    "Tx '{}': last '{}' gas cost mismatch. Expected {}, got {}",
                    self.tx_name,
                    self.opcode,
                    self.expected_gas_cost,
                    log.gas_cost
                ));
            }

            info!(
                name = %self.tx_name,
                opcode = %self.opcode,
                gas_cost = log.gas_cost,
                "Opcode gas-cost assertion passed"
            );

            Ok(())
        })
    }
}
