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

//! Frame result utilities for Arc EVM handler

use arc_precompiles::helpers::{revert_message_to_bytes, ERR_BLOCKED_ADDRESS};
use reth_ethereum::primitives::Log;
use revm::{
    handler::FrameResult,
    interpreter::{
        interpreter_action::{FrameInit, FrameInput},
        CallOutcome, CreateOutcome, Gas, InstructionResult, InterpreterResult,
    },
};

/// Result of processing a native transfer with blocklist validation
#[derive(Debug)]
pub enum BeforeFrameInitResult {
    /// Transfer is valid, emit this log. The `u64` is the total SLOAD gas cost
    /// for blocklist checks (warm/cold-aware for Zero6+).
    Log(Log, u64),
    /// Blocklist passed, but no log to emit (e.g., Zero5 self-transfer). The `u64` is SLOAD gas.
    Checked(u64),
    /// Transfer reverted due to blocklist violation
    Reverted(FrameResult),
    /// No transfer to process (zero amount or non-value operation). No SLOADs performed.
    None,
}

pub(crate) const BLOCKLISTED_GAS_PENALTY: u64 = 0;

/// Creates a new FrameResult for out-of-gas during blocklist checks.
/// This is used when a nested frame doesn't have enough gas to cover the SLOAD costs
/// for blocklist verification.
pub fn create_oog_frame_result(frame_init: &FrameInit) -> FrameResult {
    match &frame_init.frame_input {
        FrameInput::Call(call_input) => {
            // All gas is consumed on OOG
            let mut gas_counter = Gas::new(call_input.gas_limit);
            let _ = gas_counter.record_cost(call_input.gas_limit);

            let interpreter_result = InterpreterResult::new(
                InstructionResult::OutOfGas,
                Default::default(),
                gas_counter,
            );
            FrameResult::Call(CallOutcome {
                result: interpreter_result,
                memory_offset: call_input.return_memory_offset.clone(),
                was_precompile_called: false,
                precompile_call_logs: Default::default(),
            })
        }
        FrameInput::Create(create_input) => {
            // All gas is consumed on OOG
            let mut gas_counter = Gas::new(create_input.gas_limit());
            let _ = gas_counter.record_cost(create_input.gas_limit());

            let interpreter_result = InterpreterResult::new(
                InstructionResult::OutOfGas,
                Default::default(),
                gas_counter,
            );
            FrameResult::Create(CreateOutcome {
                result: interpreter_result,
                address: None,
            })
        }
        FrameInput::Empty => unreachable!(),
    }
}

/// Creates a new FrameResult for a given frame init and error message.
pub fn create_frame_result(
    frame_init: &FrameInit,
    error_message: &str,
    gas_spent: u64,
) -> FrameResult {
    let revert_data = revert_message_to_bytes(error_message);

    match &frame_init.frame_input {
        FrameInput::Call(call_input) => {
            let mut gas_counter = Gas::new(call_input.gas_limit);
            gas_counter.set_spent(gas_spent);

            let interpreter_result =
                InterpreterResult::new(InstructionResult::Revert, revert_data, gas_counter);
            FrameResult::Call(CallOutcome {
                result: interpreter_result,
                memory_offset: call_input.return_memory_offset.clone(),
                was_precompile_called: false,
                precompile_call_logs: Default::default(),
            })
        }
        FrameInput::Create(create_input) => {
            let mut gas_counter = Gas::new(create_input.gas_limit());
            gas_counter.set_spent(gas_spent);

            let interpreter_result =
                InterpreterResult::new(InstructionResult::Revert, revert_data, gas_counter);
            FrameResult::Create(CreateOutcome {
                result: interpreter_result,
                address: None,
            })
        }
        FrameInput::Empty => unreachable!(),
    }
}

/// Creates a new FrameResult for blocklist violations from a FrameInit.
pub fn create_blocklisted_frame_result(frame_init: &FrameInit) -> FrameResult {
    create_frame_result(frame_init, ERR_BLOCKED_ADDRESS, BLOCKLISTED_GAS_PENALTY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes, U256};
    use revm::interpreter::{
        interpreter_action::{CreateInputs, FrameInit, FrameInput},
        SharedMemory,
    };
    use revm_interpreter::CreateScheme;

    #[test]
    fn test_create_blocklisted_frame_result_returns_none_address_for_create() {
        let create_inputs = CreateInputs::new(
            Address::repeat_byte(0x42),
            CreateScheme::Create,
            U256::ZERO,
            Bytes::from(vec![0x60, 0x00, 0x60, 0x00, 0xF3]), // minimal bytecode
            100_000,
        );

        let frame_init = FrameInit {
            depth: 0,
            memory: SharedMemory::default(),
            frame_input: FrameInput::Create(Box::new(create_inputs)),
        };

        let result = create_blocklisted_frame_result(&frame_init);

        match result {
            FrameResult::Create(outcome) => {
                assert!(
                    outcome.address.is_none(),
                    "Create outcome should have None address for blocklisted frame"
                );
                assert_eq!(outcome.result.result, InstructionResult::Revert);
            }
            _ => panic!("Expected FrameResult::Create variant"),
        }
    }
}
