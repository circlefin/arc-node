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
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use reth_ethereum::evm::revm::precompile::{PrecompileError, PrecompileHalt, PrecompileOutput};
use reth_evm::precompiles::PrecompileInput;
use revm::context_interface::journaled_state::TransferError;
use revm::state::AccountInfo;
use revm_context_interface::cfg::gas::CALL_STIPEND;
use revm_interpreter::Gas;
use revm_primitives::address;
use revm_primitives::constants::KECCAK_EMPTY;

// system addresses in genesis
pub const NATIVE_FIAT_TOKEN_ADDRESS: Address =
    address!("0x3600000000000000000000000000000000000000");

/// Selector for the Solidity Error(string) format used in revert messages.
pub const REVERT_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];

/// Approximate gas costs for precompile read / writes
pub const PRECOMPILE_SSTORE_GAS_COST: u64 = 2900;
pub const PRECOMPILE_SLOAD_GAS_COST: u64 = 2100;
/// EIP-161 account creation surcharge when crediting an empty account.
pub const PRECOMPILE_EMPTY_ACCOUNT_GAS_COST: u64 = 25_000;

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
    let mut result = Vec::with_capacity(REVERT_SELECTOR.len().saturating_add(encoded.len()));
    result.extend_from_slice(&REVERT_SELECTOR);
    result.extend_from_slice(&encoded);
    Bytes::from(result)
}

/// Gas penalty added to early-path reverts so callers cannot probe precompiles
/// for free.
///
/// Applied to authorization and validation reverts (unauthorized caller,
/// blocklist, zero address, zero amount, overflow) via
/// [`new_reverted_with_early_penalty`].
pub(crate) const PRECOMPILE_EARLY_REVERT_GAS_PENALTY: u64 = 200;

/// Non-success precompile outcomes.
///
/// revm 38 bakes status (Success/Revert/Halt) into `PrecompileOutput`; fatal
/// errors live in `PrecompileError`. Arc's custom precompiles never emit fatal
/// errors, so both variants carry an already-typed `PrecompileOutput`.
pub(crate) enum PrecompileErrorOrRevert {
    /// User-facing revert with ABI-encoded error bytes.
    Revert(PrecompileOutput),
    /// Non-fatal halt (out-of-gas, internal failure) surfaced as `PrecompileHalt`.
    Error(PrecompileOutput),
}

impl PrecompileErrorOrRevert {
    pub(crate) fn new_reverted(gas_counter: Gas, reservoir: u64, msg: &str) -> Self {
        Self::Revert(PrecompileOutput::revert(
            gas_counter.used(),
            revert_message_to_bytes(msg),
            reservoir,
        ))
    }

    pub(crate) fn new_reverted_with_penalty(
        gas_counter: Gas,
        reservoir: u64,
        gas_penalty: u64,
        msg: &str,
    ) -> Self {
        let mut gas_with_penalty = gas_counter;
        if !gas_with_penalty.record_regular_cost(gas_penalty) {
            return Self::halt_oog(reservoir);
        }
        Self::Revert(PrecompileOutput::revert(
            gas_with_penalty.used(),
            revert_message_to_bytes(msg),
            reservoir,
        ))
    }

    pub(crate) fn halt_oog(reservoir: u64) -> Self {
        Self::Error(PrecompileOutput::halt(PrecompileHalt::OutOfGas, reservoir))
    }

    pub(crate) fn halt_other(reservoir: u64, msg: impl Into<String>) -> Self {
        Self::Error(PrecompileOutput::halt(
            PrecompileHalt::other(msg),
            reservoir,
        ))
    }
}

/// Gas cost to load an account balance for stateful precompiles.
///
/// Applies EIP-2929 warm/cold pricing.
fn account_load_cost(is_cold: bool) -> u64 {
    if is_cold {
        revm_interpreter::gas::COLD_ACCOUNT_ACCESS_COST
    } else {
        revm_interpreter::gas::WARM_STORAGE_READ_COST
    }
}

fn storage_io_error(op: &str, reservoir: u64, e: impl core::fmt::Debug) -> PrecompileErrorOrRevert {
    PrecompileErrorOrRevert::halt_other(reservoir, format!("Storage {op} failed: {e:?}"))
}

fn record_empty_account_creation_cost(
    gas_counter: &mut Gas,
    account_info: &AccountInfo,
    amount: U256,
    reservoir: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    if !amount.is_zero() && account_info.is_empty() {
        record_cost_or_out_of_gas(gas_counter, reservoir, PRECOMPILE_EMPTY_ACCOUNT_GAS_COST)?;
    }
    Ok(())
}

pub(crate) fn record_cost_or_out_of_gas(
    gas_counter: &mut Gas,
    reservoir: u64,
    cost: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    if !gas_counter.record_regular_cost(cost) {
        return Err(PrecompileErrorOrRevert::halt_oog(reservoir));
    }
    Ok(())
}

pub(crate) fn check_gas_remaining(
    gas_counter: &Gas,
    reservoir: u64,
    cost: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    if gas_counter.remaining() < cost {
        return Err(PrecompileErrorOrRevert::halt_oog(reservoir));
    }
    Ok(())
}

impl From<PrecompileErrorOrRevert> for Result<PrecompileOutput, PrecompileError> {
    fn from(val: PrecompileErrorOrRevert) -> Self {
        match val {
            PrecompileErrorOrRevert::Revert(output) | PrecompileErrorOrRevert::Error(output) => {
                Ok(output)
            }
        }
    }
}

/// Use at early-path reverts (unauthorized caller, blocklist, zero address,
/// zero amount, overflow) to give uniform gas accounting and prevent free
/// probing of precompile revert paths.
pub(crate) fn new_reverted_with_early_penalty(
    gas_counter: Gas,
    reservoir: u64,
    msg: &str,
) -> PrecompileErrorOrRevert {
    PrecompileErrorOrRevert::new_reverted_with_penalty(
        gas_counter,
        reservoir,
        PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
        msg,
    )
}

/// ABI-decodes raw precompile call arguments.
///
/// Uses validated decoding, which rejects non-canonical ABI padding for short
/// static types such as `address`, `bool`, and `uint64`.
pub(crate) fn abi_decode_raw_validated<C: SolCall>(input: &[u8]) -> alloy_sol_types::Result<C> {
    C::abi_decode_raw_validate(input)
}

/// Reads a value from storage for stateful precompiles.
///
/// # Parameters
/// - `internals`: The execution context with journal access
/// - `address`: The address whose storage to read from
/// - `storage_key`: The storage slot to read
/// - `gas_counter`: Available gas for this operation
/// - `reservoir`: EIP-8037 state-gas reservoir from the precompile call
///
/// # Gas Cost
/// EIP-2929 warm/cold aware (100 warm, 2100 cold).
///
/// # Returns
/// - `Ok(Bytes)`: The stored value as big-endian bytes
/// - `Err(PrecompileErrorOrRevert)`: If out of gas or storage read fails
pub(crate) fn read(
    internals: &mut EvmInternals,
    address: Address,
    storage_key: StorageKey,
    gas_counter: &mut Gas,
    reservoir: u64,
) -> Result<Bytes, PrecompileErrorOrRevert> {
    let mut account = internals
        .load_account_mut(address)
        .map_err(|e| storage_io_error("read", reservoir, e))?
        .data;

    // Probe slot warmth without DB I/O (skip_cold_load=true).
    // Warm → Ok with cached value. Cold → ColdLoadSkipped error, retry after charging.
    match account.sload(storage_key.into(), true) {
        Ok(slot_load) => {
            record_cost_or_out_of_gas(
                gas_counter,
                reservoir,
                revm_interpreter::gas::WARM_STORAGE_READ_COST,
            )?;
            Ok(slot_load.data.present_value().to_be_bytes_vec().into())
        }
        Err(e) if e.is_cold_load_skipped() => {
            record_cost_or_out_of_gas(
                gas_counter,
                reservoir,
                revm_interpreter::gas::COLD_SLOAD_COST,
            )?;
            let slot_load = account
                .sload(storage_key.into(), false)
                .map_err(|e| storage_io_error("read", reservoir, e))?;
            Ok(slot_load.data.present_value().to_be_bytes_vec().into())
        }
        Err(e) => Err(storage_io_error("read", reservoir, e)),
    }
}

/// Value-change component of SSTORE gas, excluding the cold-load penalty.
///
/// Mirrors revm v29 `istanbul_sstore_cost<WARM_STORAGE_READ_COST, WARM_SSTORE_RESET>`.
fn sstore_base_cost(original: U256, present: U256, new: U256) -> u64 {
    if new == present {
        revm_interpreter::gas::WARM_STORAGE_READ_COST
    } else if original == present {
        if original.is_zero() {
            revm_interpreter::gas::SSTORE_SET
        } else {
            revm_interpreter::gas::WARM_SSTORE_RESET
        }
    } else {
        revm_interpreter::gas::WARM_STORAGE_READ_COST
    }
}

/// Writes a value to storage for stateful precompiles.
///
/// # Parameters
/// - `internals`: The execution context with journal access
/// - `address`: The address whose storage to write to
/// - `storage_key`: The storage slot to write
/// - `input`: The value to store (as big-endian bytes)
/// - `gas_counter`: Available gas for this operation
/// - `reservoir`: EIP-8037 state-gas reservoir from the precompile call
///
/// # Gas Cost
/// EIP-2929/EIP-2200 aware (varies based on warm/cold and value changes).
///
/// # EIP-2200 Sentry
/// Mirrors revm's SSTORE opcode behavior: if the remaining gas is less than or
/// equal to [`CALL_STIPEND`] (2,300), the call frame fails with `OutOfGas`
/// before any storage mutation is journaled.
pub(crate) fn write(
    internals: &mut EvmInternals,
    address: Address,
    storage_key: StorageKey,
    input: &[u8],
    gas_counter: &mut Gas,
    reservoir: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    // EIP-2200 reentrancy sentry: refuse SSTORE when remaining gas does not
    // exceed the call stipend.
    if gas_counter.remaining() <= CALL_STIPEND {
        return Err(PrecompileErrorOrRevert::halt_oog(reservoir));
    }

    // Parse the input as a U256 value
    let value = U256::from_be_slice(input);

    let mut account = internals
        .load_account_mut(address)
        .map_err(|e| storage_io_error("write", reservoir, e))?
        .data;

    // Probe slot warmth via sload to get current values for gas calculation.
    // This lets us charge all gas before the actual sstore mutation.
    let slot = match account.sload(storage_key.into(), true) {
        Ok(slot_load) => slot_load.data,
        Err(e) if e.is_cold_load_skipped() => {
            record_cost_or_out_of_gas(
                gas_counter,
                reservoir,
                revm_interpreter::gas::COLD_SLOAD_COST,
            )?;
            account
                .sload(storage_key.into(), false)
                .map_err(|e| storage_io_error("write", reservoir, e))?
                .data
        }
        Err(e) => return Err(storage_io_error("write", reservoir, e)),
    };

    record_cost_or_out_of_gas(
        gas_counter,
        reservoir,
        sstore_base_cost(slot.original_value, slot.present_value, value),
    )?;

    // All gas charged — safe to mutate. Slot is warm from the sload.
    account
        .sstore(storage_key.into(), value, false)
        .map_err(|e| storage_io_error("write", reservoir, e))?;
    Ok(())
}

/// Helper to transfer funds between two accounts using the Journal
// TODO(NoStory): switch to skip-cold-then-retry (probe via
// `load_account_mut_skip_cold_load`, charge cold gas, then retry), mirroring
// `read`/`write`. Currently performs DB I/O before charging cold-load gas.
// Couple this change with removing the `#[ignore]`d
// `load_account_mut_skip_cold_load_panics_on_cold_account` sentinel test in
// the `tests` module below — it exists to fire exactly when this refactor
// becomes possible (revm 37+).
pub(crate) fn transfer(
    internals: &mut EvmInternals,
    from: Address,
    to: Address,
    amount: U256,
    gas_counter: &mut Gas,
    reservoir: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    let loaded_from_account = internals
        .load_account(from)
        .map_err(|_| PrecompileErrorOrRevert::halt_other(reservoir, ERR_EXECUTION_REVERTED))?;
    record_cost_or_out_of_gas(
        gas_counter,
        reservoir,
        account_load_cost(loaded_from_account.is_cold),
    )?;

    // Check that the account can be decremented by the amount
    check_can_decr_account(&loaded_from_account.info, amount, gas_counter, reservoir)?;

    // Mirrors prior balance_decr + balance_incr.
    record_cost_or_out_of_gas(gas_counter, reservoir, PRECOMPILE_SSTORE_GAS_COST)?;

    let to_load = internals
        .load_account(to)
        .map_err(|_| PrecompileErrorOrRevert::halt_other(reservoir, ERR_EXECUTION_REVERTED))?;
    record_cost_or_out_of_gas(gas_counter, reservoir, account_load_cost(to_load.is_cold))?;

    record_cost_or_out_of_gas(gas_counter, reservoir, PRECOMPILE_SSTORE_GAS_COST)?;

    if to_load.is_selfdestructed() {
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            reservoir,
            ERR_SELFDESTRUCTED_BALANCE_INCREASED,
        ));
    }

    record_empty_account_creation_cost(gas_counter, &to_load.info, amount, reservoir)?;

    let transfer_result = internals.transfer(from, to, amount).map_err(|_e| {
        PrecompileErrorOrRevert::new_reverted(*gas_counter, reservoir, ERR_EXECUTION_REVERTED)
    })?;

    match transfer_result {
        None => Ok(()),
        Some(error) => match error {
            // Pre-empted by `check_can_decr_account` above.
            TransferError::OutOfFunds => Err(PrecompileErrorOrRevert::new_reverted(
                *gas_counter,
                reservoir,
                ERR_INSUFFICIENT_FUNDS,
            )),
            TransferError::OverflowPayment => Err(PrecompileErrorOrRevert::new_reverted(
                *gas_counter,
                reservoir,
                ERR_OVERFLOW,
            )),
            TransferError::CreateCollision => Err(PrecompileErrorOrRevert::new_reverted(
                *gas_counter,
                reservoir,
                ERR_EXECUTION_REVERTED,
            )),
        },
    }
}

/// Helper to increment an account's balance by an amount using the Journal
// TODO(NoStory): see the matching note above `fn transfer`. Same refactor
// applies here. Couple with removing the `#[ignore]`d sentinel test in
// `tests`.
pub(crate) fn balance_incr(
    internals: &mut EvmInternals,
    to: Address,
    amount: U256,
    gas_counter: &mut Gas,
    reservoir: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    // Balance check, but doesn't touch state
    let account = internals
        .load_account(to)
        .map_err(|_| PrecompileErrorOrRevert::halt_other(reservoir, ERR_EXECUTION_REVERTED))?;
    record_cost_or_out_of_gas(gas_counter, reservoir, account_load_cost(account.is_cold))?;

    if account.is_selfdestructed() {
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            reservoir,
            ERR_SELFDESTRUCTED_BALANCE_INCREASED,
        ));
    }

    let account_balance = account.info.balance;
    account_balance
        .checked_add(amount)
        .ok_or(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            reservoir,
            ERR_OVERFLOW,
        ))?;

    // Update state
    record_cost_or_out_of_gas(gas_counter, reservoir, PRECOMPILE_SSTORE_GAS_COST)?;
    record_empty_account_creation_cost(gas_counter, &account.info, amount, reservoir)?;
    internals
        .balance_incr(to, amount)
        .map_err(|_| PrecompileErrorOrRevert::halt_other(reservoir, ERR_EXECUTION_REVERTED))?;

    Ok(())
}

/// Helper to decrement an account's balance by an amount using the Journal
// TODO(NoStory): see the matching note above `fn transfer`. Same refactor
// applies here. Couple with removing the `#[ignore]`d sentinel test in
// `tests`.
pub(crate) fn balance_decr(
    internals: &mut EvmInternals,
    from: Address,
    amount: U256,
    gas_counter: &mut Gas,
    reservoir: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    let loaded_from_account = internals
        .load_account(from)
        .map_err(|_| PrecompileErrorOrRevert::halt_other(reservoir, ERR_EXECUTION_REVERTED))?;
    record_cost_or_out_of_gas(
        gas_counter,
        reservoir,
        account_load_cost(loaded_from_account.is_cold),
    )?;

    // Check that the account can be decremented by the amount
    check_can_decr_account(&loaded_from_account.info, amount, gas_counter, reservoir)?;

    // Perform the decrement
    record_cost_or_out_of_gas(gas_counter, reservoir, PRECOMPILE_SSTORE_GAS_COST)?;
    let mut account = internals
        .load_account_mut(from)
        .map_err(|_| PrecompileErrorOrRevert::halt_other(reservoir, ERR_EXECUTION_REVERTED))?;

    // False only returned on insufficient funds; prior check_can_decr_account makes this unreachable.
    if !account.decr_balance(amount) {
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            reservoir,
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
            precompile_input.reservoir,
            ERR_STATE_CHANGE_DURING_STATIC_CALL,
        ));
    }
    Ok(())
}

/// Helper to check delegatecall
pub(crate) fn check_delegatecall(
    precompile_address: Address,
    precompile_input: &PrecompileInput,
    gas_counter: &Gas,
) -> Result<(), PrecompileErrorOrRevert> {
    if precompile_input.target_address != precompile_address
        || precompile_input.bytecode_address != precompile_address
    {
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            precompile_input.reservoir,
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
    reservoir: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    // Check that the account has sufficient balance
    let from_account_balance = loaded_account_info.balance.checked_sub(amount).ok_or(
        PrecompileErrorOrRevert::new_reverted(*gas_counter, reservoir, ERR_INSUFFICIENT_FUNDS),
    )?;

    // Check that the account would not be emptied if this transfer goes through
    let from_account_is_empty = from_account_balance.is_zero()
        && loaded_account_info.nonce == 0
        && (loaded_account_info.code_hash() == KECCAK_EMPTY
            || loaded_account_info.code_hash().is_zero());

    if from_account_is_empty {
        record_cost_or_out_of_gas(gas_counter, reservoir, PRECOMPILE_SSTORE_GAS_COST)?;
        return Err(PrecompileErrorOrRevert::new_reverted(
            *gas_counter,
            reservoir,
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
    reservoir: u64,
) -> Result<(), PrecompileErrorOrRevert> {
    let data = event.encode_log_data();

    let topic_gas = LOG_TOPIC_COST.saturating_mul(data.topics().len() as u64);
    let data_gas = LOG_DATA_COST.saturating_mul(data.data.len() as u64);
    let log_gas = LOG_BASE_COST
        .saturating_add(topic_gas)
        .saturating_add(data_gas);
    record_cost_or_out_of_gas(gas_counter, reservoir, log_gas)?;

    let log = revm::primitives::Log { address, data };

    internals.log(log);
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_utils {
    use alloy_primitives::{Address, B256, U256};
    use revm::database_interface::{DBErrorMarker, Database, DatabaseRef};
    use revm::state::{AccountInfo, Bytecode};
    use std::cell::Cell;

    /// Database wrapper that counts `storage()` calls via a shared `Cell`
    /// counter while returning empty state. Use to prove that an OOG path
    /// does not hit the database.
    #[derive(Debug, Clone)]
    pub(crate) struct TrackingDB {
        storage_reads: std::rc::Rc<Cell<u64>>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct TrackingDBError;

    impl core::fmt::Display for TrackingDBError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "TrackingDBError")
        }
    }

    impl core::error::Error for TrackingDBError {}
    impl DBErrorMarker for TrackingDBError {}

    type TrackingContext = revm::Context<
        revm::context::BlockEnv,
        revm::context::TxEnv,
        revm::context::CfgEnv,
        TrackingDB,
        revm::context::Journal<TrackingDB>,
    >;

    impl TrackingDB {
        pub(crate) fn new() -> (Self, std::rc::Rc<Cell<u64>>) {
            let counter = std::rc::Rc::new(Cell::new(0));
            (
                Self {
                    storage_reads: counter.clone(),
                },
                counter,
            )
        }

        pub(crate) fn context() -> (TrackingContext, std::rc::Rc<Cell<u64>>) {
            let (db, counter) = Self::new();
            (
                revm::context::Context::new(db, revm_primitives::hardfork::SpecId::default()),
                counter,
            )
        }
    }

    impl Database for TrackingDB {
        type Error = TrackingDBError;

        fn basic(&mut self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(None)
        }

        fn code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn storage(&mut self, _address: Address, _index: U256) -> Result<U256, Self::Error> {
            self.storage_reads
                .set(self.storage_reads.get().saturating_add(1));
            Ok(U256::ZERO)
        }

        fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
            Ok(alloy_primitives::keccak256(number.to_string().as_bytes()))
        }
    }

    impl DatabaseRef for TrackingDB {
        type Error = TrackingDBError;

        fn basic_ref(&self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(None)
        }

        fn code_by_hash_ref(&self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn storage_ref(&self, _address: Address, _index: U256) -> Result<U256, Self::Error> {
            self.storage_reads
                .set(self.storage_reads.get().saturating_add(1));
            Ok(U256::ZERO)
        }

        fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
            Ok(alloy_primitives::keccak256(number.to_string().as_bytes()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use alloy_sol_types::sol;
    use revm_primitives::B256;

    /// Demonstrates that revm's `JournalTr::load_account_mut_skip_cold_load`
    /// panics on cold accounts, making it unusable for the probe-then-charge
    /// pattern we use for storage slots via `JournaledAccountTr::sload/sstore`.
    ///
    /// This was fixed in revm 37+ (bluealloy/revm#3477). On revm 38 the call
    /// no longer panics, so the `#[should_panic]` assertion fails. Test is
    /// `#[ignore]`d as a permanent breadcrumb until the follow-up refactor
    /// switches `transfer`/`balance_incr`/`balance_decr` to the
    /// skip-cold-then-retry pattern used by `read`/`write`. Delete this test
    /// as part of that refactor.
    ///
    /// TODO(NoStory): follow-up PR — switch the three account helpers to
    /// skip-cold-then-retry; remove this test.
    #[test]
    #[ignore = "revm 38 fixed the underlying panic; superseded by the helper refactor TODO"]
    #[should_panic(expected = "Expected DBError")]
    fn load_account_mut_skip_cold_load_panics_on_cold_account() {
        use revm::context_interface::journaled_state::JournalTr;
        use revm::{Journal, JournalEntry};

        let db = revm::database_interface::EmptyDB::default();
        let mut journal = Journal::<_, JournalEntry>::new_with_inner(db, Default::default());

        let cold_address = address!("dead000000000000000000000000000000000001");
        // Panics because the JournalTr impl maps ColdLoadSkipped through
        // unwrap_db_error(), which expects a DBError variant.
        let _ = journal.load_account_mut_skip_cold_load(cold_address, true);
    }

    /// Asserts all branches of [`sstore_base_cost`] with explicit values to
    /// catch silent upstream changes.
    #[test]
    fn sstore_base_cost_covers_all_branches() {
        // new == present → WARM_STORAGE_READ_COST (100)
        assert_eq!(
            sstore_base_cost(U256::from(1), U256::from(2), U256::from(2)),
            100,
        );

        // original == present, original == 0 → SSTORE_SET (20000)
        assert_eq!(
            sstore_base_cost(U256::ZERO, U256::ZERO, U256::from(1)),
            20000,
        );

        // original == present, original != 0 → WARM_SSTORE_RESET (2900)
        assert_eq!(
            sstore_base_cost(U256::from(1), U256::from(1), U256::from(2)),
            2900,
        );

        // original != present, new != present → WARM_STORAGE_READ_COST (100)
        assert_eq!(
            sstore_base_cost(U256::from(1), U256::from(2), U256::from(3)),
            100,
        );
    }

    sol! {
        interface IAbiDecodeTest {
            function takesAddress(address account) external;
            function takesUint64(uint64 value) external;
        }
    }

    #[test]
    fn abi_decode_raw_validation_rejects_address_padding() {
        let mut input = [0u8; 32];
        input[..12].fill(0x11);
        input[12] = 0xaa;

        let result = abi_decode_raw_validated::<IAbiDecodeTest::takesAddressCall>(&input);
        assert!(result.is_err(), "non-zero address padding must be rejected");
    }

    #[test]
    fn abi_decode_raw_validation_rejects_uint64_padding() {
        let mut input = [0u8; 32];
        input[0] = 0x11;
        input[31] = 42;

        let result = abi_decode_raw_validated::<IAbiDecodeTest::takesUint64Call>(&input);
        assert!(result.is_err(), "non-zero uint64 padding must be rejected");
    }

    #[test]
    fn empty_account_creation_cost_charges_only_for_nonzero_empty_accounts() {
        let mut gas_counter = Gas::new(100_000);
        assert!(record_empty_account_creation_cost(
            &mut gas_counter,
            &AccountInfo::default(),
            U256::ZERO,
            0,
        )
        .is_ok());
        assert_eq!(gas_counter.used(), 0);

        assert!(record_empty_account_creation_cost(
            &mut gas_counter,
            &AccountInfo::default(),
            U256::from(1),
            0,
        )
        .is_ok());
        assert_eq!(gas_counter.used(), PRECOMPILE_EMPTY_ACCOUNT_GAS_COST);

        for non_empty_account in [
            AccountInfo {
                balance: U256::from(1),
                ..Default::default()
            },
            AccountInfo {
                nonce: 1,
                ..Default::default()
            },
            AccountInfo {
                code_hash: B256::from([1u8; 32]),
                ..Default::default()
            },
        ] {
            assert!(record_empty_account_creation_cost(
                &mut gas_counter,
                &non_empty_account,
                U256::from(1),
                0,
            )
            .is_ok());
            assert_eq!(gas_counter.used(), PRECOMPILE_EMPTY_ACCOUNT_GAS_COST);
        }
    }

    #[test]
    fn empty_account_creation_cost_errors_when_out_of_gas() {
        let mut gas_counter = Gas::new(PRECOMPILE_EMPTY_ACCOUNT_GAS_COST.saturating_sub(1));
        assert!(matches!(
            record_empty_account_creation_cost(
                &mut gas_counter,
                &AccountInfo::default(),
                U256::from(1),
                0,
            ),
            Err(PrecompileErrorOrRevert::Error(_))
        ));
    }

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

            let result = check_can_decr_account(&account_info, tc.decr_amount, &mut gas_counter, 0);
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

    #[test]
    fn from_precompile_error_or_revert_revert_keeps_output() {
        let revert_bytes = revert_message_to_bytes("test revert");
        let err_or_revert = PrecompileErrorOrRevert::Revert(PrecompileOutput::revert(
            1_000,
            revert_bytes.clone(),
            0,
        ));

        let result: Result<PrecompileOutput, PrecompileError> = err_or_revert.into();

        let output = result.expect("Revert variant must convert to Ok(PrecompileOutput)");
        assert!(
            output.is_revert(),
            "Revert variant must preserve Revert status"
        );
        assert_eq!(output.gas_used, 1_000);
        assert_eq!(output.bytes, revert_bytes);
    }

    #[test]
    fn from_precompile_error_or_revert_error_keeps_halt() {
        let err_or_revert = PrecompileErrorOrRevert::halt_oog(0);

        let result: Result<PrecompileOutput, PrecompileError> = err_or_revert.into();

        let output =
            result.expect("Error variant must convert to Ok(PrecompileOutput) under revm 38");
        assert!(output.is_halt(), "Error variant must preserve Halt status");
        assert_eq!(
            output.halt_reason(),
            Some(&PrecompileHalt::OutOfGas),
            "halt reason must round-trip"
        );
    }
}
