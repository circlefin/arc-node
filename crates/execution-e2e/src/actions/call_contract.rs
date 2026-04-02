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

//! Contract call actions for Arc e2e tests.
//!
//! Provides actions to execute eth_call and verify return values.
//! This is useful for testing precompiles and read-only contract calls
//! without submitting transactions.

use crate::{action::Action, ArcEnvironment};
use alloy_primitives::{Address, Bytes, TxKind};
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use futures_util::future::BoxFuture;
use reth_rpc_api::EthApiClient;
use tracing::info;

/// Executes an eth_call to a contract or precompile and optionally verifies the result.
///
/// This action:
/// 1. Executes an eth_call (read-only, no transaction submitted)
/// 2. Optionally verifies the return value matches expected
/// 3. Can be configured to expect the call to revert
/// 4. Can be configured to expect a non-zero result
///
/// # Example
///
/// ```ignore
/// use alloy_primitives::{address, bytes};
///
/// ArcTestBuilder::new()
///     .with_setup(ArcSetup::new())
///     .with_action(ProduceBlocks::new(1))
///     .with_action(
///         CallContract::new("my_call")
///             .to(address!("0000000000000000000000000000000000000100"))
///             .with_data(bytes!("..."))
///             .expect_result(bytes!("0000000000000000000000000000000000000000000000000000000000000001"))
///     )
///     .run()
///     .await
/// ```
#[derive(Debug)]
pub struct CallContract {
    /// Name to reference this call in logs and errors.
    name: String,
    /// Target address (contract or precompile).
    to: Address,
    /// Call data to send.
    data: Bytes,
    /// Expected return value. If Some, will assert equality.
    expected_result: Option<Bytes>,
    /// If true, expect the call to fail/revert.
    expect_revert: bool,
    /// If true, expect the result to be non-zero (non-empty and not all zeros).
    expect_non_zero: bool,
}

impl CallContract {
    /// Creates a new CallContract action with the given name.
    ///
    /// The name is used for logging and error messages.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            to: Address::ZERO,
            data: Bytes::new(),
            expected_result: None,
            expect_revert: false,
            expect_non_zero: false,
        }
    }

    /// Sets the target address for the call.
    pub fn to(mut self, address: Address) -> Self {
        self.to = address;
        self
    }

    /// Sets the call data.
    pub fn with_data(mut self, data: Bytes) -> Self {
        self.data = data;
        self
    }

    /// Sets the expected return value.
    ///
    /// The action will fail if the actual result doesn't match.
    pub fn expect_result(mut self, result: Bytes) -> Self {
        self.expected_result = Some(result);
        self
    }

    /// Configures the action to expect the call to revert.
    ///
    /// The action will fail if the call succeeds.
    pub fn expect_revert(mut self) -> Self {
        self.expect_revert = true;
        self
    }

    /// Configures the action to expect a non-zero result.
    ///
    /// The action will fail if the result is empty or all zeros.
    pub fn expect_non_zero_result(mut self) -> Self {
        self.expect_non_zero = true;
        self
    }
}

impl Action for CallContract {
    fn execute<'a>(&'a mut self, env: &'a mut ArcEnvironment) -> BoxFuture<'a, eyre::Result<()>> {
        Box::pin(async move {
            info!(
                name = %self.name,
                to = %self.to,
                data_len = self.data.len(),
                expect_revert = self.expect_revert,
                "Executing eth_call"
            );

            // Get RPC client from node
            let client = env
                .node()
                .rpc_client()
                .ok_or_else(|| eyre::eyre!("RPC client not available"))?;

            // Build transaction request
            let request = TransactionRequest {
                to: Some(TxKind::Call(self.to)),
                input: TransactionInput::new(self.data.clone()),
                ..Default::default()
            };

            // Execute eth_call using the EthApiClient trait
            // HttpClient implements EthApiClient with these type parameters
            let result = <jsonrpsee::http_client::HttpClient as EthApiClient<
                TransactionRequest,
                alloy_rpc_types_eth::Transaction,
                alloy_rpc_types_eth::Block,
                alloy_rpc_types_eth::TransactionReceipt,
                alloy_rpc_types_eth::Header,
                Bytes,
            >>::call(&client, request, None, None, None)
            .await;

            match result {
                Ok(output) => {
                    if self.expect_revert {
                        return Err(eyre::eyre!(
                            "Call '{}' succeeded but was expected to revert. Output: {}",
                            self.name,
                            output
                        ));
                    }

                    info!(
                        name = %self.name,
                        output_len = output.len(),
                        output = %output,
                        "eth_call succeeded"
                    );

                    // Verify expected result if provided
                    if let Some(expected) = &self.expected_result {
                        if output != *expected {
                            return Err(eyre::eyre!(
                                "Call '{}' result mismatch.\nExpected: {}\nActual: {}",
                                self.name,
                                expected,
                                output
                            ));
                        }
                        info!(name = %self.name, "Result matches expected value");
                    }

                    // Verify non-zero result if configured
                    if self.expect_non_zero {
                        if output.is_empty() || output.iter().all(|&b| b == 0) {
                            return Err(eyre::eyre!(
                                "Call '{}' returned zero/empty result but expected non-zero.\nActual: {}",
                                self.name,
                                output
                            ));
                        }
                        info!(name = %self.name, "Result is non-zero as expected");
                    }

                    Ok(())
                }
                Err(err) => {
                    if self.expect_revert {
                        info!(
                            name = %self.name,
                            error = %err,
                            "eth_call reverted as expected"
                        );
                        Ok(())
                    } else {
                        Err(eyre::eyre!(
                            "Call '{}' failed unexpectedly: {}",
                            self.name,
                            err
                        ))
                    }
                }
            }
        })
    }
}
