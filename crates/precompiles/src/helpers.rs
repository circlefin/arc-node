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

use alloy_evm::EvmInternals;
use alloy_primitives::{Address, Bytes, StorageKey, U256};
use alloy_sol_types::{SolEvent, SolValue};
use reth_ethereum::evm::revm::precompile::{PrecompileError, PrecompileOutput};
use reth_evm::precompiles::PrecompileInput;
use revm::context_interface::journaled_state::TransferError;
use revm::state::AccountInfo;
use revm_interpreter::Gas;
use revm_primitives::address;
use revm_primitives::constants::KECCAK_EMPTY;

use arc_execution_config::hardforks::{ArcHardfork, ArcHardforkFlags};

// system addresses in genesis
pub const NATIVE_FIAT_TOKEN_ADDRESS: Address =
    address!("0x3600000000000000000000000000000000000000");

/// Selector for the Solidity Error(string) format used in revert messages.
pub const REVERT_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];

/// Approximate gas costs for precompile read / writes
pub const PRECOMPILE_SSTORE_GAS_COST: u64 = 2900;
pub const PRECOMPILE_SLOAD_GAS_COST: u64 = 2100;

/// Gas costs for emitting a log
pub const LOG_BASE_COST: u64 = 375; // Base cost for emitting a log
pub const LOG_TOPIC_COST: u64 = 375; // Cost per log topic
pub const LOG_DATA_COST: u64 = 8; // Cost per byte of log data

/// Common precompile revert messages
pub const ERR_EXECUTION_REVERTED: &str = "Execution reverted";
pub const ERR_INSUFFICIENT_FUNDS: &str = "Insufficient funds";
pub const ERR_OVERFLOW: &str = "Arithmetic overflow";
pub const ERR_INVALID_CALLER: &str = "Invalid caller";
pub const ERR_CLEAR_EMPTY: &str = "Cannot clear balance of empty account";
pub const ERR_DELEGATE_CALL_NOT_ALLOWED: &str = "Delegate call not allowed";
pub const ERR_STATE_CHANGE_DURING_STATIC_CALL: &str = "State change during static call";
pub const ERR_BLOCKED_ADDRESS: &str = "Blocked address";
pub const ERR_ZERO_ADDRESS: &str = "Zero address not allowed";
pub const ERR_SELFDESTRUCTED_BALANCE_INCREASED: &str =
    "Cannot increase the balance of selfdestructed account";

/// Encodes a revert error string into ABI‑encoded bytes according to Solidity’s Error(string) format.
///
/// The returned bytes consist of:
/// - 4 bytes selector: 0x08c379a0
/// - ABI-encoded string value of the error message.
pub fn revert_message_to_bytes(msg: &str) -> Bytes {
    let encoded = msg.abi_encode();
    let mut result = Vec::with_capacity(4 + encoded.len());
    result.extend_from_slice(&REVERT_SELECTOR);
    result.extend_from_slice(&encoded);
    Bytes::from(result)
}

/// Gas penalty for ABI decode revert (invalid selector, etc)
/// In normal cases we didn't record this cost, but when reverted, add this penalty to the gas usage.
pub(crate) const PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY: u64 = 200;

/// Enum to represent either a reverted precompile output or an error
pub(crate) enum PrecompileErrorOrRevert {
    Revert(PrecompileOutput),
    Error(PrecompileError),
}

impl PrecompileErrorOrRevert {
    pub(crate) fn new_reverted(gas_counter: Gas, msg: &str) -> Self {
        Self::Revert(PrecompileOutput::new_reverted(
            gas_counter.used(),
            revert_message_to_bytes(msg),
        ))
    }

    pub(crate) fn new_reverted_with_penalty(gas_counter: Gas, gas_penalty: u64, msg: &str) -> Self {
        let mut gas_with_penalty = gas_counter;
        if !gas_with_penalty.record_cost(gas_penalty) {
            return Self::Error(PrecompileError::OutOfGas);
        }
        Self::Revert(PrecompileOutput::new_reverted(
            gas_with_penalty.used(),
            revert_message_to_bytes(msg),
        ))
    }
}

pub(crate) fn record_cost_or_out_of_gas(
    gas_counter: &mut Gas,
    cost: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    if !gas_counter.record_cost(cost) {
        return Err(PrecompileErrorOrRevert::Error(PrecompileError::OutOfGas));
    }
    Ok(())
}

pub(crate) fn check_gas_remaining(
    gas_counter: &Gas,
    cost: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    if gas_counter.remaining() < cost {
        return Err(PrecompileErrorOrRevert::Error(PrecompileError::OutOfGas));
    }
    Ok(())
}

impl From<PrecompileErrorOrRevert> for Result<PrecompileOutput, PrecompileError> {
    fn from(val: PrecompileErrorOrRevert) -> Self {
        match val {
            PrecompileErrorOrRevert::Revert(output) => Ok(output.reverted()),
            PrecompileErrorOrRevert::Error(error) => Err(error),
        }
    }
}

/// Reads a value from storage for stateful precompiles.
///
/// # Parameters
/// - `internals`: The execution context with journal access
/// - `address`: The address whose storage to read from
/// - `storage_key`: The storage slot to read
/// - `gas_counter`: Available gas for this operation
/// - `hardfork`: The current hardfork for gas calculation
///
/// # Gas Cost
/// - Pre-Zero5: Fixed cost of 2,100 gas units
/// - Zero5+: EIP-2929 warm/cold aware (100 warm, 2100 cold)
///
/// # Returns
/// - `Ok(Bytes)`: The stored value as big-endian bytes
/// - `Err(PrecompileErrorOrRevert)`: If out of gas or storage read fails
///
/// # Example
/// ```rust,ignore
/// let output = read(internals, precompile_address, StorageKey::ZERO, gas_counter, &hardfork)?;
/// let value = U256::from_be_slice(&output);
/// ```
pub(crate) fn read(
    internals: &mut EvmInternals,
    address: Address,
    storage_key: StorageKey,
    gas_counter: &mut Gas,
    hardfork_flags: ArcHardforkFlags,
) -> Result<Bytes, PrecompileErrorOrRevert> {
    // Read from storage using the journal to get value and is_cold flag
    let state_load = internals.sload(address, storage_key.into()).map_err(|e| {
        PrecompileErrorOrRevert::Error(PrecompileError::Other(
            format!("Storage read failed: {e:?}").into(),
        ))
    })?;

    // Calculate gas based on hardfork - Zero5+ uses EIP-2929 warm/cold pricing
    let gas_cost = if hardfork_flags.is_active(ArcHardfork::Zero5) {
        if state_load.is_cold {
            revm_interpreter::gas::COLD_SLOAD_COST
        } else {
            revm_interpreter::gas::WARM_STORAGE_READ_COST
        }
    } else {
        PRECOMPILE_SLOAD_GAS_COST
    };

    record_cost_or_out_of_gas(gas_counter, gas_cost)?;
    Ok(state_load.data.to_be_bytes_vec().into())
}

/// Writes a value to storage for stateful precompiles.
///
/// # Parameters
/// - `internals`: The execution context with journal access
/// - `address`: The address whose storage to write to
/// - `storage_key`: The storage slot to write
/// - `input`: The value to store (as big-endian bytes)
/// - `gas_counter`: Available gas for this operation
/// - `hardfork`: The current hardfork for gas calculation
///
/// # Gas Cost
/// - Pre-Zero5: Fixed cost of 2,900 gas units
/// - Zero5+: EIP-2929/EIP-2200 aware (varies based on warm/cold and value changes)
///
/// # Returns
/// - `Ok(())`: Success
/// - `Err(PrecompileErrorOrRevert)`: If out of gas or storage write fails
///
/// # Example
/// ```rust,ignore
/// let new_value = U256::from(42);
/// write(
///     internals,
///     precompile_address,
///     StorageKey::ZERO,
///     &new_value.to_be_bytes_vec(),
///     gas_counter,
///     &hardfork
/// )?;
/// ```
pub(crate) fn write(
    internals: &mut EvmInternals,
    address: Address,
    storage_key: StorageKey,
    input: &[u8],
    gas_counter: &mut Gas,
    hardfork_flags: ArcHardforkFlags,
) -> Result<(), PrecompileErrorOrRevert> {
    // Parse the input as a U256 value
    let value = U256::from_be_slice(input);

    // Store the value in the precompile's storage and get result with is_cold flag
    let sstore_result = internals
        .sstore(address, storage_key.into(), value)
        .map_err(|e| {
            PrecompileErrorOrRevert::Error(PrecompileError::Other(
                format!("Storage write failed: {e:?}").into(),
            ))
        })?;

    // Calculate gas based on hardfork - Zero5+ uses EIP-2929/EIP-2200 pricing
    let gas_cost = if hardfork_flags.is_active(ArcHardfork::Zero5) {
        // Berlin-era sstore cost (EIP-2929 + EIP-2200)
        // Mirrors revm v29 istanbul_sstore_cost<WARM_STORAGE_READ_COST, WARM_SSTORE_RESET>
        let vals = &sstore_result.data;
        let base_cost = if vals.is_new_eq_present() {
            revm_interpreter::gas::WARM_STORAGE_READ_COST
        } else if vals.is_original_eq_present() {
            if vals.is_original_zero() {
                20000 // SSTORE_SET
            } else {
                5000 - revm_interpreter::gas::COLD_SLOAD_COST // WARM_SSTORE_RESET
            }
        } else {
            revm_interpreter::gas::WARM_STORAGE_READ_COST
        };
        if sstore_result.is_cold {
            base_cost + revm_interpreter::gas::COLD_SLOAD_COST
        } else {
            base_cost
        }
    } else {
        PRECOMPILE_SSTORE_GAS_COST
    };

    record_cost_or_out_of_gas(gas_counter, gas_cost)?;
    Ok(())
}

/// Helper to transfer funds between two accounts using the Journal
pub(crate) fn transfer(
    internals: &mut EvmInternals,
    from: Address,
    to: Address,
    amount: U256,
    gas_counter: &mut Gas,
    is_burn: bool,
    check_selfdestructed: bool,
) -> Result<(), PrecompileErrorOrRevert> {
    record_cost_or_out_of_gas(gas_counter, PRECOMPILE_SLOAD_GAS_COST)?;
    let loaded_from_account = internals.load_account(from).map_err(|_| {
        PrecompileErrorOrRevert::Error(PrecompileError::Other(ERR_EXECUTION_REVERTED.into()))
    })?;

    // Check that the account can be decremented by the amount
    check_can_decr_account(&loaded_from_account.info, amount, gas_counter)?;

    // Overflow checking is handled by the Journal and the TransferError
    // returned
    // Here we stack the STORE gas cost, mimicking the prior balance_decr + balance_incr calls,
    // where we charged:
    // SLOAD, SSTORE
    // SLOAD, SSTORE
    // For burns, we only charge the first SLOAD, SSTORE to mimick balance_decr
    record_cost_or_out_of_gas(gas_counter, PRECOMPILE_SSTORE_GAS_COST)?;
    if !is_burn {
        record_cost_or_out_of_gas(gas_counter, PRECOMPILE_SLOAD_GAS_COST)?;
        record_cost_or_out_of_gas(gas_counter, PRECOMPILE_SSTORE_GAS_COST)?;
    }

    if check_selfdestructed {
        let to_account = internals.load_account(to).map_err(|_| {
            PrecompileErrorOrRevert::Error(PrecompileError::Other(ERR_EXECUTION_REVERTED.into()))
        })?;
        if to_account.is_selfdestructed() {
            return Err(PrecompileErrorOrRevert::new_reverted(
                *gas_counter,
                ERR_SELFDESTRUCTED_BALANCE_INCREASED,
            ));
        }
    }

    let transfer_result = internals.transfer(from, to, amount).map_err(|_e| {
        PrecompileErrorOrRevert::new_reverted(*gas_counter, ERR_EXECUTION_REVERTED)
    })?;

    match transfer_result {
        None => Ok(()),
        Some(error) => match error {
            // This should never be hit, due to the check prior
            TransferError::OutOfFunds => Err(PrecompileErrorOrRevert::new_reverted(
                *gas_counter,
                ERR_INSUFFICIENT_FUNDS,
            )),
            TransferError::OverflowPayment => Err(PrecompileErrorOrRevert::new_reverted(
                *gas_counter,
                ERR_OVERFLOW,
            )),
            TransferError::CreateCollision => Err(PrecompileErrorOrRevert::new_reverted(
                *gas_counter,
                ERR_EXECUTION_REVERTED,
            )),
        },
    }
}

/// Helper to increment an account's balance by an amount using the Journal
pub(crate) fn balance_incr(
    internals: &mut EvmInternals,
    to: Address,
    amount: U256,
    gas_counter: &mut Gas,
    check_selfdestructed: bool,
) -> Result<(), PrecompileErrorOrRevert> {
    record_cost_or_out_of_gas(gas_counter, PRECOMPILE_SLOAD_GAS_COST)?;

    // Balance check, but doesn't touch state
    let account = internals.load_account(to).map_err(|_| {
        PrecompileErrorOrRevert::Error(PrecompileError::Other(ERR_EXECUTION_REVERTED.into()))
    })?;

    if check_selfdestructed && account.is_selfdestructed() {
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            ERR_SELFDESTRUCTED_BALANCE_INCREASED,
        ));
    }

    let account_balance = account.info.balance;
    account_balance
        .checked_add(amount)
        .ok_or(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            ERR_OVERFLOW,
        ))?;

    // Update state
    record_cost_or_out_of_gas(gas_counter, PRECOMPILE_SSTORE_GAS_COST)?;
    internals.balance_incr(to, amount).map_err(|_| {
        PrecompileErrorOrRevert::Error(PrecompileError::Other(ERR_EXECUTION_REVERTED.into()))
    })?;

    Ok(())
}

/// Helper to decrement an account's balance by an amount using the Journal
pub(crate) fn balance_decr(
    internals: &mut EvmInternals,
    from: Address,
    amount: U256,
    gas_counter: &mut Gas,
) -> Result<(), PrecompileErrorOrRevert> {
    record_cost_or_out_of_gas(gas_counter, PRECOMPILE_SLOAD_GAS_COST)?;
    let loaded_from_account = internals.load_account(from).map_err(|_| {
        PrecompileErrorOrRevert::Error(PrecompileError::Other(ERR_EXECUTION_REVERTED.into()))
    })?;

    // Check that the account can be decremented by the amount
    check_can_decr_account(&loaded_from_account.info, amount, gas_counter)?;

    // Perform the decrement
    record_cost_or_out_of_gas(gas_counter, PRECOMPILE_SSTORE_GAS_COST)?;
    let mut account = internals.load_account_mut(from).map_err(|_| {
        PrecompileErrorOrRevert::Error(PrecompileError::Other(ERR_EXECUTION_REVERTED.into()))
    })?;

    // False is only returned if insufficient funds, which should theoretically anyways never be reached due to the prior check
    if !account.decr_balance(amount) {
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            ERR_INSUFFICIENT_FUNDS,
        ));
    }

    Ok(())
}

/// Helper to prevent state modifications during static calls
pub(crate) fn check_staticcall(
    precompile_input: &PrecompileInput,
    gas_counter: &mut Gas,
) -> Result<(), PrecompileErrorOrRevert> {
    if precompile_input.is_static {
        // Spend all remaining gas
        gas_counter.spend_all();
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            ERR_STATE_CHANGE_DURING_STATIC_CALL,
        ));
    }
    Ok(())
}

/// Helper to check delegatecall
pub(crate) fn check_delegatecall(
    precompile_address: Address,
    precompile_input: &PrecompileInput,
    gas_counter: &mut Gas,
) -> Result<(), PrecompileErrorOrRevert> {
    if precompile_input.target_address != precompile_address
        || precompile_input.bytecode_address != precompile_address
    {
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            ERR_DELEGATE_CALL_NOT_ALLOWED,
        ));
    }
    Ok(())
}

/// Helper to determine if an account can be decremented by an amount
/// Decrements gas counter if account would be emptied
pub(crate) fn check_can_decr_account(
    loaded_account_info: &AccountInfo,
    amount: U256,
    gas_counter: &mut Gas,
) -> Result<(), PrecompileErrorOrRevert> {
    // Check that the account has sufficient balance
    let from_account_balance = loaded_account_info.balance.checked_sub(amount).ok_or(
        PrecompileErrorOrRevert::new_reverted(*gas_counter, ERR_INSUFFICIENT_FUNDS),
    )?;

    // Check that the account would not be emptied if this transfer goes through
    let from_account_is_empty = from_account_balance.is_zero()
        && loaded_account_info.nonce == 0
        && (loaded_account_info.code_hash() == KECCAK_EMPTY
            || loaded_account_info.code_hash().is_zero());

    if from_account_is_empty {
        record_cost_or_out_of_gas(gas_counter, PRECOMPILE_SSTORE_GAS_COST)?;
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            ERR_CLEAR_EMPTY,
        ));
    }

    Ok(())
}

/// Stores a log event in the journal
pub(crate) fn emit_event<Event: SolEvent>(
    internals: &mut EvmInternals,
    address: Address,
    event: &Event,
    gas_counter: &mut Gas,
) -> Result<(), PrecompileErrorOrRevert> {
    let data = event.encode_log_data();

    record_cost_or_out_of_gas(
        gas_counter,
        LOG_BASE_COST
            + LOG_TOPIC_COST * data.topics().len() as u64
            + LOG_DATA_COST * data.data.len() as u64,
    )?;

    let log = revm::primitives::Log { address, data };

    internals.log(log);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use revm_primitives::B256;

    // Generated 11/30/2025 with AI assistance
    #[test]
    fn test_check_can_decr_account() {
        struct TestCase {
            name: &'static str,
            balance: U256,
            nonce: u64,
            code_hash: [u8; 32],
            decr_amount: U256,
            expect_revert: bool,
            revert_message: &'static str,
            expected_gas_used: u64,
        }

        let testcases = vec![
            TestCase {
                name: "insufficient_funds_reverts_for_non-empty_account",
                balance: U256::from(100),
                nonce: 1,
                code_hash: *KECCAK_EMPTY,
                decr_amount: U256::from(101),
                expect_revert: true,
                revert_message: ERR_INSUFFICIENT_FUNDS,
                expected_gas_used: 0,
            },
            TestCase {
                name: "insufficient_funds_reverts_for_empty_account_with_KECCAK_EMPTY_code_hash",
                balance: U256::from(100),
                nonce: 0,
                code_hash: *KECCAK_EMPTY,
                decr_amount: U256::from(101),
                expect_revert: true,
                revert_message: ERR_INSUFFICIENT_FUNDS,
                expected_gas_used: 0,
            },
            TestCase {
                name: "insufficient_funds_reverts_for_empty_account_with_zero_code_hash",
                balance: U256::from(100),
                nonce: 0,
                code_hash: B256::ZERO.into(),
                decr_amount: U256::from(101),
                expect_revert: true,
                revert_message: ERR_INSUFFICIENT_FUNDS,
                expected_gas_used: 0,
            },
            TestCase {
                name: "custom_revert_if_account_will_be_empty_with_KECCAK_EMPTY_code_hash",
                balance: U256::from(100),
                nonce: 0,
                code_hash: *KECCAK_EMPTY,
                decr_amount: U256::from(100),
                expect_revert: true,
                revert_message: ERR_CLEAR_EMPTY,
                expected_gas_used: PRECOMPILE_SSTORE_GAS_COST,
            },
            TestCase {
                name: "custom_revert_if_account_will_be_empty_with_zero_code_hash",
                balance: U256::from(100),
                nonce: 0,
                code_hash: B256::ZERO.into(),
                decr_amount: U256::from(100),
                expect_revert: true,
                revert_message: ERR_CLEAR_EMPTY,
                expected_gas_used: PRECOMPILE_SSTORE_GAS_COST,
            },
            TestCase {
                name: "can_clear_account_with_non-zero_nonce",
                balance: U256::from(100),
                nonce: 1,
                code_hash: *KECCAK_EMPTY,
                decr_amount: U256::from(100),
                expect_revert: false,
                revert_message: "",
                expected_gas_used: 0,
            },
            TestCase {
                name: "can_clear_account_with_non-empty_code_hash",
                balance: U256::from(100),
                nonce: 0,
                code_hash: B256::from([1u8; 32]).into(),
                decr_amount: U256::from(100),
                expect_revert: false,
                revert_message: "",
                expected_gas_used: 0,
            },
            TestCase {
                name: "account_with_sufficient_funds_can_be_decremented",
                balance: U256::from(100),
                nonce: 0,
                code_hash: *KECCAK_EMPTY,
                decr_amount: U256::from(99),
                expect_revert: false,
                revert_message: "",
                expected_gas_used: 0,
            },
        ];

        for tc in testcases {
            let mut gas_counter = Gas::new(1_000_000);
            let account_info = AccountInfo {
                balance: tc.balance,
                nonce: tc.nonce,
                code_hash: tc.code_hash.into(),
                ..Default::default()
            };

            let result = check_can_decr_account(&account_info, tc.decr_amount, &mut gas_counter);
            if tc.expect_revert {
                assert!(
                    result.is_err(),
                    "Test case {}: expected revert but got success",
                    tc.name
                );
                let err = result.err().unwrap();
                match err {
                    PrecompileErrorOrRevert::Revert(output) => {
                        let revert_bytes = output.bytes;
                        let expected_revert_bytes = revert_message_to_bytes(tc.revert_message);
                        assert_eq!(
                            revert_bytes, expected_revert_bytes,
                            "Test case {}: revert message mismatch",
                            tc.name
                        );
                    }
                    PrecompileErrorOrRevert::Error(_) => {
                        panic!("Test case {}: expected revert but got error", tc.name);
                    }
                }
                assert_eq!(
                    gas_counter.used(),
                    tc.expected_gas_used,
                    "Test case {}: gas used mismatch",
                    tc.name
                );
            } else {
                assert!(
                    result.is_ok(),
                    "Test case {}: expected success but got error",
                    tc.name
                );
                assert_eq!(
                    gas_counter.used(),
                    tc.expected_gas_used,
                    "Test case {}: gas used mismatch",
                    tc.name
                );
            }
        }
    }
}
