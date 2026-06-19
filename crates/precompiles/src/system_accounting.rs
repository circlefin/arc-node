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

use crate::helpers::{
    abi_decode_raw_with_zero6_validation, check_delegatecall, check_staticcall,
    new_reverted_with_early_penalty, read, record_cost_or_out_of_gas, write,
    PrecompileErrorOrRevert, ERR_EXECUTION_REVERTED, ERR_INVALID_CALLER,
    PRECOMPILE_EARLY_REVERT_GAS_PENALTY, PRECOMPILE_SLOAD_GAS_COST,
};
use crate::precompile;
use alloy_evm::Evm;
use alloy_primitives::B256;
use alloy_primitives::{address, keccak256, Address, Bytes, StorageKey};
use alloy_sol_types::{sol, SolCall, SolValue};
use arc_execution_config::hardforks::ArcHardfork;
use reth_ethereum::evm::revm::precompile::PrecompileOutput;
use revm::handler::SYSTEM_ADDRESS;
use revm::state::EvmState;
use revm::DatabaseCommit;
use revm_interpreter::Gas;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SystemAccountingError<E> {
    #[error("EVM execution failed: {0}")]
    Execution(E),
    #[error("ABI decode error: {0}")]
    AbiDecode(String),
    #[error("System call reverted")]
    Reverted(),
    #[error("Unable to store value")]
    StoreFailed(),
}

// System Accounting precompile address
pub const SYSTEM_ACCOUNTING_ADDRESS: Address =
    address!("0x1800000000000000000000000000000000000002");

// Storage key for storing gas values
const GAS_VALUES_STORAGE_KEY: StorageKey = StorageKey::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
]);

/// Ring buffer capacity for historical gas values. Consensus reads only
/// freshly-written slots (the executor reads the parent slot for EMA smoothing;
/// the assembler reads the current slot just written by `finish()`), so no
/// history depth is required for correctness. The extra capacity exists purely
/// as headroom for external readers (RPC, monitoring) and is otherwise arbitrary.
const GAS_VALUES_RING_BUFFER_SIZE: u64 = 64;

// Arc system-accounting caller.
const ARC_SYSTEM_CALLER: Address = SYSTEM_ADDRESS;

sol! {
    struct GasValues {
        uint64 gasUsed;
        uint64 gasUsedSmoothed;
        /// store the computed base fee for next block
        /// max value is 2^64 - 1 ~= 18 USDC
        uint64 nextBaseFee;
    }

    interface ISystemAccounting {
        /// Writes `gasValues` into ring-buffer slot
        /// `blockNumber % GAS_VALUES_RING_BUFFER_SIZE`, overwriting whatever
        /// the slot previously held. ARC_SYSTEM_CALLER-gated; no validation on
        /// `blockNumber`, since writes happen once per block from the block
        /// executor.
        function storeGasValues(uint64 blockNumber, GasValues calldata gasValues) external returns (bool);

        /// Returns ring-buffer slot `blockNumber % GAS_VALUES_RING_BUFFER_SIZE`
        /// as-is, without any freshness check. If `blockNumber` has been
        /// rotated out (more than `GAS_VALUES_RING_BUFFER_SIZE - 1` behind the
        /// latest written block) or is in the future, the slot holds the last
        /// block that mapped to it, i.e. a different block's values. Slots
        /// that have never been written (possible only early in the chain's
        /// life, before every slot has been reached once) read as zero.
        /// Callers needing freshness must cross-check against their own view
        /// of the chain tip. Consensus does not depend on freshness: the
        /// executor reads only the parent slot for EMA smoothing, which was
        /// written by the previous block's `finish()` (or reads as zero at
        /// genesis, the correct EMA baseline), and the block assembler reads
        /// only the current slot just written by the same block's `finish()`.
        function getGasValues(uint64 blockNumber) external view returns (GasValues calldata gasValue);
    }
}

/// Computes the storage slot for a mapping key of type address
///
/// A mapping, while slightly less efficient than a fixed size contiguous array,
/// is more flexible if additional gas values should be added in the future.
///
/// Implements Solidity's mapping storage slot calculation:
/// Formula: keccak256(h(k) . p), where:
/// - k is the mapping key (uint64)
/// - p is the mapping slot position (GAS_VALUES_STORAGE_KEY)
/// - h left-pads the key to 32 bytes
/// - . is concatenation
///
/// `block_number` is reduced mod `GAS_VALUES_RING_BUFFER_SIZE` before hashing,
/// so any two block numbers that differ by a multiple of the ring buffer size
/// collide on the same slot. The mapping carries no identity of the block that
/// last wrote the slot — callers who need that identity must track it
/// out-of-band.
pub fn compute_gas_values_storage_slot(block_number: u64) -> StorageKey {
    // Map block number into ring buffer
    let key_value = block_number % GAS_VALUES_RING_BUFFER_SIZE;

    // Left-pad 8 byte u64 to 32 bytes
    let mut key_bytes = [0u8; 32];
    key_bytes[24..].copy_from_slice(key_value.to_be_bytes().as_ref());

    // Use AVERAGED_HISTORICAL_GAS_STORAGE_KEY as the slot bytes
    let slot_bytes = GAS_VALUES_STORAGE_KEY.0;

    // Concatenate key and slot, then hash
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(&key_bytes);
    data[32..].copy_from_slice(&slot_bytes);

    StorageKey::new(keccak256(data).0)
}

precompile!(run_system_accounting, precompile_input, hardfork_flags; {
    ISystemAccounting::storeGasValuesCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let mut precompile_input = precompile_input;

            // Check if static call is attempting to modify state
            check_staticcall(
                &precompile_input,
                &mut gas_counter,
            )?;

            // Decode arguments passed to blocklist function
            let args = abi_decode_raw_with_zero6_validation::<ISystemAccounting::storeGasValuesCall>(
                input,
                hardfork_flags,
            )
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, PRECOMPILE_EARLY_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED,
                    )
                )?;

            // Redundant 2100-gas charge — no SLOAD occurs here, but kept pre-Zero6 to
            // preserve consensus on already-finalized blocks.
            if !hardfork_flags.is_active(ArcHardfork::Zero6) {
                record_cost_or_out_of_gas(&mut gas_counter, PRECOMPILE_SLOAD_GAS_COST)?;
            }

            // Check caller
            if precompile_input.caller != ARC_SYSTEM_CALLER {
                return Err(new_reverted_with_early_penalty(gas_counter, ERR_INVALID_CALLER, hardfork_flags));
            }

            // Check delegatecall
            check_delegatecall(
                SYSTEM_ACCOUNTING_ADDRESS,
                &precompile_input,
                &gas_counter,
            )?;

            // Update storage
            let storage_slot = compute_gas_values_storage_slot(args.blockNumber);
            let updated_value_bytes = pack_gas_values_for_storage(args.gasValues);
            write(
                &mut precompile_input.internals,
                SYSTEM_ACCOUNTING_ADDRESS,
                storage_slot,
                &updated_value_bytes,
                &mut gas_counter,
                hardfork_flags,
            )?;

            let output = true.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into()))
        })()
    },
    ISystemAccounting::getGasValuesCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let mut precompile_input = precompile_input;

            // Decode arguments passed to blocklist function
            let args = abi_decode_raw_with_zero6_validation::<ISystemAccounting::getGasValuesCall>(
                input,
                hardfork_flags,
            )
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, PRECOMPILE_EARLY_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED,
                    )
                )?;

            // Read stored value
            let storage_slot = compute_gas_values_storage_slot(args.blockNumber);
            let slot_value = read(
                &mut precompile_input.internals,
                SYSTEM_ACCOUNTING_ADDRESS,
                storage_slot,
                &mut gas_counter,
                hardfork_flags,
            )?;
            let gas_values = unpack_gas_values_from_storage(B256::from_slice(slot_value.as_ref()));
            let output = gas_values.abi_encode();

            Ok(PrecompileOutput::new(gas_counter.used(), output.into()))
        })()
    },
});

/// Packs GasValues into a single 32-byte storage slot
/// The layout is:
/// - `gasUsedSmoothed` (u64): bytes [16..24]
/// - `gasUsed` (u64):         bytes [24..32]
fn pack_gas_values_for_storage(g: GasValues) -> [u8; 32] {
    let mut slot = [0u8; 32];
    slot[24..32].copy_from_slice(&g.gasUsed.to_be_bytes());
    slot[16..24].copy_from_slice(&g.gasUsedSmoothed.to_be_bytes());
    slot[8..16].copy_from_slice(&g.nextBaseFee.to_be_bytes());
    slot
}

pub fn unpack_gas_values_from_storage(slot: B256) -> GasValues {
    let bytes = slot.as_slice();
    let gas_used = u64::from_be_bytes(
        bytes[24..32]
            .try_into()
            .expect("8-byte slice from 32-byte array"),
    );
    let gas_used_smoothed = u64::from_be_bytes(
        bytes[16..24]
            .try_into()
            .expect("8-byte slice from 32-byte array"),
    );
    let next_base_fee = u64::from_be_bytes(
        bytes[8..16]
            .try_into()
            .expect("8-byte slice from 32-byte array"),
    );
    GasValues {
        gasUsed: gas_used,
        gasUsedSmoothed: gas_used_smoothed,
        nextBaseFee: next_base_fee,
    }
}

/// Conducts system tx to retrieve an average historical gas used value
pub fn retrieve_gas_values<E>(
    block_number: u64,
    evm: &mut E,
) -> Result<GasValues, SystemAccountingError<E::Error>>
where
    E: Evm,
    E::DB: DatabaseCommit,
{
    let call_data = ISystemAccounting::getGasValuesCall {
        blockNumber: block_number,
    }
    .abi_encode();

    let result_and_state = evm
        .transact_system_call(
            ARC_SYSTEM_CALLER,
            SYSTEM_ACCOUNTING_ADDRESS,
            Bytes::from(call_data),
        )
        .map_err(SystemAccountingError::Execution)?;

    if !result_and_state.result.is_success() {
        return Err(SystemAccountingError::Reverted());
    }

    let output = result_and_state
        .result
        .output()
        .ok_or(SystemAccountingError::AbiDecode(
            "No values to decode".to_string(),
        ))?;

    let gas_values = ISystemAccounting::getGasValuesCall::abi_decode_returns(output)
        .map_err(|e| SystemAccountingError::AbiDecode(format!("ABI decode error: {e}")))?;

    Ok(gas_values)
}

/// Conducts a system tx to update a stored average historical gas used value
pub fn store_gas_values<E>(
    block_number: u64,
    gas_values: GasValues,
    evm: &mut E,
) -> Result<EvmState, SystemAccountingError<E::Error>>
where
    E: Evm,
    E::DB: DatabaseCommit,
{
    let call_data = ISystemAccounting::storeGasValuesCall {
        blockNumber: block_number,
        gasValues: gas_values,
    }
    .abi_encode();

    let result_and_state = evm
        .transact_system_call(
            ARC_SYSTEM_CALLER,
            SYSTEM_ACCOUNTING_ADDRESS,
            Bytes::from(call_data),
        )
        .map_err(SystemAccountingError::Execution)?;

    if !result_and_state.result.is_success() {
        return Err(SystemAccountingError::Reverted());
    }

    let output = result_and_state
        .result
        .output()
        .ok_or(SystemAccountingError::AbiDecode(
            "No values to decode".to_string(),
        ))?;

    let decoded = ISystemAccounting::storeGasValuesCall::abi_decode_returns(output)
        .map_err(|e| SystemAccountingError::AbiDecode(e.to_string()))?;

    if !decoded {
        return Err(SystemAccountingError::StoreFailed());
    }

    evm.db_mut().commit(result_and_state.state.clone());

    Ok(result_and_state.state)
}

#[cfg(test)]
mod tests {
    #![allow(unused_imports, dead_code)]
    use super::*;
    use crate::helpers::{
        ERR_DELEGATE_CALL_NOT_ALLOWED, ERR_EXECUTION_REVERTED, ERR_INVALID_CALLER,
        PRECOMPILE_EARLY_REVERT_GAS_PENALTY, PRECOMPILE_SLOAD_GAS_COST, PRECOMPILE_SSTORE_GAS_COST,
        REVERT_SELECTOR,
    };
    use arc_execution_config::hardforks::{ArcHardfork, ArcHardforkFlags};

    // EIP-2929 warm/cold gas costs for Zero5
    const WARM_SLOAD_GAS_COST: u64 = 100;
    // Cold SSTORE (0→non-zero) per EIP-2200
    const COLD_SSTORE_ZERO_TO_NONZERO_GAS_COST: u64 = 22100;
    use alloy_primitives::{address, Bytes, U256};
    use alloy_sol_types::SolValue;
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
    use serde_with::NoneAsEmptyString;

    fn call_system_accounting<DB: revm::database_interface::Database + std::fmt::Debug>(
        ctx: &mut revm::context::Context<
            revm::context::BlockEnv,
            revm::context::TxEnv,
            revm::context::CfgEnv,
            DB,
            revm::context::Journal<DB>,
        >,
        inputs: &CallInputs,
        hardfork_flags: ArcHardforkFlags,
    ) -> Result<Option<InterpreterResult>, String> {
        let mut provider = PrecompilesMap::from_static(Precompiles::latest());
        let target_addr: Address = inputs.target_address;
        provider.set_precompile_lookup(move |address: &Address| {
            if *address == SYSTEM_ACCOUNTING_ADDRESS || target_addr == SYSTEM_ACCOUNTING_ADDRESS {
                Some(DynPrecompile::new_stateful(
                    PrecompileId::Custom("SYSTEM_ACCOUNTING".into()),
                    move |input| run_system_accounting(input, hardfork_flags),
                ))
            } else {
                None
            }
        });
        provider.run(ctx, inputs)
    }

    // Helper to decode revert Error(string)
    fn bytes_to_revert_message(input: &[u8]) -> Option<String> {
        if input.len() < 4 {
            return None;
        }
        if input[0..4] != REVERT_SELECTOR {
            return None;
        }
        String::abi_decode(&input[4..]).ok()
    }

    // Test helpers to simplify calling the precompile within a shared Context
    fn write(
        ctx: &mut Context,
        block_number: u64,
        gas_values: GasValues,
        gas_limit: u64,
    ) -> InterpreterResult {
        let inputs = CallInputs {
            scheme: CallScheme::Call,
            target_address: SYSTEM_ACCOUNTING_ADDRESS,
            bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            known_bytecode: None,
            caller: ARC_SYSTEM_CALLER,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(
                ISystemAccounting::storeGasValuesCall {
                    blockNumber: block_number,
                    gasValues: gas_values,
                }
                .abi_encode()
                .into(),
            ),
            gas_limit,
            is_static: false,
            return_memory_offset: 0..0,
        };

        call_system_accounting(ctx, &inputs, ArcHardforkFlags::with(&[ArcHardfork::Zero5]))
            .unwrap()
            .unwrap()
    }

    fn read(
        ctx: &mut Context,
        block_number: u64,
        gas_limit: u64,
    ) -> (InterpreterResult, GasValues) {
        let inputs = CallInputs {
            scheme: CallScheme::Call,
            target_address: SYSTEM_ACCOUNTING_ADDRESS,
            bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            known_bytecode: None,
            caller: ARC_SYSTEM_CALLER,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(
                ISystemAccounting::getGasValuesCall {
                    blockNumber: block_number,
                }
                .abi_encode()
                .into(),
            ),
            gas_limit,
            is_static: false,
            return_memory_offset: 0..0,
        };

        let res =
            call_system_accounting(ctx, &inputs, ArcHardforkFlags::with(&[ArcHardfork::Zero5]))
                .unwrap()
                .unwrap();
        let decoded = ISystemAccounting::getGasValuesCall::abi_decode_returns(res.output.as_ref())
            .expect("decode getGasValues");
        (res, decoded)
    }

    #[test]
    fn pack_unpack_roundtrip() {
        let samples = [
            GasValues {
                gasUsed: 1,
                gasUsedSmoothed: 2,
                nextBaseFee: 5,
            },
            GasValues {
                gasUsed: 2,
                gasUsedSmoothed: 1,
                nextBaseFee: 0,
            },
            GasValues {
                gasUsed: u64::MAX,
                gasUsedSmoothed: 0,
                nextBaseFee: 100,
            },
            GasValues {
                gasUsed: 0,
                gasUsedSmoothed: u64::MAX,
                nextBaseFee: u64::MAX,
            },
            GasValues {
                gasUsed: 123_456_789,
                gasUsedSmoothed: 987_654_321,
                nextBaseFee: 123_411_331,
            },
        ];

        for g in samples {
            let slot_bytes = pack_gas_values_for_storage(g.clone());
            let unpacked = unpack_gas_values_from_storage(B256::from(slot_bytes));
            assert_eq!(unpacked.gasUsed, g.clone().gasUsed);
            assert_eq!(unpacked.gasUsedSmoothed, g.clone().gasUsedSmoothed);
        }
    }

    #[test]
    fn get_gas_values_failure_case_table_tests() {
        struct GetCase {
            name: &'static str,
            caller: Address,
            calldata: Bytes,
            gas_limit: u64,
            expected_result: InstructionResult,
            expected_revert_str: Option<&'static str>,
            return_data: Option<Bytes>,
            gas_used: u64,
        }

        let block_zero = 0u64;
        let cases: &[GetCase] = &[
            GetCase {
                name: "get() default zero values",
                caller: address!("0x1000000000000000000000000000000000000001"),
                calldata: ISystemAccounting::getGasValuesCall {
                    blockNumber: block_zero,
                }
                .abi_encode()
                .into(),
                gas_limit: PRECOMPILE_SLOAD_GAS_COST,
                expected_result: InstructionResult::Return,
                expected_revert_str: None,
                return_data: Some(
                    GasValues {
                        gasUsed: 0,
                        gasUsedSmoothed: 0,
                        nextBaseFee: 0,
                    }
                    .abi_encode()
                    .into(),
                ),
                gas_used: PRECOMPILE_SLOAD_GAS_COST,
            },
            GetCase {
                name: "get() invalid params reverts",
                caller: ARC_SYSTEM_CALLER,
                calldata: ISystemAccounting::getGasValuesCall::SELECTOR.into(),
                gas_limit: PRECOMPILE_SLOAD_GAS_COST,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                return_data: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
            },
            GetCase {
                name: "get() OOG",
                caller: ARC_SYSTEM_CALLER,
                calldata: ISystemAccounting::getGasValuesCall { blockNumber: 1 }
                    .abi_encode()
                    .into(),
                gas_limit: PRECOMPILE_SLOAD_GAS_COST - 1,
                expected_result: InstructionResult::PrecompileOOG,
                expected_revert_str: None,
                return_data: None,
                gas_used: 0,
            },
        ];

        for tc in cases {
            let mut ctx = Context::mainnet();
            ctx.journal_mut()
                .load_account(SYSTEM_ACCOUNTING_ADDRESS)
                .expect("Unable to load system accounting account");

            // if let Some((bn, val)) = tc.prepopulate_block.clone() {
            //     let slot = super::compute_gas_values_storage_slot(bn);
            //     let stored_u256 = U256::from_be_slice(&pack_gas_values_for_storage(val));
            //     ctx.journal_mut()
            //         .sstore(SYSTEM_ACCOUNTING_ADDRESS, slot.into(), stored_u256)
            //         .expect("sstore prepopulate");
            // }

            let inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: SYSTEM_ACCOUNTING_ADDRESS,
                bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
                known_bytecode: None,
                caller: tc.caller,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(tc.calldata.clone()),
                gas_limit: tc.gas_limit,
                is_static: false,
                return_memory_offset: 0..0,
            };

            let res = call_system_accounting(
                &mut ctx,
                &inputs,
                ArcHardforkFlags::with(&[ArcHardfork::Zero5]),
            )
            .unwrap()
            .unwrap();

            // Result
            assert_eq!(res.result, tc.expected_result, "{}", tc.name);

            // Revert string
            if let Some(expected_revert_str) = tc.expected_revert_str {
                let reason = bytes_to_revert_message(res.output.as_ref()).expect("revert reason");
                assert_eq!(reason, expected_revert_str, "{}", tc.name);
            }

            // Return data
            if let Some(expected_return) = &tc.return_data {
                assert_eq!(res.output, *expected_return, "{}", tc.name);
            }

            // Gas used
            assert_eq!(res.gas.used(), tc.gas_used, "{}", tc.name);
        }
    }

    #[test]
    fn store_gas_values_table_tests() {
        struct StoreCase {
            name: &'static str,
            caller: Address,
            calldata: Bytes,
            gas_limit: u64,
            /// If set, overrides `gas_limit` when Zero6 is active. Needed when
            /// the Zero6 early-revert penalty pushes required gas above the
            /// Zero5 limit.
            zero6_gas_limit: Option<u64>,
            expected_result: InstructionResult,
            expected_revert_str: Option<&'static str>,
            return_data: Option<Bytes>,
            gas_used: u64,
            /// If set, overrides `gas_used` for pre-Zero5 hardforks (fixed
            /// SSTORE cost vs. EIP-2929/EIP-2200 warm/cold pricing).
            pre_zero5_gas_used: Option<u64>,
            /// If set, overrides `gas_used` when Zero6 is active (auth reverts
            /// charge `PRECOMPILE_EARLY_REVERT_GAS_PENALTY`).
            zero6_gas_used: Option<u64>,
            target_address: Address,
            bytecode_address: Address,
        }

        let bn_ok = 1024u64;
        let val_ok = GasValues {
            gasUsed: 11,
            gasUsedSmoothed: 22,
            nextBaseFee: 33,
        };
        // Zero5: 2100 (redundant pre-auth charge, no real SLOAD) + 22100 (cold SSTORE 0→non-zero)
        //        = 24200.
        // Zero6: 22100 only — redundant charge dropped (see `zero6_gas_used` override below).
        // Pre-Zero5 uses the fixed SSTORE path (see `pre_zero5_gas_used` override below).
        let expected_gas_success = PRECOMPILE_SLOAD_GAS_COST + COLD_SSTORE_ZERO_TO_NONZERO_GAS_COST;

        let cases: &[StoreCase] = &[
            StoreCase {
                name: "successful insert",
                caller: ARC_SYSTEM_CALLER,
                calldata: ISystemAccounting::storeGasValuesCall {
                    blockNumber: bn_ok,
                    gasValues: val_ok.clone(),
                }
                .abi_encode()
                .into(),
                gas_limit: expected_gas_success,
                zero6_gas_limit: None,
                expected_result: InstructionResult::Return,
                expected_revert_str: None,
                return_data: Some(true.abi_encode().into()),
                gas_used: expected_gas_success,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST + PRECOMPILE_SSTORE_GAS_COST),
                zero6_gas_used: Some(COLD_SSTORE_ZERO_TO_NONZERO_GAS_COST),
                target_address: SYSTEM_ACCOUNTING_ADDRESS,
                bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            },
            StoreCase {
                name: "invalid calldata reverts",
                caller: ARC_SYSTEM_CALLER,
                calldata: ISystemAccounting::storeGasValuesCall::SELECTOR.into(),
                gas_limit: PRECOMPILE_SLOAD_GAS_COST,
                zero6_gas_limit: None,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                return_data: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                pre_zero5_gas_used: None,
                zero6_gas_used: None,
                target_address: SYSTEM_ACCOUNTING_ADDRESS,
                bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            },
            StoreCase {
                name: "OOG while storing value",
                caller: ARC_SYSTEM_CALLER,
                calldata: ISystemAccounting::storeGasValuesCall {
                    blockNumber: bn_ok,
                    gasValues: val_ok.clone(),
                }
                .abi_encode()
                .into(),
                // Pre-Zero6: OOGs at the redundant 2100-gas pre-auth charge.
                gas_limit: PRECOMPILE_SLOAD_GAS_COST - 1,
                // Zero6: redundant charge dropped, so the next gas-charging point is the
                // cold SSTORE inside `write()`. One gas short of that cost OOGs there.
                zero6_gas_limit: Some(COLD_SSTORE_ZERO_TO_NONZERO_GAS_COST - 1),
                expected_result: InstructionResult::PrecompileOOG,
                expected_revert_str: None,
                return_data: None,
                gas_used: 0,
                pre_zero5_gas_used: None,
                zero6_gas_used: None,
                target_address: SYSTEM_ACCOUNTING_ADDRESS,
                bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            },
            StoreCase {
                name: "reverts from unauthorized caller",
                caller: address!("0x0000000000000000000000000000000000000123"),
                calldata: ISystemAccounting::storeGasValuesCall {
                    blockNumber: bn_ok,
                    gasValues: val_ok.clone(),
                }
                .abi_encode()
                .into(),
                gas_limit: PRECOMPILE_SLOAD_GAS_COST,
                // Zero6: redundant 2100-gas charge dropped, so only the early-revert
                // penalty is consumed. Limit must still cover the penalty exactly.
                zero6_gas_limit: Some(PRECOMPILE_EARLY_REVERT_GAS_PENALTY),
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_INVALID_CALLER),
                return_data: None,
                gas_used: PRECOMPILE_SLOAD_GAS_COST,
                pre_zero5_gas_used: None,
                zero6_gas_used: Some(PRECOMPILE_EARLY_REVERT_GAS_PENALTY),
                target_address: SYSTEM_ACCOUNTING_ADDRESS,
                bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            },
            StoreCase {
                name: "reverts from zero-address caller (legacy system caller)",
                caller: Address::ZERO,
                calldata: ISystemAccounting::storeGasValuesCall {
                    blockNumber: bn_ok,
                    gasValues: val_ok.clone(),
                }
                .abi_encode()
                .into(),
                gas_limit: PRECOMPILE_SLOAD_GAS_COST,
                zero6_gas_limit: Some(PRECOMPILE_EARLY_REVERT_GAS_PENALTY),
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_INVALID_CALLER),
                return_data: None,
                gas_used: PRECOMPILE_SLOAD_GAS_COST,
                pre_zero5_gas_used: None,
                zero6_gas_used: Some(PRECOMPILE_EARLY_REVERT_GAS_PENALTY),
                target_address: SYSTEM_ACCOUNTING_ADDRESS,
                bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            },
            StoreCase {
                name: "reverts if target address != precompile address",
                caller: ARC_SYSTEM_CALLER,
                calldata: ISystemAccounting::storeGasValuesCall {
                    blockNumber: bn_ok,
                    gasValues: val_ok.clone(),
                }
                .abi_encode()
                .into(),
                gas_limit: expected_gas_success,
                zero6_gas_limit: None,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                return_data: None,
                gas_used: PRECOMPILE_SLOAD_GAS_COST,
                pre_zero5_gas_used: None,
                // Zero6: nothing is charged before check_delegatecall reverts (auth passes,
                // redundant pre-auth charge dropped). System-tx callers never delegatecall in
                // production, so the 0-gas exit here is unreachable on real workloads.
                zero6_gas_used: Some(0),
                target_address: address!("0x0000000000000000000000000000000000000123"),
                bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            },
            StoreCase {
                name: "reverts if bytecode address != precompile address",
                caller: ARC_SYSTEM_CALLER,
                calldata: ISystemAccounting::storeGasValuesCall {
                    blockNumber: bn_ok,
                    gasValues: val_ok.clone(),
                }
                .abi_encode()
                .into(),
                gas_limit: expected_gas_success,
                zero6_gas_limit: None,
                expected_result: InstructionResult::Revert,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                return_data: None,
                gas_used: PRECOMPILE_SLOAD_GAS_COST,
                pre_zero5_gas_used: None,
                zero6_gas_used: Some(0),
                target_address: SYSTEM_ACCOUNTING_ADDRESS,
                bytecode_address: address!("0x0000000000000000000000000000000000000123"),
            },
        ];

        for tc in cases {
            for hardfork_flags in ArcHardforkFlags::all_combinations() {
                // ZeroX hardforks are cumulative; Zero6 implies Zero5.
                if hardfork_flags.is_active(ArcHardfork::Zero6)
                    && !hardfork_flags.is_active(ArcHardfork::Zero5)
                {
                    continue;
                }

                let tc_name = format!("{} (hardfork_flags: {:?})", tc.name, hardfork_flags);

                let gas_limit = if hardfork_flags.is_active(ArcHardfork::Zero6) {
                    tc.zero6_gas_limit.unwrap_or(tc.gas_limit)
                } else {
                    tc.gas_limit
                };

                let expected_gas_used = if hardfork_flags.is_active(ArcHardfork::Zero6) {
                    tc.zero6_gas_used.unwrap_or(tc.gas_used)
                } else if hardfork_flags.is_active(ArcHardfork::Zero5) {
                    tc.gas_used
                } else {
                    tc.pre_zero5_gas_used.unwrap_or(tc.gas_used)
                };

                let mut ctx = Context::mainnet();
                ctx.journal_mut()
                    .load_account(SYSTEM_ACCOUNTING_ADDRESS)
                    .expect("Unable to load system accounting account");

                let inputs = CallInputs {
                    scheme: CallScheme::Call,
                    target_address: tc.target_address,
                    bytecode_address: tc.bytecode_address,
                    known_bytecode: None,
                    caller: tc.caller,
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(tc.calldata.clone()),
                    gas_limit,
                    is_static: false,
                    return_memory_offset: 0..0,
                };

                let res = call_system_accounting(&mut ctx, &inputs, hardfork_flags)
                    .unwrap()
                    .unwrap();
                // Check result
                assert_eq!(res.result, tc.expected_result, "{tc_name}");

                // Revert string
                if let Some(expected_revert_str) = tc.expected_revert_str {
                    let reason =
                        bytes_to_revert_message(res.output.as_ref()).expect("revert reason");
                    assert_eq!(reason, expected_revert_str, "{tc_name}");
                }

                // Return data
                if let Some(expected_return) = &tc.return_data {
                    assert_eq!(res.output, *expected_return, "{tc_name}");
                }
                // Gas used
                assert_eq!(res.gas.used(), expected_gas_used, "{tc_name}");
            }
        }
    }

    #[test]
    fn read_write_workflow() {
        let mut ctx = Context::mainnet();
        ctx.journal_mut()
            .load_account(SYSTEM_ACCOUNTING_ADDRESS)
            .expect("Unable to load system accounting account");

        let res = write(
            &mut ctx,
            1,
            GasValues {
                gasUsed: 2,
                gasUsedSmoothed: 3,
                nextBaseFee: 6,
            },
            30_000_000,
        );
        assert_eq!(res.result, InstructionResult::Return);

        // Read the value for the same block - slot is warm after write
        let (res_read, decoded_read) = read(&mut ctx, 1, WARM_SLOAD_GAS_COST);
        assert_eq!(res_read.result, InstructionResult::Return);
        assert_eq!(res_read.gas.used(), WARM_SLOAD_GAS_COST);
        assert_eq!(decoded_read.gasUsed, 2);
        assert_eq!(decoded_read.gasUsedSmoothed, 3);

        // Now loop the ring buffer and overwrite the value
        // Same slot (block 1 % 64 == block 65 % 64), so it's warm
        let res_overwrite = write(
            &mut ctx,
            1 + GAS_VALUES_RING_BUFFER_SIZE,
            GasValues {
                gasUsed: 4,
                gasUsedSmoothed: 5,
                nextBaseFee: 100000000000,
            },
            30_000_000,
        );
        assert_eq!(res_overwrite.result, InstructionResult::Return);

        // Read the value again for the new block - slot still warm
        let (res_read_new_block, decoded_read_new_block) = read(
            &mut ctx,
            1 + GAS_VALUES_RING_BUFFER_SIZE,
            WARM_SLOAD_GAS_COST,
        );
        assert_eq!(res_read_new_block.result, InstructionResult::Return);
        assert_eq!(res_read_new_block.gas.used(), WARM_SLOAD_GAS_COST);
        assert_eq!(decoded_read_new_block.gasUsed, 4);
        assert_eq!(decoded_read_new_block.gasUsedSmoothed, 5);

        // Read the value for the original block number - same slot, still warm
        let (res_read_original_block, decoded_read_original_block) =
            read(&mut ctx, 1, WARM_SLOAD_GAS_COST);
        assert_eq!(res_read_original_block.result, InstructionResult::Return);
        assert_eq!(res_read_original_block.gas.used(), WARM_SLOAD_GAS_COST);
        assert_eq!(decoded_read_original_block.gasUsed, 4);
        assert_eq!(decoded_read_original_block.gasUsedSmoothed, 5);
    }

    /// Under Zero6+, any SSTORE through `helpers::write` must fail with
    /// `PrecompileOOG` and consume zero gas when the remaining gas is at or
    /// below `CALL_STIPEND` (2,300), mirroring revm's `ReentrancySentryOOG`
    /// halt for the SSTORE opcode.
    #[test]
    fn store_gas_values_eip_2200_sentry_zero6() {
        use revm_context_interface::cfg::gas::CALL_STIPEND;

        let zero6_flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5, ArcHardfork::Zero6]);

        let calldata: Bytes = ISystemAccounting::storeGasValuesCall {
            blockNumber: 1,
            gasValues: GasValues {
                gasUsed: 1,
                gasUsedSmoothed: 2,
                nextBaseFee: 3,
            },
        }
        .abi_encode()
        .into();

        let make_inputs = |gas_limit: u64| CallInputs {
            scheme: CallScheme::Call,
            target_address: SYSTEM_ACCOUNTING_ADDRESS,
            bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            known_bytecode: None,
            caller: ARC_SYSTEM_CALLER,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(calldata.clone()),
            gas_limit,
            is_static: false,
            return_memory_offset: 0..0,
        };

        // gas_limit == CALL_STIPEND: sentry fires immediately; no auth check,
        // no journal mutation, no gas consumed.
        for gas_limit in [1, CALL_STIPEND - 1, CALL_STIPEND] {
            let mut ctx = Context::mainnet();
            ctx.journal_mut()
                .load_account(SYSTEM_ACCOUNTING_ADDRESS)
                .expect("load system accounting account");

            let res = call_system_accounting(&mut ctx, &make_inputs(gas_limit), zero6_flags)
                .unwrap()
                .unwrap();

            assert_eq!(
                res.result,
                InstructionResult::PrecompileOOG,
                "Zero6 sentry must OOG at gas_limit={gas_limit}"
            );
            assert_eq!(
                res.gas.used(),
                0,
                "Zero6 sentry must charge zero gas at gas_limit={gas_limit}"
            );
        }

        // gas_limit == CALL_STIPEND + 1: sentry passes; OOG happens later
        // inside `write()` at the cold-SSTORE dynamic charge. Externally still
        // PrecompileOOG, but proves the sentry boundary is exclusive.
        let mut ctx = Context::mainnet();
        ctx.journal_mut()
            .load_account(SYSTEM_ACCOUNTING_ADDRESS)
            .expect("load system accounting account");
        let res = call_system_accounting(&mut ctx, &make_inputs(CALL_STIPEND + 1), zero6_flags)
            .unwrap()
            .unwrap();
        assert_eq!(res.result, InstructionResult::PrecompileOOG);

        // Sanity: with enough gas the same call succeeds and the sentry is a
        // no-op on the happy path.
        let happy_gas = COLD_SSTORE_ZERO_TO_NONZERO_GAS_COST;
        let mut ctx = Context::mainnet();
        ctx.journal_mut()
            .load_account(SYSTEM_ACCOUNTING_ADDRESS)
            .expect("load system accounting account");
        let res = call_system_accounting(&mut ctx, &make_inputs(happy_gas), zero6_flags)
            .unwrap()
            .unwrap();
        assert_eq!(res.result, InstructionResult::Return);
        assert_eq!(res.gas.used(), happy_gas);
    }

    /// Under Zero6, `read()` probes slot warmth with `sload(key, true)`.
    /// When the slot is cold the probe returns `ColdLoadSkipped` and the
    /// helper must charge `COLD_SLOAD_COST` (2100) *before* retrying with
    /// the real DB load.
    ///
    /// This test gives exactly `COLD_SLOAD_COST - 1` gas so the charge
    /// fails before the retry I/O. A bug that deferred the charge would
    /// succeed at this gas level (warm cost = 100 < 2099).
    ///
    /// Uses `TrackingDB` to prove zero storage reads occur before the OOG.
    #[test]
    fn read_cold_slot_oog_before_retry() {
        use crate::helpers::test_utils::TrackingDB;
        use revm_interpreter::gas::COLD_SLOAD_COST;

        let zero6_flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5, ArcHardfork::Zero6]);

        let calldata: Bytes = ISystemAccounting::getGasValuesCall { blockNumber: 42 }
            .abi_encode()
            .into();
        let make_inputs = |gas_limit: u64| CallInputs {
            scheme: CallScheme::Call,
            target_address: SYSTEM_ACCOUNTING_ADDRESS,
            bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            known_bytecode: None,
            caller: ARC_SYSTEM_CALLER,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(calldata.clone()),
            gas_limit,
            is_static: false,
            return_memory_offset: 0..0,
        };

        // Gas = COLD_SLOAD_COST - 1: must OOG at the cold charge, zero DB reads.
        let (mut ctx, storage_reads) = TrackingDB::context();
        ctx.journal_mut()
            .load_account(SYSTEM_ACCOUNTING_ADDRESS)
            .expect("load");
        let res = call_system_accounting(&mut ctx, &make_inputs(COLD_SLOAD_COST - 1), zero6_flags)
            .unwrap()
            .unwrap();
        assert_eq!(
            res.result,
            InstructionResult::PrecompileOOG,
            "cold read must OOG when gas < COLD_SLOAD_COST"
        );
        assert_eq!(
            storage_reads.get(),
            0,
            "OOG must occur before any storage DB read"
        );

        // Gas = COLD_SLOAD_COST: must succeed (slot is cold, value is zero).
        let (mut ctx, storage_reads) = TrackingDB::context();
        ctx.journal_mut()
            .load_account(SYSTEM_ACCOUNTING_ADDRESS)
            .expect("load");
        let res = call_system_accounting(&mut ctx, &make_inputs(COLD_SLOAD_COST), zero6_flags)
            .unwrap()
            .unwrap();
        assert_eq!(
            res.result,
            InstructionResult::Return,
            "cold read must succeed at exactly COLD_SLOAD_COST"
        );
        assert_eq!(res.gas.used(), COLD_SLOAD_COST);
        assert!(
            storage_reads.get() > 0,
            "success path must hit the DB for the real sload"
        );
    }

    /// Under Zero6, `write()` probes slot warmth with `sload(key, true)`.
    /// When the slot is cold the probe returns `ColdLoadSkipped` and the
    /// helper charges `COLD_SLOAD_COST` before retrying. This test verifies
    /// that the cold charge + sstore base cost are both applied correctly
    /// on a cold slot by testing at the exact boundary.
    ///
    /// For a 0→non-zero write: total = COLD_SLOAD_COST (2100) + SSTORE_SET
    /// (20000) = 22100. Gas at 22099 must OOG; gas at 22100 must succeed.
    /// The EIP-2200 sentry (CALL_STIPEND=2300) is below COLD_SLOAD_COST, so
    /// it never gates the cold-load charge for 0→non-zero writes.
    ///
    /// Uses `TrackingDB` to prove zero storage reads occur before the OOG.
    #[test]
    fn write_cold_slot_oog_at_base_cost_boundary() {
        use crate::helpers::test_utils::TrackingDB;
        use revm_interpreter::gas::COLD_SLOAD_COST;

        let zero6_flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5, ArcHardfork::Zero6]);
        // 0→non-zero base cost is SSTORE_SET (20000); total = COLD_SLOAD_COST + 20000
        let exact_cost = COLD_SLOAD_COST + 20000;

        let calldata: Bytes = ISystemAccounting::storeGasValuesCall {
            blockNumber: 99,
            gasValues: GasValues {
                gasUsed: 1,
                gasUsedSmoothed: 2,
                nextBaseFee: 3,
            },
        }
        .abi_encode()
        .into();
        let make_inputs = |gas_limit: u64| CallInputs {
            scheme: CallScheme::Call,
            target_address: SYSTEM_ACCOUNTING_ADDRESS,
            bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
            known_bytecode: None,
            caller: ARC_SYSTEM_CALLER,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(calldata.clone()),
            gas_limit,
            is_static: false,
            return_memory_offset: 0..0,
        };

        // Gas = COLD_SLOAD_COST - 1: OOG at the cold charge, before the sload DB read.
        let (mut ctx, storage_reads) = TrackingDB::context();
        ctx.journal_mut()
            .load_account(SYSTEM_ACCOUNTING_ADDRESS)
            .expect("load");
        let res = call_system_accounting(&mut ctx, &make_inputs(COLD_SLOAD_COST - 1), zero6_flags)
            .unwrap()
            .unwrap();
        assert_eq!(
            res.result,
            InstructionResult::PrecompileOOG,
            "cold write must OOG when gas < COLD_SLOAD_COST"
        );
        assert_eq!(
            storage_reads.get(),
            0,
            "OOG at cold charge must occur before any storage DB read"
        );

        // One gas short of full cost: OOG at sstore base cost, after the sload
        // (whose gas was already charged).
        let (mut ctx, storage_reads) = TrackingDB::context();
        ctx.journal_mut()
            .load_account(SYSTEM_ACCOUNTING_ADDRESS)
            .expect("load");
        let res = call_system_accounting(&mut ctx, &make_inputs(exact_cost - 1), zero6_flags)
            .unwrap()
            .unwrap();
        assert_eq!(
            res.result,
            InstructionResult::PrecompileOOG,
            "cold write one gas short of sstore must OOG"
        );
        assert_eq!(
            storage_reads.get(),
            1,
            "sload DB read happens after cold charge was paid"
        );

        // Exact cost: must succeed.
        let (mut ctx, storage_reads) = TrackingDB::context();
        ctx.journal_mut()
            .load_account(SYSTEM_ACCOUNTING_ADDRESS)
            .expect("load");
        let res = call_system_accounting(&mut ctx, &make_inputs(exact_cost), zero6_flags)
            .unwrap()
            .unwrap();
        assert_eq!(
            res.result,
            InstructionResult::Return,
            "cold write at exact cost must succeed"
        );
        assert_eq!(res.gas.used(), exact_cost);
        assert!(
            storage_reads.get() > 0,
            "success path must hit the DB for the real sload"
        );
    }

    #[test]
    fn test_compute_gas_values_storage_slot() {
        use super::compute_gas_values_storage_slot;

        const EXPECTED_KEY_FOR_SLOT_0: &str =
            "0xa6eef7e35abe7026729641147f7915573c7e97b47efa546f5f6e3230263bcb49";
        const EXPECTED_KEY_FOR_SLOT_1: &str =
            "0xcc69885fda6bcc1a4ace058b4a62bf5e179ea78fd58a1ccd71c22cc9b688792f";

        // Test basic block number mapping
        let slot_0 = compute_gas_values_storage_slot(0);
        assert_eq!(slot_0.to_string(), EXPECTED_KEY_FOR_SLOT_0);
        let slot_1 = compute_gas_values_storage_slot(1);
        assert_eq!(slot_1.to_string(), EXPECTED_KEY_FOR_SLOT_1);

        // Test ring buffer wrapping (64 block ring buffer)
        let slot_64 = compute_gas_values_storage_slot(GAS_VALUES_RING_BUFFER_SIZE);
        assert_eq!(slot_64.to_string(), EXPECTED_KEY_FOR_SLOT_0);
        let slot_65 = compute_gas_values_storage_slot(1 + GAS_VALUES_RING_BUFFER_SIZE);
        assert_eq!(slot_65.to_string(), EXPECTED_KEY_FOR_SLOT_1);
    }

    sol! {
        struct GasValues_Zero3 {
            uint64 gasUsed;
            uint64 gasUsedSmoothed;
        }

        interface ISystemAccounting_Zero3 {
            function storeGasValues(uint64 blockNumber, GasValues_Zero3 calldata gasValues) external returns (bool);
            function getGasValues(uint64 blockNumber) external view returns (GasValues_Zero3 calldata gasValue);
        }
    }

    #[test]
    fn system_accounting_slot_value_compatibility() {
        /// Packs GasValues into a single 32-byte storage slot
        /// The layout is:
        /// - `gasUsedSmoothed` (u64): bytes [16..24]
        /// - `gasUsed` (u64):         bytes [24..32]
        fn pack_gas_values_for_storage_zero3(g: GasValues_Zero3) -> [u8; 32] {
            let mut slot = [0u8; 32];
            slot[24..32].copy_from_slice(&g.gasUsed.to_be_bytes());
            slot[16..24].copy_from_slice(&g.gasUsedSmoothed.to_be_bytes());
            slot
        }

        fn unpack_gas_values_from_storage_zero3(slot: B256) -> GasValues_Zero3 {
            let bytes = slot.as_slice();
            let gas_used = u64::from_be_bytes(bytes[24..32].try_into().unwrap());
            let gas_used_smoothed = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
            GasValues_Zero3 {
                gasUsed: gas_used,
                gasUsedSmoothed: gas_used_smoothed,
            }
        }

        for gas_value in [
            GasValues_Zero3 {
                gasUsed: 123,
                gasUsedSmoothed: 456,
            },
            GasValues_Zero3 {
                gasUsed: 0,
                gasUsedSmoothed: 0,
            },
            GasValues_Zero3 {
                gasUsed: u64::MAX,
                gasUsedSmoothed: u64::MAX,
            },
        ] {
            let value = pack_gas_values_for_storage_zero3(gas_value.clone());
            let unpacked = unpack_gas_values_from_storage_zero3(B256::from(value));
            assert_eq!(unpacked.gasUsed, gas_value.gasUsed);
            assert_eq!(unpacked.gasUsedSmoothed, gas_value.gasUsedSmoothed);

            // The slot value pack/unpack is compatible.
            let unpacked_new = unpack_gas_values_from_storage(B256::from(value));
            assert_eq!(unpacked_new.gasUsed, gas_value.gasUsed);
            assert_eq!(unpacked_new.gasUsedSmoothed, gas_value.gasUsedSmoothed);
            assert_eq!(unpacked_new.nextBaseFee, 0);
        }
    }

    #[test]
    fn system_accounting_interface_incompatible() {
        let output: Bytes = GasValues_Zero3 {
            gasUsed: 123,
            gasUsedSmoothed: 456,
        }
        .abi_encode()
        .into();
        assert_eq!(output.to_string(), "0x000000000000000000000000000000000000000000000000000000000000007b00000000000000000000000000000000000000000000000000000000000001c8");

        let output_new: Bytes = GasValues {
            gasUsed: 123,
            gasUsedSmoothed: 456,
            nextBaseFee: 0,
        }
        .abi_encode()
        .into();
        assert_eq!(output_new.to_string(), "0x000000000000000000000000000000000000000000000000000000000000007b00000000000000000000000000000000000000000000000000000000000001c80000000000000000000000000000000000000000000000000000000000000000");

        assert_ne!(output, output_new);
    }

    #[test]
    fn test_static_call_reverts_store_gas_values() {
        use crate::helpers::ERR_STATE_CHANGE_DURING_STATIC_CALL;

        for hardfork_flags in ArcHardforkFlags::all_combinations() {
            let mut ctx = Context::mainnet();
            ctx.journal_mut()
                .load_account(SYSTEM_ACCOUNTING_ADDRESS)
                .expect("Unable to load system accounting account");

            // State-modifying function (storeGasValues) must revert under static call
            let inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: SYSTEM_ACCOUNTING_ADDRESS,
                bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
                known_bytecode: None,
                caller: ARC_SYSTEM_CALLER,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(
                    ISystemAccounting::storeGasValuesCall {
                        blockNumber: 1,
                        gasValues: GasValues {
                            gasUsed: 100,
                            gasUsedSmoothed: 200,
                            nextBaseFee: 50,
                        },
                    }
                    .abi_encode()
                    .into(),
                ),
                gas_limit: 100_000,
                is_static: true,
                return_memory_offset: 0..0,
            };

            let result = call_system_accounting(&mut ctx, &inputs, hardfork_flags)
                .expect("call should not error")
                .expect("result should be Some");

            assert_eq!(
                result.result,
                InstructionResult::Revert,
                "storeGasValues ({hardfork_flags:?}): expected Revert under static call",
            );
            let revert_reason = bytes_to_revert_message(result.output.as_ref());
            assert_eq!(
                revert_reason.as_deref(),
                Some(ERR_STATE_CHANGE_DURING_STATIC_CALL),
                "storeGasValues ({hardfork_flags:?}): wrong revert reason",
            );

            // Read-only function (getGasValues) must succeed under static call
            let read_inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: SYSTEM_ACCOUNTING_ADDRESS,
                bytecode_address: SYSTEM_ACCOUNTING_ADDRESS,
                known_bytecode: None,
                caller: ARC_SYSTEM_CALLER,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(
                    ISystemAccounting::getGasValuesCall { blockNumber: 1 }
                        .abi_encode()
                        .into(),
                ),
                gas_limit: 100_000,
                is_static: true,
                return_memory_offset: 0..0,
            };

            let result = call_system_accounting(&mut ctx, &read_inputs, hardfork_flags)
                .expect("call should not error")
                .expect("result should be Some");

            assert_eq!(
                result.result,
                InstructionResult::Return,
                "getGasValues ({hardfork_flags:?}): expected Return under static call",
            );
        }
    }
}
