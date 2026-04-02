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

//! Test precompile for subcall integration tests.
//!
//! This precompile demonstrates the two-phase subcall pattern:
//! - `init_subcall`: ABI-decode `(address target, bytes calldata)` from input
//! - `complete_subcall`: On child success, ABI-encode child output as `bytes` return
//!
//! Only available under `#[cfg(test)]`.

use alloy_primitives::{address, Address, Bytes};
use alloy_sol_types::{sol_data, SolType, SolValue};
use arc_precompiles::subcall::{
    SubcallCompletionResult, SubcallContinuationData, SubcallError, SubcallInitResult,
    SubcallPrecompile,
};
use revm::handler::FrameResult;
use revm::interpreter::interpreter_action::{CallInput, CallInputs, CallScheme, CallValue};
use revm::interpreter::Gas;

/// Address of the subcall test precompile: `0x1800...0099`
pub const SUBCALL_TEST_ADDRESS: Address = address!("1800000000000000000000000000000000000099");

// ABI types for init_subcall input decoding: (address target, bytes calldata)
type InitSubcallInput = (sol_data::Address, sol_data::Bytes);

/// Test subcall precompile for integration testing.
#[derive(Debug)]
pub struct SubcallTestPrecompile;

impl SubcallPrecompile for SubcallTestPrecompile {
    fn init_subcall(&self, inputs: &CallInputs) -> Result<SubcallInitResult, SubcallError> {
        let input_bytes = match &inputs.input {
            CallInput::Bytes(b) => b.clone(),
            CallInput::SharedBuffer(_) => {
                return Err(SubcallError::AbiDecodeError(
                    "unexpected shared buffer input".into(),
                ));
            }
        };

        let decoded = <InitSubcallInput as SolType>::abi_decode(&input_bytes)
            .map_err(|e| SubcallError::AbiDecodeError(format!("subcall_test: {e}")))?;

        let (target, calldata) = decoded;

        // hardcoded limit for testing purposes
        let child_gas = Gas::new(100_000);

        let child_inputs = Box::new(CallInputs {
            scheme: CallScheme::Call,
            target_address: target,
            bytecode_address: target,
            known_bytecode: None,
            value: CallValue::Transfer(alloy_primitives::U256::ZERO),
            input: CallInput::Bytes(calldata),
            gas_limit: child_gas.remaining(),
            is_static: inputs.is_static,
            caller: inputs.caller,
            return_memory_offset: 0..0,
        });

        Ok(SubcallInitResult {
            child_inputs,
            continuation_data: SubcallContinuationData {
                state: Box::new(()),
            },
            // Test precompile — not gas-precise; see CallFromPrecompile for real accounting
            gas_overhead: 0,
        })
    }

    fn complete_subcall(
        &self,
        _continuation_data: SubcallContinuationData,
        child_result: &FrameResult,
    ) -> Result<SubcallCompletionResult, SubcallError> {
        let outcome = match child_result {
            FrameResult::Call(outcome) => outcome,
            _ => return Err(SubcallError::UnexpectedFrameResult),
        };

        let child_success = outcome.result.result.is_ok();
        let child_output = &outcome.result.output;

        if !child_success {
            // Child failed — signal that the precompile should revert
            return Ok(SubcallCompletionResult {
                output: if outcome.result.result.is_revert() {
                    child_output.clone()
                } else {
                    Bytes::new()
                },
                success: false,
            });
        }

        // ABI-encode the child output as `bytes` return type
        let encoded_output = Bytes::from(child_output.clone().abi_encode());

        Ok(SubcallCompletionResult {
            output: encoded_output,
            success: true,
        })
    }
}

/// Address for the FailingCompleteSubcallPrecompile test precompile.
pub const FAILING_COMPLETE_SUBCALL_ADDRESS: Address =
    address!("1800000000000000000000000000000000000098");

/// Test precompile whose complete_subcall always errors — used to verify all-gas-consumed behavior.
#[derive(Debug)]
pub struct FailingCompleteSubcallPrecompile;

impl SubcallPrecompile for FailingCompleteSubcallPrecompile {
    fn init_subcall(&self, inputs: &CallInputs) -> Result<SubcallInitResult, SubcallError> {
        let input_bytes = match &inputs.input {
            CallInput::Bytes(b) => b.clone(),
            CallInput::SharedBuffer(_) => {
                return Err(SubcallError::AbiDecodeError(
                    "unexpected shared buffer".into(),
                ));
            }
        };

        let decoded = <InitSubcallInput as SolType>::abi_decode(&input_bytes)
            .map_err(|e| SubcallError::AbiDecodeError(format!("failing_complete_subcall: {e}")))?;
        let (target, calldata) = decoded;

        Ok(SubcallInitResult {
            child_inputs: Box::new(CallInputs {
                scheme: CallScheme::Call,
                target_address: target,
                bytecode_address: target,
                known_bytecode: None,
                value: CallValue::Transfer(alloy_primitives::U256::ZERO),
                input: CallInput::Bytes(calldata),
                gas_limit: 50_000,
                is_static: false,
                caller: inputs.caller,
                return_memory_offset: 0..0,
            }),
            continuation_data: SubcallContinuationData {
                state: Box::new(()),
            },
            gas_overhead: 0,
        })
    }

    fn complete_subcall(
        &self,
        _continuation_data: SubcallContinuationData,
        _child_result: &FrameResult,
    ) -> Result<SubcallCompletionResult, SubcallError> {
        Err(SubcallError::InternalError(
            "intentional complete_subcall failure".into(),
        ))
    }
}
