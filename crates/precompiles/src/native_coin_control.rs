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

//! Native Coin Control Precompile
//!
//! This precompile implements native coin control operations including
//! blocklisting and unblocklisting addresses from receiving native coin transfers.

use crate::helpers::{
    check_delegatecall, check_gas_remaining, check_staticcall, emit_event, read, write,
    PrecompileErrorOrRevert, ERR_EXECUTION_REVERTED, LOG_BASE_COST, LOG_TOPIC_COST,
    NATIVE_FIAT_TOKEN_ADDRESS, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, PRECOMPILE_SLOAD_GAS_COST,
    PRECOMPILE_SSTORE_GAS_COST,
};
use crate::stateful;
use alloy_evm::EvmInternals;
use alloy_primitives::{address, Address, StorageKey, U256};
use alloy_sol_types::{sol, SolCall, SolValue};
use arc_execution_config::hardforks::{ArcHardfork, ArcHardforkFlags};
use arc_execution_config::native_coin_control as native_coin_control_config;
use reth_ethereum::evm::revm::precompile::PrecompileOutput;
use revm_interpreter::Gas;

// Native coin control precompile address
pub const NATIVE_COIN_CONTROL_ADDRESS: Address =
    address!("0x1800000000000000000000000000000000000001");

// Allowed caller form NativeFiatToken
const ALLOWED_CALLER_ADDRESS: Address = NATIVE_FIAT_TOKEN_ADDRESS;

/// Exported error message / revert string
pub const BLOCKLISTED_ERROR_MESSAGE: &str = "address is blocklisted";

// Storage key for allowed caller (deprecated since Zero5)
const ALLOWED_CALLER_STORAGE_KEY: StorageKey = StorageKey::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
]);

// Gas costs
const BLOCKLISTED_EVENT_GAS_COST: u64 = LOG_BASE_COST + 2 * LOG_TOPIC_COST; // 2 topics
const UNBLOCKLISTED_EVENT_GAS_COST: u64 = LOG_BASE_COST + 2 * LOG_TOPIC_COST; // 2 topics

// Total gas costs for each operation

// - Reading allowed caller (2100 gas)
// - Writing blocklist storage (2900 gas)
// - Emitting event (1125 gas)
// Total: 6125 gas
const BLOCKLIST_GAS_COST: u64 =
    PRECOMPILE_SLOAD_GAS_COST + PRECOMPILE_SSTORE_GAS_COST + BLOCKLISTED_EVENT_GAS_COST;

// - Reading blocklist storage (2100 gas)
// Total: 2100 gas
pub const IS_BLOCKLISTED_GAS_COST: u64 = PRECOMPILE_SLOAD_GAS_COST;

// - Reading allowed caller (2100 gas)
// - Writing blocklist storage (2900 gas)
// - Emitting event (1125 gas)
// Total: 6125 gas
const UNBLOCKLIST_GAS_COST: u64 =
    PRECOMPILE_SLOAD_GAS_COST + PRECOMPILE_SSTORE_GAS_COST + UNBLOCKLISTED_EVENT_GAS_COST;

// Storage values
pub const BLOCKLISTED_STATUS: U256 = U256::from_limbs([1, 0, 0, 0]); // 0x01
pub const UNBLOCKLISTED_STATUS: U256 = U256::ZERO; // 0x00

// Error messages
const ERR_CANNOT_BLOCKLIST: &str = "Not enabled for blocklisting";
const ERR_CANNOT_UNBLOCKLIST: &str = "Not enabled for unblocklisting";

sol! {
    /// Native Coin Control precompile interface
    interface INativeCoinControl {
        /// Add an address to the blocklist
        function blocklist(address account) external returns (bool success);

        /// Check if an address is blocklisted
        function isBlocklisted(address account) external view returns (bool _isBlocklisted);

        /// Remove an address from the blocklist
        function unBlocklist(address account) external returns (bool success);
    }

    /// Events
    #[derive(Debug)]
    event Blocklisted(address indexed account);

    #[derive(Debug)]
    event UnBlocklisted(address indexed account);
}

/// Checks if the caller is authorized to call mutative native coin control functions
fn is_authorized(
    internals: &mut EvmInternals,
    caller: Address,
    gas_counter: &mut Gas,
    hardfork_flags: ArcHardforkFlags,
) -> Result<bool, PrecompileErrorOrRevert> {
    // Get allowed caller
    let allowed_caller_output = read(
        internals,
        NATIVE_COIN_CONTROL_ADDRESS,
        ALLOWED_CALLER_STORAGE_KEY,
        gas_counter,
        hardfork_flags,
    )?;

    // Compare caller to allowed_caller_output
    let caller_word = U256::from_be_slice(caller.as_ref());
    let allowed_caller_word = U256::from_be_slice(&allowed_caller_output);
    Ok(caller_word == allowed_caller_word)
}

/// Computes the storage slot for a mapping key of type address
///
/// Delegates to the execution-config canonical implementation.
pub fn compute_is_blocklisted_storage_slot(key: Address) -> StorageKey {
    StorageKey::new(native_coin_control_config::compute_is_blocklisted_storage_slot(key).0)
}

stateful!(run_native_coin_control, precompile_input, hardfork_flags; {
    INativeCoinControl::blocklistCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let mut precompile_input = precompile_input;

            // Check if static call is attempting to modify state
            check_staticcall(
                &precompile_input,
                &mut gas_counter,
            )?;

            // Decode arguments passed to blocklist function
            let args = INativeCoinControl::blocklistCall::abi_decode_raw(input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED,
                    )
                )?;

            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                // Early return if not enough gas
                check_gas_remaining(&gas_counter, BLOCKLIST_GAS_COST - PRECOMPILE_SLOAD_GAS_COST)?;

                // Check authorization
                if precompile_input.caller != ALLOWED_CALLER_ADDRESS {
                    return Err(PrecompileErrorOrRevert::new_reverted(
                        gas_counter,
                        ERR_CANNOT_BLOCKLIST,
                    ));
                }
            } else {
                // Early return if not enough gas
                check_gas_remaining(&gas_counter, BLOCKLIST_GAS_COST)?;

                // Check authorization
                if !(is_authorized(
                    &mut precompile_input.internals,
                    precompile_input.caller,
                    &mut gas_counter,
                    hardfork_flags,
                )?) {
                    return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_CANNOT_BLOCKLIST));
                }
            }

            // Check delegate call
            check_delegatecall(
                NATIVE_COIN_CONTROL_ADDRESS,
                &precompile_input,
                &mut gas_counter,
            )?;

            // Add to blocklist
            let storage_slot = compute_is_blocklisted_storage_slot(args.account);
            write(
                &mut precompile_input.internals,
                NATIVE_COIN_CONTROL_ADDRESS,
                storage_slot,
                &BLOCKLISTED_STATUS.to_be_bytes_vec(),
                &mut gas_counter,
                hardfork_flags,
            )?;

            // Emit event
            emit_event(
                &mut precompile_input.internals,
                NATIVE_COIN_CONTROL_ADDRESS,
                &Blocklisted {
                    account: args.account,
                },
                &mut gas_counter,
            )?;

            let output = true.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into()))
        })()
    },

    INativeCoinControl::isBlocklistedCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let mut precompile_input = precompile_input;

            // Decode arguments passed to isBlocklisted function
            let args = INativeCoinControl::isBlocklistedCall::abi_decode_raw(input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED,
                    )
                )?;

            // Early return if not enough gas
            check_gas_remaining(&gas_counter, IS_BLOCKLISTED_GAS_COST)?;

            // Check if address is blocklisted
            let storage_slot = compute_is_blocklisted_storage_slot(args.account);
            let storage_output = read(
                &mut precompile_input.internals,
                NATIVE_COIN_CONTROL_ADDRESS,
                storage_slot,
                &mut gas_counter,
                hardfork_flags,
            )?;

            let status = U256::from_be_slice(&storage_output);
            // Pessimistically assume blocklisted unless strictly matching unblocklisted status
            let is_blocked = status != UNBLOCKLISTED_STATUS;

            let output = is_blocked.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into()))
        })()
    },

    INativeCoinControl::unBlocklistCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let mut precompile_input = precompile_input;

            // Check if static call is attempting to modify state
            check_staticcall(
                &precompile_input,
                &mut gas_counter,
            )?;

            // Decode arguments passed to unBlocklist function
            let args = INativeCoinControl::unBlocklistCall::abi_decode_raw(input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED,
                    )
                )?;

            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                // Early return if not enough gas
                check_gas_remaining(&gas_counter, UNBLOCKLIST_GAS_COST - PRECOMPILE_SLOAD_GAS_COST)?;

                // Check authorization
                if precompile_input.caller != ALLOWED_CALLER_ADDRESS {
                    return Err(PrecompileErrorOrRevert::new_reverted(
                        gas_counter,
                        ERR_CANNOT_UNBLOCKLIST,
                    ));
                }
            } else {
                // Early return if not enough gas
                check_gas_remaining(&gas_counter, UNBLOCKLIST_GAS_COST)?;

                // Check authorization
                if !(is_authorized(
                    &mut precompile_input.internals,
                    precompile_input.caller,
                    &mut gas_counter,
                    hardfork_flags,
                )?) {
                    return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_CANNOT_UNBLOCKLIST));
                }
            }

            // Check delegate call
            check_delegatecall(
                NATIVE_COIN_CONTROL_ADDRESS,
                &precompile_input,
                &mut gas_counter,
            )?;

            // Remove from blocklist
            let storage_slot = compute_is_blocklisted_storage_slot(args.account);
            write(
                &mut precompile_input.internals,
                NATIVE_COIN_CONTROL_ADDRESS,
                storage_slot,
                &UNBLOCKLISTED_STATUS.to_be_bytes_vec(),
                &mut gas_counter,
                hardfork_flags,
            )?;

            // Emit event
            emit_event(
                &mut precompile_input.internals,
                NATIVE_COIN_CONTROL_ADDRESS,
                &UnBlocklisted {
                    account: args.account,
                },
                &mut gas_counter,
            )?;

            let output = true.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into()))
        })()
    },
});

#[cfg(test)]
mod tests {
    use crate::helpers::ERR_DELEGATE_CALL_NOT_ALLOWED;
    use arc_execution_config::hardforks::ArcHardforkFlags;

    use super::*;
    use alloy_primitives::Bytes;
    use alloy_sol_types::SolEvent;
    use reth_ethereum::evm::revm::{
        context::{Context, ContextTr, JournalTr},
        interpreter::{CallInput, CallInputs, CallScheme, CallValue, InstructionResult},
        MainContext,
    };
    use reth_evm::precompiles::{DynPrecompile, PrecompilesMap};
    use revm::{
        handler::PrecompileProvider,
        interpreter::InterpreterResult,
        precompile::{PrecompileId, Precompiles},
    };

    fn mock_context(hardfork_flags: ArcHardforkFlags) -> revm::Context {
        let mut ctx = Context::mainnet();

        ctx.journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .expect("Unable to load native coin control account");

        if !hardfork_flags.is_active(ArcHardfork::Zero5) {
            ctx.journal_mut()
                .sstore(
                    NATIVE_COIN_CONTROL_ADDRESS,
                    ALLOWED_CALLER_STORAGE_KEY.into(),
                    U256::from_be_slice(ALLOWED_CALLER_ADDRESS.as_ref()),
                )
                .expect("Unable to write allowed caller");
        }

        ctx
    }

    fn call_native_coin_control(
        ctx: &mut Context,
        inputs: &CallInputs,
        hardfork_flags: ArcHardforkFlags,
    ) -> Result<Option<InterpreterResult>, String> {
        let mut provider = PrecompilesMap::from_static(Precompiles::latest());
        let target_addr: Address = inputs.target_address;
        provider.set_precompile_lookup(move |address: &Address| {
            if *address == NATIVE_COIN_CONTROL_ADDRESS || target_addr == NATIVE_COIN_CONTROL_ADDRESS
            {
                Some(DynPrecompile::new_stateful(
                    PrecompileId::Custom("NATIVE_COIN_CONTROL".into()),
                    move |input| run_native_coin_control(input, hardfork_flags),
                ))
            } else {
                None
            }
        });
        provider.run(ctx, inputs)
    }

    struct NativeCoinControlTest {
        name: &'static str,
        caller: Address,
        calldata: Bytes,
        gas_limit: u64,
        expected_result: InstructionResult,
        expected_revert_str: Option<&'static str>,
        return_data: Option<Bytes>,
        gas_used: u64,
        /// If set, overrides gas_used for pre-Zero5 hardforks (before EIP-2929/2200 storage costs)
        pre_zero5_gas_used: Option<u64>,
        target_address: Address,
        bytecode_address: Address,
    }

    // Test constants
    const ADDRESS_A: Address = address!("1000000000000000000000000000000000000001");
    const ADDRESS_B: Address = address!("2000000000000000000000000000000000000002");

    fn assert_precompile_result(
        precompile_res: Result<Option<InterpreterResult>, String>,
        tc: &NativeCoinControlTest,
        hardfork_flags: ArcHardforkFlags,
        tc_name: &str,
    ) {
        match precompile_res {
            Ok(result) => {
                assert!(result.is_some(), "{}: expected result to be some", tc.name);
                let result = result.unwrap();

                assert_eq!(
                    result.result, tc.expected_result,
                    "{tc_name}: expected result to match",
                );

                if let Some(expected_revert_str) = tc.expected_revert_str {
                    assert!(
                        result.is_revert(),
                        "{tc_name}: expected output to be reverted"
                    );
                    let revert_reason = bytes_to_revert_message(result.output.as_ref());
                    assert!(revert_reason.is_some(), "{tc_name}: expected revert reason");
                    assert_eq!(
                        revert_reason.unwrap(),
                        expected_revert_str,
                        "{tc_name}: expected revert reason to match",
                    );
                } else {
                    assert!(
                        !result.is_revert(),
                        "{tc_name}: expected output not to be reverted"
                    );
                }

                if let Some(expected_return_data) = &tc.return_data {
                    assert_eq!(
                        result.output, *expected_return_data,
                        "{tc_name}: expected return data to match",
                    );
                }

                let expected_gas_used = if hardfork_flags.is_active(ArcHardfork::Zero5) {
                    tc.gas_used
                } else {
                    tc.pre_zero5_gas_used.unwrap_or(tc.gas_used)
                };
                assert_eq!(
                    result.gas.used(),
                    expected_gas_used,
                    "{tc_name}: gas used to match"
                );
            }
            Err(e) => {
                panic!("{tc_name}: unexpected error {:?}", e)
            }
        }
    }

    #[test]
    fn native_coin_control_precompile_basic_functionality() {
        let cases: &[NativeCoinControlTest] = &[
            // SSTORE (0→1, cold) = 22100, event = 1125, total = 23225
            NativeCoinControlTest {
                name: "blocklist() succeeds and returns true",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::blocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: 100_000,
                expected_result: InstructionResult::Return,
                expected_revert_str: None,
                return_data: Some(true.abi_encode().into()),
                gas_used: 23225,
                pre_zero5_gas_used: Some(BLOCKLIST_GAS_COST),
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            // Reverts before storage ops, 0 gas
            NativeCoinControlTest {
                name: "blocklist() unauthorized caller reverts",
                caller: ADDRESS_A,
                calldata: INativeCoinControl::blocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: 100_000,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_CANNOT_BLOCKLIST),
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            NativeCoinControlTest {
                name: "blocklist() insufficient gas errors with OOG",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::blocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: BLOCKLIST_GAS_COST - PRECOMPILE_SLOAD_GAS_COST - 1, // Not enough gas
                expected_result: InstructionResult::PrecompileOOG,
                expected_revert_str: None,
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            NativeCoinControlTest {
                name: "blocklist() invalid params errors with Execution Reverted",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::blocklistCall::SELECTOR.into(),
                gas_limit: BLOCKLIST_GAS_COST,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                return_data: None,
                gas_used: PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            // Reverts before storage ops, 0 gas
            NativeCoinControlTest {
                name: "blocklist() with target address != precompile address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::blocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: BLOCKLIST_GAS_COST,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: ADDRESS_B,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            // Reverts before storage ops, 0 gas
            NativeCoinControlTest {
                name: "blocklist() with bytecode address != precompile address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::blocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: BLOCKLIST_GAS_COST,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: ADDRESS_B,
            },
            // SLOAD cold = 2100 (same for Zero5 and pre-Zero5)
            NativeCoinControlTest {
                name: "isBlocklisted() returns false for non-blocklisted address",
                caller: ADDRESS_A, // Authorization not required for view function
                calldata: INativeCoinControl::isBlocklistedCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: 100_000,
                expected_result: InstructionResult::Return,
                expected_revert_str: None,
                return_data: Some(false.abi_encode().into()),
                gas_used: PRECOMPILE_SLOAD_GAS_COST,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            NativeCoinControlTest {
                name: "isBlocklisted() insufficient gas errors with OOG",
                caller: ADDRESS_A,
                calldata: INativeCoinControl::isBlocklistedCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: IS_BLOCKLISTED_GAS_COST - 1, // Not enough gas
                expected_result: InstructionResult::PrecompileOOG,
                expected_revert_str: None,
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            NativeCoinControlTest {
                name: "isBlocklisted() invalid params errors with Execution Reverted",
                caller: ADDRESS_A,
                calldata: INativeCoinControl::isBlocklistedCall::SELECTOR.into(),
                gas_limit: IS_BLOCKLISTED_GAS_COST,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                return_data: None,
                gas_used: PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            // SSTORE (0→0, cold) = 2200, event = 1125, total = 3325
            NativeCoinControlTest {
                name: "unBlocklist() succeeds and returns true",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::unBlocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: 100_000,
                expected_result: InstructionResult::Return,
                expected_revert_str: None,
                return_data: Some(true.abi_encode().into()),
                gas_used: 3325,
                pre_zero5_gas_used: Some(UNBLOCKLIST_GAS_COST),
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            // Reverts before storage ops, 0 gas
            NativeCoinControlTest {
                name: "unBlocklist() with target != precompile address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::unBlocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: 100_000,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: ADDRESS_B,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            // Reverts before storage ops, 0 gas
            NativeCoinControlTest {
                name: "unBlocklist() with bytecode_address != precompile address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::unBlocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: 100_000,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: ADDRESS_B,
            },
            // Reverts before storage ops, 0 gas
            NativeCoinControlTest {
                name: "unBlocklist() unauthorized caller reverts",
                caller: ADDRESS_A,
                calldata: INativeCoinControl::unBlocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: 100_000,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_CANNOT_UNBLOCKLIST),
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            NativeCoinControlTest {
                name: "unBlocklist() insufficient gas errors with OOG",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::unBlocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
                gas_limit: UNBLOCKLIST_GAS_COST - PRECOMPILE_SLOAD_GAS_COST - 1, // Not enough gas
                expected_result: InstructionResult::PrecompileOOG,
                expected_revert_str: None,
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
            NativeCoinControlTest {
                name: "unBlocklist() invalid params errors with Execution Reverted",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinControl::unBlocklistCall::SELECTOR.into(),
                gas_limit: UNBLOCKLIST_GAS_COST,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                return_data: None,
                gas_used: PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_CONTROL_ADDRESS,
                bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            },
        ];

        for tc in cases {
            for hardfork_flags in ArcHardforkFlags::all_combinations() {
                let tc_name =
                    tc.name.to_string() + &format!(" (hardfork_flags: {:?})", hardfork_flags);

                // Sanity check that we're not configuring test cases incorrectly
                match tc.expected_result {
                    InstructionResult::Revert | InstructionResult::Return => {}
                    _ => {
                        assert!(
                            tc.return_data.is_none(),
                            "{tc_name}: expected no return data",
                        );
                    }
                }

                let mut ctx = mock_context(hardfork_flags);

                // Prepare inputs
                let inputs = CallInputs {
                    scheme: CallScheme::Call,
                    target_address: tc.target_address,
                    bytecode_address: tc.bytecode_address,
                    known_bytecode: None,
                    caller: tc.caller,
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(tc.calldata.clone()),
                    gas_limit: tc.gas_limit,
                    is_static: false,
                    return_memory_offset: 0..0,
                };

                let precompile_res = call_native_coin_control(&mut ctx, &inputs, hardfork_flags);
                assert_precompile_result(precompile_res, tc, hardfork_flags, &tc_name);
            }
        }
    }

    #[test]
    fn blocklist_workflow_zero3() {
        test_blocklist_workflow(ArcHardforkFlags::default());
    }
    #[test]
    fn blocklist_workflow_zero4() {
        test_blocklist_workflow(ArcHardforkFlags::with(&[ArcHardfork::Zero4]));
    }
    #[test]
    fn blocklist_workflow_zero5() {
        test_blocklist_workflow(ArcHardforkFlags::with(&[ArcHardfork::Zero5]));
    }
    fn test_blocklist_workflow(hardfork_flags: ArcHardforkFlags) {
        let mut ctx = mock_context(hardfork_flags);

        // Test 1: Initially address should not be blocklisted
        let is_blocklisted_input = CallInputs {
            scheme: CallScheme::Call,
            target_address: NATIVE_COIN_CONTROL_ADDRESS,
            bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            known_bytecode: None,
            caller: ADDRESS_A,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(
                INativeCoinControl::isBlocklistedCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
            ),
            gas_limit: 100_000,
            is_static: false,
            return_memory_offset: 0..0,
        };

        let result = call_native_coin_control(&mut ctx, &is_blocklisted_input, hardfork_flags)
            .unwrap()
            .unwrap();
        assert_eq!(result.result, InstructionResult::Return);
        assert_eq!(result.output, Bytes::from(false.abi_encode()));

        // Test 2: Blocklist the address
        let blocklist_input = CallInputs {
            scheme: CallScheme::Call,
            target_address: NATIVE_COIN_CONTROL_ADDRESS,
            bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            known_bytecode: None,
            caller: ALLOWED_CALLER_ADDRESS,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(
                INativeCoinControl::blocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
            ),
            gas_limit: 100_000,
            is_static: false,
            return_memory_offset: 0..0,
        };

        let result = call_native_coin_control(&mut ctx, &blocklist_input, hardfork_flags)
            .unwrap()
            .unwrap();
        assert_eq!(result.result, InstructionResult::Return);
        assert_eq!(result.output, Bytes::from(true.abi_encode()));

        // Check event emission for blocklist
        let journal_mut = ctx.journal_mut();
        let logs = journal_mut.logs();
        let expected_log = Blocklisted { account: ADDRESS_B }.encode_log_data();
        assert_eq!(logs.len(), 1, "Expected one log event for blocklist");
        let log = &logs[0];
        assert_eq!(
            log.address, NATIVE_COIN_CONTROL_ADDRESS,
            "Log address mismatch"
        );
        assert_eq!(
            log.data, expected_log,
            "Log data mismatch for blocklist event"
        );

        // Test 3: Verify address is now blocklisted
        let result = call_native_coin_control(&mut ctx, &is_blocklisted_input, hardfork_flags)
            .unwrap()
            .unwrap();
        assert_eq!(result.result, InstructionResult::Return);
        assert_eq!(result.output, Bytes::from(true.abi_encode()));

        // Test 4: Unblocklist the address
        let unblocklist_input = CallInputs {
            scheme: CallScheme::Call,
            target_address: NATIVE_COIN_CONTROL_ADDRESS,
            bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
            known_bytecode: None,
            caller: ALLOWED_CALLER_ADDRESS,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(
                INativeCoinControl::unBlocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
            ),
            gas_limit: 100_000,
            is_static: false,
            return_memory_offset: 0..0,
        };

        let result = call_native_coin_control(&mut ctx, &unblocklist_input, hardfork_flags)
            .unwrap()
            .unwrap();
        assert_eq!(result.result, InstructionResult::Return);
        assert_eq!(result.output, Bytes::from(true.abi_encode()));

        // Check event emission for unblocklist
        let journal_mut = ctx.journal_mut();
        let logs = journal_mut.logs();
        assert_eq!(logs.len(), 2, "Expected two log events after unblocklist");

        // Verify the second event is UnBlocklisted
        let expected_unblocklist_log = UnBlocklisted { account: ADDRESS_B }.encode_log_data();
        let unblocklist_log = &logs[1];
        assert_eq!(
            unblocklist_log.address, NATIVE_COIN_CONTROL_ADDRESS,
            "Log address mismatch for unblocklist"
        );
        assert_eq!(
            unblocklist_log.data, expected_unblocklist_log,
            "Log data mismatch for unblocklist event"
        );

        // Test 5: Verify address is no longer blocklisted
        let result = call_native_coin_control(&mut ctx, &is_blocklisted_input, hardfork_flags)
            .unwrap()
            .unwrap();
        assert_eq!(result.result, InstructionResult::Return);
        assert_eq!(result.output, Bytes::from(false.abi_encode()));
    }

    // Helper to convert bytes to a revert error string
    fn bytes_to_revert_message(input: &[u8]) -> Option<String> {
        use crate::helpers::REVERT_SELECTOR;
        use alloy_sol_types::SolValue;

        // Expect at least 4 bytes for the selector.
        if input.len() < 4 {
            return None;
        }
        // Check the selector matches the standard Error(string) selector.
        if input[0..4] != REVERT_SELECTOR {
            return None;
        }

        String::abi_decode(&input[4..]).ok()
    }

    #[test]
    fn test_static_call_reverts_state_modifying_functions() {
        use crate::helpers::ERR_STATE_CHANGE_DURING_STATIC_CALL;

        let state_modifying_calldatas: &[(&str, Bytes)] = &[
            (
                "blocklist",
                INativeCoinControl::blocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
            ),
            (
                "unBlocklist",
                INativeCoinControl::unBlocklistCall { account: ADDRESS_B }
                    .abi_encode()
                    .into(),
            ),
        ];

        for hardfork_flags in ArcHardforkFlags::all_combinations() {
            // State-modifying functions must revert under static call
            for (fn_name, calldata) in state_modifying_calldatas {
                let mut ctx = mock_context(hardfork_flags);
                let inputs = CallInputs {
                    scheme: CallScheme::Call,
                    target_address: NATIVE_COIN_CONTROL_ADDRESS,
                    bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
                    known_bytecode: None,
                    caller: ALLOWED_CALLER_ADDRESS,
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(calldata.clone()),
                    gas_limit: 100_000,
                    is_static: true,
                    return_memory_offset: 0..0,
                };

                let result = call_native_coin_control(&mut ctx, &inputs, hardfork_flags)
                    .expect("call should not error")
                    .expect("result should be Some");

                assert_eq!(
                    result.result,
                    InstructionResult::Revert,
                    "{fn_name} (hardfork_flags: {hardfork_flags:?}): expected Revert",
                );
                let revert_reason = bytes_to_revert_message(result.output.as_ref());
                assert_eq!(
                    revert_reason.as_deref(),
                    Some(ERR_STATE_CHANGE_DURING_STATIC_CALL),
                    "{fn_name} (hardfork_flags: {hardfork_flags:?}): wrong revert reason",
                );
            }

            // Read-only function (isBlocklisted) must succeed under static call
            {
                let mut ctx = mock_context(hardfork_flags);
                let inputs = CallInputs {
                    scheme: CallScheme::Call,
                    target_address: NATIVE_COIN_CONTROL_ADDRESS,
                    bytecode_address: NATIVE_COIN_CONTROL_ADDRESS,
                    known_bytecode: None,
                    caller: ADDRESS_A,
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(
                        INativeCoinControl::isBlocklistedCall { account: ADDRESS_B }
                            .abi_encode()
                            .into(),
                    ),
                    gas_limit: 100_000,
                    is_static: true,
                    return_memory_offset: 0..0,
                };

                let result = call_native_coin_control(&mut ctx, &inputs, hardfork_flags)
                    .expect("call should not error")
                    .expect("result should be Some");

                assert_eq!(
                    result.result,
                    InstructionResult::Return,
                    "isBlocklisted (hardfork_flags: {hardfork_flags:?}): expected Return under static call",
                );
            }
        }
    }
}
