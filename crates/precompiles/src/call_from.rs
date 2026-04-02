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

//! CallFrom subcall precompile — sender-preserving sub-calls.
//!
//! Allows an allowlisted caller contract to invoke a target contract while specifying
//! a custom `msg.sender`. This is the building block for Memo and Multicall3From.
//!
//! ## Solidity Interface
//!
//! ```solidity
//! function callFrom(address sender, address target, bytes calldata data)
//!     external returns (bool success, bytes memory returnData);
//! ```
//!
//! ## Security
//!
//! Access is restricted via the subcall registry's allowlist — only allowlisted contracts
//! may invoke CallFrom. The `sender` parameter is trusted: the allowlisted caller is
//! responsible for ensuring it passes the correct sender (typically its own `msg.sender`).

use crate::subcall::{
    SubcallCompletionResult, SubcallContinuationData, SubcallError, SubcallInitResult,
    SubcallPrecompile,
};
use alloy_primitives::{address, Address, U256};
use alloy_sol_types::{sol, SolCall};
use revm::handler::FrameResult;
use revm::interpreter::interpreter_action::{CallInput, CallInputs, CallScheme, CallValue};
use revm_context_interface::cfg::gas;

/// Address of the CallFrom precompile: `0x1800...0003`
pub const CALL_FROM_ADDRESS: Address = address!("1800000000000000000000000000000000000003");

/// Fixed base gas for init_subcall ABI decoding of `callFrom(address, address, bytes)`.
///
/// Covers selector matching plus the fixed-size ABI head: 2 address words + 1 offset word +
/// 1 length word. The dynamic `bytes data` payload is charged separately at
/// [`COPY`] gas per 32-byte word (see [`abi_decode_gas`]).
pub const ABI_DECODE_BASE_GAS: u64 = 100;

/// Computes total init_subcall gas: base overhead + ceil(data.len() / 32) * COPY.
pub fn abi_decode_gas(data_len: usize) -> u64 {
    let words = (data_len as u64).div_ceil(32);
    ABI_DECODE_BASE_GAS.saturating_add(words.saturating_mul(gas::COPY))
}

sol! {
    /// CallFrom precompile interface.
    interface ICallFrom {
        /// Execute a call to `target` with `data` as calldata, preserving `sender` as msg.sender.
        function callFrom(address sender, address target, bytes data) external returns (bool success, bytes memory returnData);
    }
}

/// Stateless CallFrom precompile implementing the two-phase subcall pattern.
///
/// `init_subcall`: ABI-decode `(sender, target, data)`, construct child call with the given sender.
/// `complete_subcall`: ABI-encode `(bool success, bytes returnData)` from the child result.
#[derive(Debug)]
pub struct CallFromPrecompile;

/// ABI-decode `callFrom(address, address, bytes)` from `inputs` and build the child
/// [`CallInputs`]. Returns the child inputs and the gas overhead consumed by decoding.
///
/// Shared by [`CallFromPrecompile::init_subcall`] and
/// [`CallFromPrecompile::trace_child_call`].
fn decode_child_call(inputs: &CallInputs) -> Result<(CallInputs, u64), SubcallError> {
    let input_bytes = match &inputs.input {
        CallInput::Bytes(b) => b,
        // SharedBuffer should not occur for precompile calls dispatched via frame_init,
        // but handle it defensively.
        CallInput::SharedBuffer(_) => {
            return Err(SubcallError::AbiDecodeError(
                "unexpected shared buffer input".into(),
            ));
        }
    };
    let decoded = ICallFrom::callFromCall::abi_decode(input_bytes)
        .map_err(|e| SubcallError::AbiDecodeError(format!("callFrom: {e}")))?;

    let sender = decoded.sender;
    let target = decoded.target;
    let calldata = decoded.data;

    // init_subcall overhead: fixed base + per-word charge for the dynamic `bytes data`.
    let overhead = abi_decode_gas(calldata.len());

    // EIP-150: deduct init_subcall overhead, then forward 63/64ths to child.
    let available = inputs.gas_limit.checked_sub(overhead).ok_or_else(|| {
        SubcallError::InsufficientGas("gas limit below ABI decode overhead".into())
    })?;
    let child_gas_limit = available - (available / 64);

    let child_inputs = CallInputs {
        scheme: CallScheme::Call,
        target_address: target,
        bytecode_address: target,
        known_bytecode: None,
        value: CallValue::Transfer(U256::ZERO),
        input: CallInput::Bytes(calldata),
        gas_limit: child_gas_limit,
        is_static: false,
        caller: sender,
        return_memory_offset: 0..0,
    };

    Ok((child_inputs, overhead))
}

impl SubcallPrecompile for CallFromPrecompile {
    fn init_subcall(&self, inputs: &CallInputs) -> Result<SubcallInitResult, SubcallError> {
        let (child_inputs, overhead) = decode_child_call(inputs)?;

        Ok(SubcallInitResult {
            child_inputs: Box::new(child_inputs),
            continuation_data: SubcallContinuationData {
                // CallFrom is stateless — no continuation state needed
                state: Box::new(()),
            },
            gas_overhead: overhead,
        })
    }

    fn trace_child_call(&self, inputs: &CallInputs) -> Option<CallInputs> {
        decode_child_call(inputs)
            .ok()
            .map(|(child_inputs, _)| child_inputs)
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
        let child_output = outcome.result.output.clone();

        // ABI-encode (bool success, bytes returnData) matching the declared interface.
        // The precompile always succeeds; the caller inspects the bool to determine
        // whether the child call succeeded or reverted.
        let encoded = ICallFrom::callFromCall::abi_encode_returns(&ICallFrom::callFromReturn {
            success: child_success,
            returnData: child_output,
        });

        Ok(SubcallCompletionResult {
            output: encoded.into(),
            success: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard against silent upstream changes to the EVM COPY gas cost.
    /// An unexpected change would alter `abi_decode_gas` results, effectively
    /// creating an unintentional hardfork.
    #[test]
    fn evm_copy_gas_cost_is_3() {
        assert_eq!(
            gas::COPY,
            3,
            "revm COPY gas cost changed — review abi_decode_gas impact"
        );
    }

    /// Guard against `trace_child_call` and `init_subcall` diverging.
    ///
    /// Both methods share `decode_child_call`, but if someone bypasses it in one
    /// path, the trace identity would silently drift from the executed identity.
    /// This test asserts that caller, target, scheme, value, and calldata match.
    /// Gas limit is intentionally excluded — `init_subcall` applies EIP-150
    /// 63/64ths forwarding while `trace_child_call` passes through the raw limit.
    #[test]
    fn trace_child_call_matches_init_subcall() {
        use alloy_primitives::address;
        use alloy_sol_types::SolCall;
        use revm::interpreter::interpreter_action::{CallInput, CallScheme, CallValue};

        let precompile = CallFromPrecompile;
        let sender = address!("e000000000000000000000000000000000000001");
        let target = address!("c000000000000000000000000000000000000002");
        let child_data: Vec<u8> = vec![0x42, 0x43];
        let gas_limit: u64 = 100_000;

        let calldata = ICallFrom::callFromCall {
            sender,
            target,
            data: child_data.clone().into(),
        }
        .abi_encode();

        let inputs = CallInputs {
            scheme: CallScheme::Call,
            target_address: CALL_FROM_ADDRESS,
            bytecode_address: CALL_FROM_ADDRESS,
            known_bytecode: None,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(calldata.into()),
            gas_limit,
            is_static: false,
            caller: address!("c000000000000000000000000000000000000001"),
            return_memory_offset: 0..0,
        };

        let init_result = precompile
            .init_subcall(&inputs)
            .expect("init_subcall should succeed");
        let trace_result = precompile
            .trace_child_call(&inputs)
            .expect("trace_child_call should return Some for valid input");

        let init_child = &*init_result.child_inputs;

        assert_eq!(trace_result.caller, init_child.caller, "caller mismatch");
        assert_eq!(
            trace_result.target_address, init_child.target_address,
            "target_address mismatch"
        );
        assert_eq!(
            trace_result.bytecode_address, init_child.bytecode_address,
            "bytecode_address mismatch"
        );
        assert_eq!(trace_result.scheme, init_child.scheme, "scheme mismatch");
        assert_eq!(trace_result.value, init_child.value, "value mismatch");
        assert_eq!(trace_result.input, init_child.input, "input mismatch");
        assert_eq!(
            trace_result.is_static, init_child.is_static,
            "is_static mismatch"
        );
    }
}
