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

//! Native Coin Authority Precompile
//!
//! This precompile implements native coin operations including mint, burn,
//! transfer, and total supply management.

use crate::helpers::{
    balance_decr, balance_incr, check_delegatecall, check_gas_remaining, check_staticcall,
    emit_event, read, transfer, write, PrecompileErrorOrRevert, ERR_BLOCKED_ADDRESS,
    ERR_EXECUTION_REVERTED, LOG_BASE_COST, LOG_DATA_COST, LOG_TOPIC_COST,
    NATIVE_FIAT_TOKEN_ADDRESS, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, PRECOMPILE_SLOAD_GAS_COST,
    PRECOMPILE_SSTORE_GAS_COST,
};
use crate::native_coin_control::{compute_is_blocklisted_storage_slot, UNBLOCKLISTED_STATUS};
use crate::stateful;
use crate::NATIVE_COIN_CONTROL_ADDRESS;
use alloy_evm::EvmInternals;
use alloy_primitives::{address, Address, StorageKey, U256};
use alloy_sol_types::{sol, SolCall, SolValue};
use arc_execution_config::hardforks::{ArcHardfork, ArcHardforkFlags};
use reth_ethereum::evm::revm::precompile::PrecompileOutput;
use revm_interpreter::Gas;

// Native coin authority precompile address
pub const NATIVE_COIN_AUTHORITY_ADDRESS: Address =
    address!("0x1800000000000000000000000000000000000000");

use revm::handler::SYSTEM_ADDRESS;

// Allowed caller from NativeFiatToken
const ALLOWED_CALLER_ADDRESS: Address = NATIVE_FIAT_TOKEN_ADDRESS;

// Storage key for allowed caller (deprecated since Zero5)
const ALLOWED_CALLER_STORAGE_KEY: StorageKey = StorageKey::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
]);

// Storage key for total supply
const TOTAL_SUPPLY_STORAGE_KEY: StorageKey = StorageKey::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
]);

const MINT_EVENT_GAS_COST: u64 = LOG_BASE_COST + 2 * LOG_TOPIC_COST + 32 * LOG_DATA_COST; // 2 topics + 32 bytes of data
const BURN_EVENT_GAS_COST: u64 = LOG_BASE_COST + 2 * LOG_TOPIC_COST + 32 * LOG_DATA_COST; // 2 topics + 32 bytes of data
const TRANSFER_EVENT_GAS_COST: u64 = LOG_BASE_COST + 3 * LOG_TOPIC_COST + 32 * LOG_DATA_COST; // 3 topics + 32 bytes of data

// Total gas costs for each operation

// Mint (pre-Zero5): NativeCoinMinted event (2 topics)
// - Reading allowed caller (2100 gas)
// - Reading blocked to (2100 gas)
// - Reading total supply (2100 gas)
// - Reading account balance (2100 gas)
// - Writing total supply (2900 gas)
// - Writing account balance (2900 gas)
// - Emitting NativeCoinMinted event (1381 gas)
// Total: 15,581 gas
const MINT_GAS_COST: u64 =
    4 * PRECOMPILE_SLOAD_GAS_COST + 2 * PRECOMPILE_SSTORE_GAS_COST + MINT_EVENT_GAS_COST;

// Mint (Zero5+): upper-bound gas limit for tests.
// Zero5 uses warm/cold SLOAD pricing (100/2100) and removes the auth SLOAD, so
// the actual cost varies per call. This assumes all-cold as a safe upper bound.
// Only used as a test gas limit, not for production gas checks.
#[cfg(test)]
const MINT_GAS_COST_EIP7708: u64 =
    4 * PRECOMPILE_SLOAD_GAS_COST + 2 * PRECOMPILE_SSTORE_GAS_COST + TRANSFER_EVENT_GAS_COST;

// Burn (pre-Zero5): NativeCoinBurned event (2 topics)
// - Reading allowed caller (2100 gas)
// - Reading blocked from (2100 gas)
// - Reading account balance (2100 gas)
// - Reading total supply (2100 gas)
// - Writing account balance (2900 gas)
// - Writing total supply (2900 gas)
// - Emitting NativeCoinBurned event (1381 gas)
// Total: 15,581 gas
const BURN_GAS_COST: u64 =
    4 * PRECOMPILE_SLOAD_GAS_COST + 2 * PRECOMPILE_SSTORE_GAS_COST + BURN_EVENT_GAS_COST;

// Burn (Zero5+): upper-bound gas limit for tests.
// Same rationale as MINT_GAS_COST_EIP7708 above.
#[cfg(test)]
const BURN_GAS_COST_EIP7708: u64 =
    4 * PRECOMPILE_SLOAD_GAS_COST + 2 * PRECOMPILE_SSTORE_GAS_COST + TRANSFER_EVENT_GAS_COST;

// - Reading allowed caller (2100 gas) (removed since Zero5)
// - Reading blocked from (2100 gas)
// - Reading blocked to (2100 gas)
// - Reading account balance x2 (4200 gas)
// - Writing account balance x2 (5800 gas)
// - Emitting event (1756 gas)
// Total: 18056 gas
const TRANSFER_GAS_COST: u64 =
    5 * PRECOMPILE_SLOAD_GAS_COST + 2 * PRECOMPILE_SSTORE_GAS_COST + TRANSFER_EVENT_GAS_COST;
// The gas cost for a transfer operation when the amount is zero
// - Reading allowed caller (2100 gas) (removed since Zero5)
// - Reading blocked from (2100 gas)
// - Reading blocked to (2100 gas)
const TRANSFER_GAS_COST_WITH_ZERO_AMOUNT: u64 = 3 * PRECOMPILE_SLOAD_GAS_COST;

const TOTAL_SUPPLY_GAS_COST: u64 = PRECOMPILE_SLOAD_GAS_COST;

// Error messages
const ERR_CANNOT_MINT: &str = "Not enabled native coin minter";
const ERR_CANNOT_BURN: &str = "Not enabled native coin burner";
const ERR_CANNOT_TRANSFER: &str = "Not enabled for native coin transfers";
const ERR_OVERFLOW: &str = "Arithmetic overflow";
const ERR_ZERO_AMOUNT: &str = "Zero amount invalid";
use crate::helpers::ERR_ZERO_ADDRESS;

sol! {
    /// Native Coin Authority precompile interface
    interface INativeCoinAuthority {
        /// Mint new coins to the specified address
        function mint(address to, uint256 amount) external returns (bool);

        /// Burn coins from the specified address
        function burn(address from, uint256 amount) external returns (bool);

        /// Transfer coins between addresses
        function transfer(address from, address to, uint256 amount) external returns (bool);

        /// Get the total supply of native coins
        function totalSupply() external view returns (uint256 supply);
    }

    /// Events
    #[derive(Debug)]
    event NativeCoinMinted(address indexed recipient, uint256 amount);

    #[derive(Debug)]
    event NativeCoinBurned(address indexed from, uint256 amount);

    #[derive(Debug)]
    event NativeCoinTransferred(address indexed from, address indexed to, uint256 amount);

    /// ERC-20 Transfer event (EIP-7708), used under Zero5 for native coin transfers
    #[derive(Debug)]
    event Transfer(address indexed from, address indexed to, uint256 value);
}

/// Checks if the caller is authorized to call mutative native coin authority functions
fn is_authorized(
    internals: &mut EvmInternals,
    caller: Address,
    gas_counter: &mut Gas,
    hardfork_flags: ArcHardforkFlags,
) -> Result<bool, PrecompileErrorOrRevert> {
    // Get allowed caller
    let allowed_caller_output = read(
        internals,
        NATIVE_COIN_AUTHORITY_ADDRESS,
        ALLOWED_CALLER_STORAGE_KEY,
        gas_counter,
        hardfork_flags,
    )?;

    // Compare caller to allowed_caller_output
    let caller_word = U256::from_be_slice(caller.as_ref());
    let allowed_caller_word = U256::from_be_slice(&allowed_caller_output);
    Ok(caller_word == allowed_caller_word)
}

fn is_blocklisted(
    internals: &mut EvmInternals,
    address: Address,
    gas_counter: &mut Gas,
    hardfork_flags: ArcHardforkFlags,
) -> Result<bool, PrecompileErrorOrRevert> {
    // Get address storage slot for blocklist
    let storage_slot = compute_is_blocklisted_storage_slot(address);
    let storage_output = read(
        internals,
        NATIVE_COIN_CONTROL_ADDRESS,
        storage_slot,
        gas_counter,
        hardfork_flags,
    )?;

    Ok(!U256::from_be_slice(&storage_output).eq(&UNBLOCKLISTED_STATUS))
}

stateful!(run_native_coin_authority, precompile_input, hardfork_flags; {
    INativeCoinAuthority::mintCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let mut precompile_input = precompile_input;
            // Check if static call is attempting to modify state
            check_staticcall(
                &precompile_input,
                &mut gas_counter,
            )?;

            // Decode arguments passed to mint function
            let args = INativeCoinAuthority::mintCall::abi_decode_raw(
                input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED)
                )?;

            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                // Zero5+: Skip early gas check - warm/cold pricing makes upfront calculation unreliable
                // Check authorization
                if precompile_input.caller != ALLOWED_CALLER_ADDRESS {
                    return Err(PrecompileErrorOrRevert::new_reverted(
                        gas_counter,
                        ERR_CANNOT_MINT,
                    ));
                }
            } else {
                // Early return if not enough gas
                check_gas_remaining(&gas_counter, MINT_GAS_COST)?;

                // Check authorization
                if !is_authorized(
                    &mut precompile_input.internals,
                    precompile_input.caller,
                    &mut gas_counter,
                    hardfork_flags,
                )? {
                    return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_CANNOT_MINT));
                }
            }

            // Prevent delegate calls
            check_delegatecall(
                NATIVE_COIN_AUTHORITY_ADDRESS,
                &precompile_input,
                &mut gas_counter,
            )?;

            // Check blocklist
            if is_blocklisted(&mut precompile_input.internals, args.to, &mut gas_counter, hardfork_flags)? {
                return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_BLOCKED_ADDRESS));
            }

            // Validate amount
            if args.amount == U256::ZERO {
                return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_ZERO_AMOUNT));
            }

            // Read current total supply
            let total_supply_output = read(
                &mut precompile_input.internals,
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY,
                &mut gas_counter,
                hardfork_flags,
            )?;
            let current_total_supply = U256::from_be_slice(&total_supply_output);

            // Check for overflow
            let new_total_supply = match current_total_supply.checked_add(args.amount) {
                Some(new_total_supply) => new_total_supply,
                None => return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_OVERFLOW)),
            };

            // Write new total supply
            write(
                &mut precompile_input.internals,
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY,
                &new_total_supply.to_be_bytes_vec(),
                &mut gas_counter,
                hardfork_flags,
            )?;

            // Update account balance
            balance_incr(&mut precompile_input.internals, args.to, args.amount, &mut gas_counter,
               hardfork_flags.is_active(ArcHardfork::Zero5))?;

            // Emit event: ERC-20 Transfer(0x0, to) under Zero5, NativeCoinMinted otherwise.
            // Address::ZERO as `from` follows the ERC-20 convention for minting. This is
            // intentionally allowed here even though CALL/CREATE value transfers reject
            // Address::ZERO (see check_blocklist_and_create_log in evm.rs).
            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                emit_event(&mut precompile_input.internals, SYSTEM_ADDRESS, &Transfer {
                    from: Address::ZERO,
                    to: args.to,
                    value: args.amount,
                }, &mut gas_counter)?;
            } else {
                emit_event(&mut precompile_input.internals, NATIVE_COIN_AUTHORITY_ADDRESS, &NativeCoinMinted {
                    recipient: args.to,
                    amount: args.amount,
                }, &mut gas_counter)?;
            }

            let output = true.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into()))
        })()
    },

    INativeCoinAuthority::burnCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let mut precompile_input = precompile_input;
            // Check if static call is attempting to modify state
            check_staticcall(
                &precompile_input,
                &mut gas_counter,
            )?;

            // Decode arguments passed to burn function
            let args = INativeCoinAuthority::burnCall::abi_decode_raw(input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED,
                    )
                )?;

            // Check authorization
            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                // Zero5+: Skip early gas check - warm/cold pricing makes upfront calculation unreliable
                if precompile_input.caller != ALLOWED_CALLER_ADDRESS {
                    return Err(PrecompileErrorOrRevert::new_reverted(
                        gas_counter,
                        ERR_CANNOT_BURN,
                    ));
                }
            } else {
                // Early return if not enough gas
                check_gas_remaining(&gas_counter, BURN_GAS_COST)?;

                if !(is_authorized(&mut precompile_input.internals, precompile_input.caller, &mut gas_counter, hardfork_flags)?) {
                    return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_CANNOT_BURN));
                }
            }

            // Prevent delegate calls
            check_delegatecall(
                NATIVE_COIN_AUTHORITY_ADDRESS,
                &precompile_input,
                &mut gas_counter,
            )?;

            // Check blocklist
            if is_blocklisted(&mut precompile_input.internals, args.from, &mut gas_counter, hardfork_flags)? {
                return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_BLOCKED_ADDRESS));
            }

            // Validate amount
            if args.amount == U256::ZERO {
                return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_ZERO_AMOUNT));
            }

            // Check balance and burn tokens
            balance_decr(&mut precompile_input.internals, args.from, args.amount, &mut gas_counter)?;

            // Adjust total supply
            let total_supply_output = read(
                &mut precompile_input.internals,
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY,
                &mut gas_counter,
                hardfork_flags,
            )?;
            let current_total_supply = U256::from_be_slice(&total_supply_output);

            // Write new total supply
            // Underflow cannot happen due to the balance check
            write(
                &mut precompile_input.internals,
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY,
                &current_total_supply.saturating_sub(args.amount).to_be_bytes_vec(),
                &mut gas_counter,
                hardfork_flags,
            )?;

            // Emit event: ERC-20 Transfer(from, 0x0) under Zero5, NativeCoinBurned otherwise.
            // Address::ZERO as `to` follows the ERC-20 convention for burning. This is
            // intentionally allowed here even though CALL/CREATE value transfers reject
            // Address::ZERO (see check_blocklist_and_create_log in evm.rs).
            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                emit_event(&mut precompile_input.internals, SYSTEM_ADDRESS, &Transfer {
                    from: args.from,
                    to: Address::ZERO,
                    value: args.amount,
                }, &mut gas_counter)?;
            } else {
                emit_event(&mut precompile_input.internals, NATIVE_COIN_AUTHORITY_ADDRESS, &NativeCoinBurned {
                    from: args.from,
                    amount: args.amount,
                }, &mut gas_counter)?;
            }

            // Return response
            let output = true.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into()))
        })()
    },

    INativeCoinAuthority::transferCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let mut precompile_input = precompile_input;
            // Check if static call is attempting to modify state
            check_staticcall(
                &precompile_input,
                &mut gas_counter,
            )?;

            // Decode arguments passed to transfer function
            let args = INativeCoinAuthority::transferCall::abi_decode_raw(input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED,
                    )
                )?;

            // Check authorization
            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                // Zero5+: Skip early gas check - warm/cold pricing makes upfront calculation unreliable
                if precompile_input.caller != ALLOWED_CALLER_ADDRESS {
                    return Err(PrecompileErrorOrRevert::new_reverted(
                        gas_counter,
                        ERR_CANNOT_TRANSFER,
                    ));
                }
            } else {
                // The gas cost is different if the amount is zero or not
                let expect_gas_cost = if args.amount != U256::ZERO {
                    TRANSFER_GAS_COST
                } else {
                    TRANSFER_GAS_COST_WITH_ZERO_AMOUNT
                };

                // Early return if not enough gas
                check_gas_remaining(&gas_counter, expect_gas_cost)?;

                if !(is_authorized(&mut precompile_input.internals, precompile_input.caller, &mut gas_counter, hardfork_flags)?) {
                    return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_CANNOT_TRANSFER));
                }
            }

            // Prevent delegate calls
            check_delegatecall(
                NATIVE_COIN_AUTHORITY_ADDRESS,
                &precompile_input,
                &mut gas_counter,
            )?;

            // Reject transfers involving zero address (Zero5+)
            if hardfork_flags.is_active(ArcHardfork::Zero5)
                && (args.from == Address::ZERO || args.to == Address::ZERO)
            {
                return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_ZERO_ADDRESS));
            }

            // Check blocklist
            if is_blocklisted(&mut precompile_input.internals, args.from, &mut gas_counter, hardfork_flags)? {
                return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_BLOCKED_ADDRESS));
            }
            if is_blocklisted(&mut precompile_input.internals, args.to, &mut gas_counter, hardfork_flags)? {
                return Err(PrecompileErrorOrRevert::new_reverted(gas_counter, ERR_BLOCKED_ADDRESS));
            }

            // Zero amount transfers are allowed, but do not emit an event
            if args.amount != U256::ZERO {
                // Note on self-transfers (from == to): REVM's transfer_loaded() early-returns
                // without touching balances for self-transfers. Here we still call transfer()
                // which performs balance_decr + balance_incr (net zero change). This is
                // functionally correct and intentionally kept to preserve identical gas costs
                // across the Zero5 hardfork boundary — skipping the balance ops would reduce
                // gas consumption and break the "gas cost unchanged" invariant.
                transfer(&mut precompile_input.internals, args.from, args.to, args.amount, &mut gas_counter, false,
                     hardfork_flags.is_active(ArcHardfork::Zero5))?;

                // Emit event: ERC-20 Transfer under Zero5, NativeCoinTransferred otherwise.
                // EIP-7708: self-transfers (from == to) do not emit a log.
                if hardfork_flags.is_active(ArcHardfork::Zero5) {
                    if args.from != args.to {
                        emit_event(&mut precompile_input.internals, SYSTEM_ADDRESS, &Transfer {
                            from: args.from,
                            to: args.to,
                            value: args.amount,
                        }, &mut gas_counter)?;
                    }
                } else {
                    emit_event(&mut precompile_input.internals, NATIVE_COIN_AUTHORITY_ADDRESS, &NativeCoinTransferred {
                        from: args.from,
                        to: args.to,
                        amount: args.amount,
                    }, &mut gas_counter)?;
                }
            }

            // Return response
            let output = true.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into()))
        })()
    },
    INativeCoinAuthority::totalSupplyCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let mut precompile_input = precompile_input;

            // Validate the input is correct
            if !input.is_empty() {
                return Err(PrecompileErrorOrRevert::new_reverted_with_penalty(
                    gas_counter, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED));
            }

            // Early return if not enough gas
            check_gas_remaining(&gas_counter, TOTAL_SUPPLY_GAS_COST)?;

            // Read the total supply
            let total_supply_output = read(
                &mut precompile_input.internals,
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY,
                &mut gas_counter,
                hardfork_flags,
            )?;
            let total_supply = U256::from_be_slice(&total_supply_output);

            // Return response
            let output = total_supply.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into()))
        })()
    },
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{
        ERR_CLEAR_EMPTY, ERR_DELEGATE_CALL_NOT_ALLOWED, ERR_INSUFFICIENT_FUNDS,
        ERR_SELFDESTRUCTED_BALANCE_INCREASED, REVERT_SELECTOR,
    };
    use crate::native_coin_control::{
        compute_is_blocklisted_storage_slot, run_native_coin_control, BLOCKLISTED_STATUS,
        NATIVE_COIN_CONTROL_ADDRESS, UNBLOCKLISTED_STATUS,
    };
    use alloy_primitives::Bytes;
    use alloy_sol_types::SolEvent;
    use arc_execution_config::hardforks::{ArcHardfork, ArcHardforkFlags};
    use reth_ethereum::evm::revm::{
        context::{Context, ContextTr, JournalTr},
        interpreter::{CallInput, CallInputs, CallScheme, CallValue, InstructionResult},
        MainContext,
    };
    use reth_evm::precompiles::{DynPrecompile, PrecompilesMap};
    use revm::{
        bytecode::{Bytecode, JumpTable, LegacyAnalyzedBytecode},
        handler::PrecompileProvider,
        interpreter::InterpreterResult,
        precompile::{PrecompileId, Precompiles},
    };
    use revm_context_interface::journaled_state::account::JournaledAccountTr;
    use revm_primitives::b256;
    use std::collections::HashSet;

    fn mock_context(hardfork_flags: ArcHardforkFlags) -> revm::Context {
        let mut ctx = Context::mainnet();

        // Set up native coin authority
        ctx.journal_mut()
            .load_account(NATIVE_COIN_AUTHORITY_ADDRESS)
            .expect("Unable to load native coin authority account");

        if !hardfork_flags.is_active(ArcHardfork::Zero5) {
            ctx.journal_mut()
                .sstore(
                    NATIVE_COIN_AUTHORITY_ADDRESS,
                    ALLOWED_CALLER_STORAGE_KEY.into(),
                    U256::from_be_slice(ALLOWED_CALLER_ADDRESS.as_ref()),
                )
                .expect("Unable to write allowed caller");
        }

        // Preload native coin control account, it will load storage slot in tests.
        ctx.journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .expect("Unable to load native coin authority account");

        ctx
    }

    fn call_native_coin_authority(
        ctx: &mut Context,
        inputs: &CallInputs,
        hardfork_flags: ArcHardforkFlags,
    ) -> Result<Option<InterpreterResult>, String> {
        // The EvmInternals has no public constructor, so we can not test DynPrecompile directly.
        let mut provider = PrecompilesMap::from_static(Precompiles::latest());
        let target_addr: Address = inputs.target_address;
        provider.set_precompile_lookup(move |address: &Address| {
            if *address == NATIVE_COIN_AUTHORITY_ADDRESS
                || target_addr == NATIVE_COIN_AUTHORITY_ADDRESS
            {
                Some(DynPrecompile::new_stateful(
                    PrecompileId::Custom("NATIVE_COIN_AUTHORITY".into()),
                    move |input| run_native_coin_authority(input, hardfork_flags),
                ))
            } else if *address == NATIVE_COIN_CONTROL_ADDRESS {
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
    struct NativeCoinAuthorityTest {
        name: &'static str,
        caller: Address,
        calldata: Bytes,
        gas_limit: u64,
        /// If set, overrides gas_limit for pre-Zero5 hardforks
        pre_zero5_gas_limit: Option<u64>,
        /// If set, overrides gas_limit when EIP-7708 (Zero5) is active (different event costs)
        eip7708_gas_limit: Option<u64>,
        expected_revert_str: Option<&'static str>,
        expected_result: InstructionResult,
        return_data: Option<Bytes>,
        blocklisted_addresses: Option<HashSet<Address>>,
        gas_used: u64,
        /// If set, overrides gas_used for pre-Zero5 hardforks (before warm/cold aware storage costs)
        pre_zero5_gas_used: Option<u64>,
        /// If set, overrides gas_used when EIP-7708 (Zero5) is active.
        eip7708_gas_used: Option<u64>,
        target_address: Address,
        bytecode_address: Address,
        /// If true, skip this test case for hardfork combinations without Zero5 (EIP-7708).
        /// Defaults to false when not specified (via `..Default::default()`).
        eip7708_only: bool,
        /// If true, skip this test case for hardfork combinations with Zero5 (EIP-7708).
        /// Used for OOG tests whose gas boundary changes when EIP-7708 suppresses event emission.
        pre_eip7708_only: bool,
    }

    impl Default for NativeCoinAuthorityTest {
        fn default() -> Self {
            Self {
                name: "",
                caller: Address::ZERO,
                calldata: Bytes::new(),
                gas_limit: 0,
                pre_zero5_gas_limit: None,
                eip7708_gas_limit: None,
                expected_revert_str: None,
                expected_result: InstructionResult::Stop,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: None,
                eip7708_gas_used: None,
                target_address: Address::ZERO,
                bytecode_address: Address::ZERO,
                eip7708_only: false,
                pre_eip7708_only: false,
            }
        }
    }

    // Test constants
    const ADDRESS_A: Address = address!("1000000000000000000000000000000000000001");
    const ADDRESS_B: Address = address!("2000000000000000000000000000000000000002");
    const ADDRESS_C: Address = address!("300000D000000000000000000000000000000003");
    const NON_EMPTY_ADDRESS: Address = address!("400000D000000000000000000000000000000004");

    fn assert_precompile_result(
        precompile_res: Result<Option<InterpreterResult>, String>,
        tc: &NativeCoinAuthorityTest,
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

                // Resolve expected gas based on hardfork: Zero5 enables EIP-7708 events.
                let expected_gas_used = if hardfork_flags.is_active(ArcHardfork::Zero5) {
                    tc.eip7708_gas_used.unwrap_or(tc.gas_used)
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

    /// Sets up blocklist entries in the test context
    fn setup_blocklist(ctx: &mut Context, blocklisted_addresses: &Option<HashSet<Address>>) {
        if let Some(addresses) = blocklisted_addresses {
            for &address in addresses {
                let storage_slot = compute_is_blocklisted_storage_slot(address);
                ctx.journal_mut()
                    .sstore(
                        NATIVE_COIN_CONTROL_ADDRESS,
                        storage_slot.into(),
                        BLOCKLISTED_STATUS,
                    )
                    .expect("Unable to set blocklist status");
            }
        }
    }

    /// Cleans up blocklist entries after test
    fn cleanup_blocklist(ctx: &mut Context, blocklisted_addresses: &Option<HashSet<Address>>) {
        if let Some(addresses) = blocklisted_addresses {
            for &address in addresses {
                let storage_slot = compute_is_blocklisted_storage_slot(address);
                ctx.journal_mut()
                    .sstore(
                        NATIVE_COIN_CONTROL_ADDRESS,
                        storage_slot.into(),
                        UNBLOCKLISTED_STATUS,
                    )
                    .expect("Unable to clear blocklist status");
            }
        }
    }

    /// Sets up initial state for test context (total supply, balances, code)
    fn setup_initial_state(ctx: &mut Context, mock_initial_supply: U256) {
        // Configure initial total supply
        ctx.journal_mut()
            .sstore(
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY.into(),
                mock_initial_supply,
            )
            .expect("Unable to write initial total supply");

        // Configure initial balance for ADDRESS_A
        ctx.journal_mut()
            .load_account(ADDRESS_A)
            .expect("Cannot load account");
        ctx.journal_mut()
            .balance_incr(ADDRESS_A, mock_initial_supply)
            .expect("Unable to write initial balance for ADDRESS_A");

        // Configure a non-empty state for NON_EMPTY_ADDRESS
        ctx.journal_mut()
            .load_account(NON_EMPTY_ADDRESS)
            .expect("Cannot load account");
        ctx.journal_mut().set_code_with_hash(
            NON_EMPTY_ADDRESS,
            Bytecode::LegacyAnalyzed(std::sync::Arc::new(LegacyAnalyzedBytecode::new(
                Bytes::from_static(&[0x60, 0x00, 0x60, 0x00, 0x56]), // PUSH1 0x00 PUSH1 0x00 JUMP
                JumpTable::default().len(),
                JumpTable::default(),
            ))),
            b256!("56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b422"),
        );
        ctx.journal_mut()
            .balance_incr(NON_EMPTY_ADDRESS, mock_initial_supply)
            .expect("Unable to write initial balance for NON_EMPTY_ADDRESS");
    }

    /// Returns the appropriate gas limit based on hardfork
    fn get_gas_limit(tc: &NativeCoinAuthorityTest, hardfork_flags: ArcHardforkFlags) -> u64 {
        if hardfork_flags.is_active(ArcHardfork::Zero5) {
            tc.eip7708_gas_limit.unwrap_or(tc.gas_limit)
        } else {
            tc.pre_zero5_gas_limit.unwrap_or(tc.gas_limit)
        }
    }

    /// Validates test case configuration
    fn validate_test_case(tc: &NativeCoinAuthorityTest) {
        match tc.expected_result {
            InstructionResult::Revert | InstructionResult::Return => {}
            _ => {
                assert!(
                    tc.return_data.is_none(),
                    "{}: expected no return data",
                    tc.name
                );
            }
        }
    }

    #[test]
    // These tests test the outputs of the native coin authority precompile, such as
    // the InstructionResult, error conditions, and revert messages.

    // Tests for the state side effects (balance mutations and events) are tested separately
    fn native_coin_authority_precompile_outputs() {
        // Put the initial supply in a constant
        let mock_initial_supply = U256::from(1_000_000_000);

        let cases: &[NativeCoinAuthorityTest] = &[
            // Authorization check is now a constant comparison (no SLOAD), reverts with 0 gas
            NativeCoinAuthorityTest {
                name: "mint() unauthorized caller reverts",
                caller: ADDRESS_A,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::from(1000),
                }
                .abi_encode()
                .into(),
                gas_limit: 100_000,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_CANNOT_MINT),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, blocklist SLOAD is cold (2100), reverts before other ops
            NativeCoinAuthorityTest {
                name: "mint() zero amount reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::ZERO,
                }
                .abi_encode()
                .into(),
                gas_limit: 100_000,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_ZERO_AMOUNT),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 2100, // blocklist check cold SLOAD only
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST * 2),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, blocklist cold (2100), total supply warm (100) - warm because test setup writes it
            NativeCoinAuthorityTest {
                name: "mint() reverts on overflow",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::MAX - mock_initial_supply + U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: 100_000,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_OVERFLOW),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 2200, // blocklist cold (2100) + total_supply warm (100)
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST * 3),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "mint() insufficient gas errors with OOG",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                // Pre-Zero5: uses early gas check with MINT_GAS_COST - PRECOMPILE_SLOAD_GAS_COST - 1
                // Zero5 (EIP-7708): uses Transfer event (9056 gas needed), so 8680 still triggers OOG
                gas_limit: 8680,
                pre_zero5_gas_limit: Some(MINT_GAS_COST - PRECOMPILE_SLOAD_GAS_COST - 1),
                expected_revert_str: None,
                expected_result: InstructionResult::PrecompileOOG,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0, // for OOG, it not the responsibility for the precompile layer
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                pre_eip7708_only: true,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "mint() invalid params errors with Execution Reverted",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall::SELECTOR.into(),
                gas_limit: MINT_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "mint() prevents calls if target != precompile address",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall::SELECTOR.into(),
                gas_limit: MINT_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY,
                pre_zero5_gas_used: None,
                target_address: ADDRESS_B,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, delegate check happens before any storage ops
            NativeCoinAuthorityTest {
                name: "mint() prevents calls if bytecode_address != precompile address",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: MINT_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0, // No auth SLOAD in Zero5
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST), // as it comes after the authorization check SLOAD
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: ADDRESS_B, // different bytecode address
                ..Default::default()
            },
            // No auth SLOAD, delegate check happens before any storage ops
            NativeCoinAuthorityTest {
                name: "mint() prevents calls if target_address != precompile address",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: MINT_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0, // No auth SLOAD in Zero5
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST), // as it comes after the authorization check SLOAD
                target_address: ADDRESS_B,                           // different target address
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // blocklist cold (2100) + total_supply warm read (100) + total_supply warm write (100)
            // + balance_incr fixed (5000) + event (1381) = 8681
            // EIP-7708 (Zero5): Transfer event (3 topics) replaces NativeCoinMinted (2 topics)
            NativeCoinAuthorityTest {
                name: "mint() success and returns true",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: MINT_GAS_COST,
                pre_zero5_gas_limit: None,
                eip7708_gas_limit: Some(MINT_GAS_COST_EIP7708),
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 8681,
                pre_zero5_gas_used: Some(MINT_GAS_COST),
                eip7708_gas_used: Some(9056),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, reverts immediately
            NativeCoinAuthorityTest {
                name: "burn() with unauthorized caller reverts",
                caller: ADDRESS_A,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: U256::from(1000),
                }
                .abi_encode()
                .into(),
                gas_limit: BURN_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_CANNOT_BURN),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, blocklist SLOAD cold (2100), reverts before balance ops
            NativeCoinAuthorityTest {
                name: "burn() with zero amount reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: U256::ZERO,
                }
                .abi_encode()
                .into(),
                gas_limit: BURN_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_ZERO_AMOUNT),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 2100, // blocklist cold SLOAD only
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST * 2),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, blocklist cold (2100), balance check SLOAD (fixed 2100)
            NativeCoinAuthorityTest {
                name: "burn() more than balance reverts with insufficient funds",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: mock_initial_supply + U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: BURN_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_INSUFFICIENT_FUNDS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 4200, // blocklist cold + balance check fixed
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST * 3),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "burn() with insufficient gas errors with OOG",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                // Pre-Zero5: uses early gas check with BURN_GAS_COST - PRECOMPILE_SLOAD_GAS_COST - 1
                // Zero5 (EIP-7708): uses Transfer event (9056 gas needed), so 8680 still triggers OOG
                gas_limit: 8680,
                pre_zero5_gas_limit: Some(BURN_GAS_COST - PRECOMPILE_SLOAD_GAS_COST - 1),
                expected_revert_str: None,
                expected_result: InstructionResult::PrecompileOOG,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                pre_eip7708_only: true,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "burn() with invalid params reverts with Execution Reverted",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall::SELECTOR.into(),
                gas_limit: BURN_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, delegate check happens before storage ops
            NativeCoinAuthorityTest {
                name: "burn() reverts if target address != precompile address",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: BURN_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: ADDRESS_B,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, delegate check happens before storage ops
            NativeCoinAuthorityTest {
                name: "burn() reverts if bytecode address != precompile address",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: BURN_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: ADDRESS_B,
                ..Default::default()
            },
            // blocklist cold (2100) + balance_decr fixed (5000) + total_supply warm read (100)
            // + total_supply warm write (100) + event (1381) = 8681
            // EIP-7708 (Zero5): Transfer event (3 topics) replaces NativeCoinBurned (2 topics)
            NativeCoinAuthorityTest {
                name: "burn() succeeds and returns true",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: BURN_GAS_COST,
                pre_zero5_gas_limit: None,
                eip7708_gas_limit: Some(BURN_GAS_COST_EIP7708),
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 8681,
                pre_zero5_gas_used: Some(BURN_GAS_COST),
                eip7708_gas_used: Some(9056),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, reverts immediately
            NativeCoinAuthorityTest {
                name: "transfer() with unauthorized caller reverts",
                caller: ADDRESS_A,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(1000),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_CANNOT_TRANSFER),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, from blocklist cold (2100), to blocklist cold (2100), balance check fixed (2100)
            NativeCoinAuthorityTest {
                name: "transfer() more than balance reverts with insufficient funds",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: mock_initial_supply + U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_INSUFFICIENT_FUNDS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 6300, // 2 blocklist cold SLOADs (4200) + balance check (2100)
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST * 4),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "transfer() with insufficient gas errors with OOG",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                // Zero5: 15956 gas needed for success, use 15955 to trigger OOG
                // Pre-Zero5: uses early gas check with TRANSFER_GAS_COST - PRECOMPILE_SLOAD_GAS_COST - 1
                gas_limit: 15955,
                pre_zero5_gas_limit: Some(TRANSFER_GAS_COST - PRECOMPILE_SLOAD_GAS_COST - 1),
                expected_revert_str: None,
                expected_result: InstructionResult::PrecompileOOG,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "transfer() with invalid params reverts with Execution Reverted",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall::SELECTOR.into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, delegate check happens before storage ops
            NativeCoinAuthorityTest {
                name: "transfer() reverts if target address != precompile address",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: ADDRESS_B,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, delegate check happens before storage ops
            NativeCoinAuthorityTest {
                name: "transfer() reverts if bytecode address != precompile address",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: ADDRESS_B,
                ..Default::default()
            },
            // Zero address checks (Zero5+) happen before blocklist SLOADs, so gas_used = 0
            NativeCoinAuthorityTest {
                name: "transfer() to zero address reverts (Zero5+)",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: Address::ZERO,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_ZERO_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                eip7708_only: true,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "transfer() from zero address reverts (Zero5+)",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: Address::ZERO,
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_ZERO_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                eip7708_only: true,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "transfer() with both zero addresses reverts (Zero5+)",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: Address::ZERO,
                    to: Address::ZERO,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_ZERO_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                eip7708_only: true,
                ..Default::default()
            },
            // from blocklist cold (2100) + to blocklist cold (2100) + transfer fixed (10000)
            // + event (1756) = 15956
            NativeCoinAuthorityTest {
                name: "transfer() with non-zero amount succeeds and returns true",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 15956,
                pre_zero5_gas_used: Some(TRANSFER_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, from/to blocklist cold (4200), balance check (2100), balance decr SSTORE penalty (2900)
            NativeCoinAuthorityTest {
                name: "transfer() full balance for empty account reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(mock_initial_supply),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_CLEAR_EMPTY),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 9200, // 2 blocklist cold (4200) + balance (2100) + SSTORE penalty (2900)
                pre_zero5_gas_used: Some(
                    PRECOMPILE_SLOAD_GAS_COST * 4 + PRECOMPILE_SSTORE_GAS_COST,
                ),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // from blocklist cold (2100) + to blocklist cold (2100) + transfer fixed (10000)
            // + event (1756) = 15956
            NativeCoinAuthorityTest {
                name: "transfer() full balance for non-empty account does not revert",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: NON_EMPTY_ADDRESS,
                    to: ADDRESS_B,
                    amount: U256::from(mock_initial_supply),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 15956,
                pre_zero5_gas_used: Some(TRANSFER_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // No auth SLOAD, from/to blocklist cold (4200) - no balance ops for zero amount
            NativeCoinAuthorityTest {
                name: "transfer() with zero amount succeeds and returns true",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::ZERO,
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 4200, // 2 blocklist cold SLOADs
                pre_zero5_gas_used: Some(TRANSFER_GAS_COST_WITH_ZERO_AMOUNT),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // total_supply warm SLOAD (100) - test setup writes it
            NativeCoinAuthorityTest {
                name: "totalSupply() returns the total supply",
                caller: ADDRESS_A,
                calldata: INativeCoinAuthority::totalSupplyCall::SELECTOR.into(),
                gas_limit: TOTAL_SUPPLY_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(mock_initial_supply.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 100,
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            NativeCoinAuthorityTest {
                name: "totalSupply() errors with OOG if insufficient gas",
                caller: ADDRESS_A,
                calldata: INativeCoinAuthority::totalSupplyCall::SELECTOR.into(),
                gas_limit: TOTAL_SUPPLY_GAS_COST - 1, // Not enough gas
                pre_zero5_gas_limit: None,
                expected_revert_str: None,
                expected_result: InstructionResult::PrecompileOOG,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                pre_zero5_gas_used: None,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // Blocklist test cases
            // blocklist warm (100) - test setup writes to blocklist slot, making it warm
            NativeCoinAuthorityTest {
                name: "mint() to blocklisted address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::from(1000),
                }
                .abi_encode()
                .into(),
                gas_limit: MINT_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_BLOCKED_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: Some(HashSet::from([ADDRESS_B])),
                gas_used: 100, // blocklist warm (test setup wrote it)
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST * 2),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // blocklist cold (2100) + total_supply warm read (100) + total_supply warm write (100)
            // + balance_incr fixed (5000) + event (1381) = 8681
            // EIP-7708 (Zero5): Transfer event (3 topics) replaces NativeCoinMinted (2 topics)
            NativeCoinAuthorityTest {
                name: "mint() to non-blocklisted address succeeds",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::from(1000),
                }
                .abi_encode()
                .into(),
                gas_limit: MINT_GAS_COST,
                pre_zero5_gas_limit: None,
                eip7708_gas_limit: Some(MINT_GAS_COST_EIP7708),
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: Some(HashSet::from([ADDRESS_C])),
                gas_used: 8681,
                pre_zero5_gas_used: Some(MINT_GAS_COST),
                eip7708_gas_used: Some(9056),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // blocklist warm (100) - test setup writes to blocklist slot, making it warm
            NativeCoinAuthorityTest {
                name: "burn() from blocklisted address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_B,
                    amount: U256::from(1000),
                }
                .abi_encode()
                .into(),
                gas_limit: BURN_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_BLOCKED_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: Some(HashSet::from([ADDRESS_B])),
                gas_used: 100, // blocklist warm (test setup wrote it)
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST * 2),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // from blocklist warm (100) - test setup writes to blocklist slot
            NativeCoinAuthorityTest {
                name: "transfer() from blocklisted address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(1000),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_BLOCKED_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: Some(HashSet::from([ADDRESS_A])),
                gas_used: 100, // from blocklist warm (test setup wrote it)
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST * 2),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
            // from blocklist cold (2100), to blocklist warm (100) - test setup writes to ADDRESS_B slot
            NativeCoinAuthorityTest {
                name: "transfer() to blocklisted address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(1000),
                }
                .abi_encode()
                .into(),
                gas_limit: TRANSFER_GAS_COST,
                pre_zero5_gas_limit: None,
                expected_revert_str: Some(ERR_BLOCKED_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: Some(HashSet::from([ADDRESS_B])),
                gas_used: 2200, // from blocklist cold (2100) + to blocklist warm (100)
                pre_zero5_gas_used: Some(PRECOMPILE_SLOAD_GAS_COST * 3),
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                ..Default::default()
            },
        ];

        for tc in cases {
            for hardfork_flags in ArcHardforkFlags::all_combinations() {
                if tc.eip7708_only && !hardfork_flags.is_active(ArcHardfork::Zero5) {
                    continue;
                }
                if tc.pre_eip7708_only && hardfork_flags.is_active(ArcHardfork::Zero5) {
                    continue;
                }

                let tc_name =
                    tc.name.to_string() + &format!(" (hardfork_flags: {:?})", hardfork_flags);

                validate_test_case(tc);

                let mut ctx = mock_context(hardfork_flags);
                setup_blocklist(&mut ctx, &tc.blocklisted_addresses);
                setup_initial_state(&mut ctx, mock_initial_supply);

                let inputs = CallInputs {
                    scheme: CallScheme::Call,
                    target_address: tc.target_address,
                    bytecode_address: tc.bytecode_address,
                    known_bytecode: None,
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(tc.calldata.clone()),
                    gas_limit: get_gas_limit(tc, hardfork_flags),
                    caller: tc.caller,
                    is_static: false,
                    return_memory_offset: 0..0,
                };

                let precompile_res = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags);
                assert_precompile_result(precompile_res, tc, hardfork_flags, &tc_name);

                cleanup_blocklist(&mut ctx, &tc.blocklisted_addresses);
            }
        }
    }

    #[test]
    fn mint_side_effects() {
        for hardfork_flags in ArcHardforkFlags::all_combinations() {
            // Initial supply
            let initial_supply = U256::from(1_000_000_000);
            let mint_amount = U256::from(1000);
            let expected_total_supply = initial_supply + mint_amount;

            // Mock context with allowed caller
            let mut ctx = mock_context(hardfork_flags);

            // Set initial total supply
            ctx.journal_mut()
                .sstore(
                    NATIVE_COIN_AUTHORITY_ADDRESS,
                    TOTAL_SUPPLY_STORAGE_KEY.into(),
                    initial_supply,
                )
                .expect("Unable to write initial total supply");

            // Prepare inputs
            let inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                known_bytecode: None,
                caller: ALLOWED_CALLER_ADDRESS,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(
                    INativeCoinAuthority::mintCall {
                        to: ADDRESS_B,
                        amount: mint_amount,
                    }
                    .abi_encode()
                    .into(),
                ),
                gas_limit: 100_000,
                is_static: false,
                return_memory_offset: 0..0,
            };

            // Run precompile
            let precompile_res = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags);

            // Assert result
            assert!(precompile_res.is_ok());
            let result = precompile_res.unwrap().unwrap();
            assert_eq!(result.result, InstructionResult::Return);

            // Check total supply updated
            let total_supply_updated = ctx
                .journal_mut()
                .sload(
                    NATIVE_COIN_AUTHORITY_ADDRESS,
                    TOTAL_SUPPLY_STORAGE_KEY.into(),
                )
                .expect("Failed to read total supply after mint");

            assert_eq!(total_supply_updated.data, expected_total_supply);

            // Check recipient balance updated
            let recipient_balance = ctx
                .journal_mut()
                .load_account(ADDRESS_B)
                .expect("Failed to load recipient account")
                .info
                .balance;

            assert_eq!(recipient_balance, mint_amount);

            // Check event emission
            let journal_mut = ctx.journal_mut();
            let logs = journal_mut.logs();

            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                // Zero5 (EIP-7708): Transfer(0x0, recipient, amount) replaces NativeCoinMinted
                assert_eq!(
                    logs.len(),
                    1,
                    "Zero5: one EIP-7708 Transfer event expected for mint"
                );
                let log = &logs[0];
                assert_eq!(
                    log.address, SYSTEM_ADDRESS,
                    "Log should be from EIP-7708 system address"
                );
                let expected_log = Transfer {
                    from: Address::ZERO,
                    to: ADDRESS_B,
                    value: mint_amount,
                }
                .encode_log_data();
                assert_eq!(log.data, expected_log);
            } else {
                let expected_log = NativeCoinMinted {
                    recipient: ADDRESS_B,
                    amount: mint_amount,
                }
                .encode_log_data();
                assert_eq!(logs.len(), 1, "Expected one log event for mint");
                let log = &logs[0];
                assert_eq!(
                    log.address, NATIVE_COIN_AUTHORITY_ADDRESS,
                    "Log address mismatch"
                );
                assert_eq!(log.data, expected_log);
            }
        }
    }

    #[test]
    fn burn_side_effects() {
        for hardfork_flags in ArcHardforkFlags::all_combinations() {
            // Initial supply and burn amount
            let initial_supply = U256::from(1_000_000_000);
            let burn_amount = U256::from(1000);
            let expected_total_supply = initial_supply - burn_amount;

            // Mock context with allowed caller
            let mut ctx = mock_context(hardfork_flags);

            // Set initial total supply
            ctx.journal_mut()
                .sstore(
                    NATIVE_COIN_AUTHORITY_ADDRESS,
                    TOTAL_SUPPLY_STORAGE_KEY.into(),
                    initial_supply,
                )
                .expect("Unable to write initial total supply");

            // Set initial balance for ADDRESS_A
            ctx.journal_mut()
                .load_account(ADDRESS_A)
                .expect("Cannot load account");
            ctx.journal_mut()
                .balance_incr(ADDRESS_A, initial_supply)
                .expect("Unable to write initial balance for ADDRESS_A");

            // Prepare inputs for burn
            let inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                known_bytecode: None,
                caller: ALLOWED_CALLER_ADDRESS,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(
                    INativeCoinAuthority::burnCall {
                        from: ADDRESS_A,
                        amount: burn_amount,
                    }
                    .abi_encode()
                    .into(),
                ),
                gas_limit: 100_000,
                is_static: false,
                return_memory_offset: 0..0,
            };

            // Run precompile
            let precompile_res = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags);

            // Assert result
            assert!(precompile_res.is_ok());
            let result = precompile_res.unwrap().unwrap();
            assert_eq!(result.result, InstructionResult::Return);

            // Check total supply updated
            let total_supply_updated = ctx
                .journal_mut()
                .sload(
                    NATIVE_COIN_AUTHORITY_ADDRESS,
                    TOTAL_SUPPLY_STORAGE_KEY.into(),
                )
                .expect("Failed to read total supply after burn");

            assert_eq!(total_supply_updated.data, expected_total_supply);

            // Check account balance updated
            let account_balance = ctx
                .journal_mut()
                .load_account(ADDRESS_A)
                .expect("Failed to load account")
                .info
                .balance;

            assert_eq!(account_balance, initial_supply - burn_amount);

            // For the "current" hardfork, a burn should be a corresponding transfer
            // to the zero address. In zero4, it should be a true burn.
            let zero_account_balance = ctx
                .journal_mut()
                .load_account(Address::from([0u8; 20]))
                .expect("Failed to load zero address")
                .info
                .balance;

            assert_eq!(
                zero_account_balance,
                U256::ZERO,
                "Zero address balance should remain zero after burn"
            );

            // Check event emission
            let journal_mut: &mut revm::Journal<
                revm::database::EmptyDBTyped<std::convert::Infallible>,
            > = ctx.journal_mut();
            let logs = journal_mut.logs();

            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                // Zero5 (EIP-7708): Transfer(from, 0x0, amount) replaces NativeCoinBurned
                assert_eq!(
                    logs.len(),
                    1,
                    "Zero5: one EIP-7708 Transfer event expected for burn"
                );
                let log = &logs[0];
                assert_eq!(
                    log.address, SYSTEM_ADDRESS,
                    "Log should be from EIP-7708 system address"
                );
                let expected_log = Transfer {
                    from: ADDRESS_A,
                    to: Address::ZERO,
                    value: burn_amount,
                }
                .encode_log_data();
                assert_eq!(log.data, expected_log);
            } else {
                let expected_log = NativeCoinBurned {
                    from: ADDRESS_A,
                    amount: burn_amount,
                }
                .encode_log_data();
                assert_eq!(logs.len(), 1, "Expected one log event for burn");
                let log = &logs[0];
                assert_eq!(
                    log.address, NATIVE_COIN_AUTHORITY_ADDRESS,
                    "Log address mismatch"
                );
                assert_eq!(log.data, expected_log);
            }
        }
    }

    #[test]
    fn transfer_side_effects() {
        for hardfork_flags in ArcHardforkFlags::all_combinations() {
            // Initial supply and transfer amount
            let initial_supply = U256::from(1_000_000_000);
            let transfer_amount = U256::from(1000);

            // Mock context with allowed caller
            let mut ctx = mock_context(hardfork_flags);

            // Set initial total supply
            ctx.journal_mut()
                .sstore(
                    NATIVE_COIN_AUTHORITY_ADDRESS,
                    TOTAL_SUPPLY_STORAGE_KEY.into(),
                    initial_supply,
                )
                .expect("Unable to write initial total supply");

            // Set initial balance for ADDRESS_A
            ctx.journal_mut()
                .load_account(ADDRESS_A)
                .expect("Cannot load account");
            ctx.journal_mut()
                .balance_incr(ADDRESS_A, initial_supply)
                .expect("Unable to write initial balance for ADDRESS_A");

            // --- Case 1: Transfer zero amount ---
            let inputs_zero = CallInputs {
                scheme: CallScheme::Call,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                known_bytecode: None,
                caller: ALLOWED_CALLER_ADDRESS,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(
                    INativeCoinAuthority::transferCall {
                        from: ADDRESS_A,
                        to: ADDRESS_B,
                        amount: U256::ZERO,
                    }
                    .abi_encode()
                    .into(),
                ),
                gas_limit: 100_000,
                is_static: false,
                return_memory_offset: 0..0,
            };

            let precompile_res_zero =
                call_native_coin_authority(&mut ctx, &inputs_zero, hardfork_flags);
            assert!(precompile_res_zero.is_ok());
            let result_zero = precompile_res_zero.unwrap().unwrap();
            assert_eq!(result_zero.result, InstructionResult::Return);

            // Check total supply unchanged
            let total_supply_after_zero = ctx
                .journal_mut()
                .sload(
                    NATIVE_COIN_AUTHORITY_ADDRESS,
                    TOTAL_SUPPLY_STORAGE_KEY.into(),
                )
                .expect("Failed to read total supply after zero transfer");
            assert_eq!(total_supply_after_zero.data, initial_supply);

            // Check balances unchanged
            let balance_a_zero = ctx
                .journal_mut()
                .load_account(ADDRESS_A)
                .expect("Failed to load account A")
                .info
                .balance;
            let balance_b_zero = ctx
                .journal_mut()
                .load_account(ADDRESS_B)
                .expect("Failed to load account B")
                .info
                .balance;

            assert_eq!(balance_a_zero, initial_supply);
            assert_eq!(balance_b_zero, U256::ZERO);

            // Check no event emitted
            let logs_zero = ctx.journal().logs();
            assert_eq!(
                logs_zero.len(),
                0,
                "No event should be emitted for zero transfer"
            );

            // --- Case 2: Transfer non-zero amount ---
            let inputs_nonzero = CallInputs {
                scheme: CallScheme::Call,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                known_bytecode: None,
                caller: ALLOWED_CALLER_ADDRESS,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(
                    INativeCoinAuthority::transferCall {
                        from: ADDRESS_A,
                        to: ADDRESS_B,
                        amount: transfer_amount,
                    }
                    .abi_encode()
                    .into(),
                ),
                gas_limit: 100_000,
                is_static: false,
                return_memory_offset: 0..0,
            };

            let precompile_res_nonzero =
                call_native_coin_authority(&mut ctx, &inputs_nonzero, hardfork_flags);
            assert!(precompile_res_nonzero.is_ok());
            let result_nonzero = precompile_res_nonzero.unwrap().unwrap();
            assert_eq!(result_nonzero.result, InstructionResult::Return);

            // Check total supply unchanged
            let total_supply_after_nonzero = ctx
                .journal_mut()
                .sload(
                    NATIVE_COIN_AUTHORITY_ADDRESS,
                    TOTAL_SUPPLY_STORAGE_KEY.into(),
                )
                .expect("Failed to read total supply after nonzero transfer");
            assert_eq!(total_supply_after_nonzero.data, initial_supply);

            // Check balances updated
            let balance_a_nonzero = ctx
                .journal_mut()
                .load_account(ADDRESS_A)
                .expect("Failed to load account A")
                .info
                .balance;
            let balance_b_nonzero = ctx
                .journal_mut()
                .load_account(ADDRESS_B)
                .expect("Failed to load account B")
                .info
                .balance;
            assert_eq!(balance_a_nonzero, initial_supply - transfer_amount);
            assert_eq!(balance_b_nonzero, transfer_amount);

            // Check event emission
            let logs_nonzero = ctx.journal().logs();
            assert_eq!(logs_nonzero.len(), 1, "Expected one log event for transfer");
            let log = &logs_nonzero[0];

            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                // Zero5 (EIP-7708): ERC-20 Transfer event with EIP-7708 emitter address
                let expected_log = Transfer {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    value: transfer_amount,
                }
                .encode_log_data();
                assert_eq!(
                    log.address, SYSTEM_ADDRESS,
                    "Zero5: log emitter should be SYSTEM_ADDRESS"
                );
                assert_eq!(log.data, expected_log);
            } else {
                // Pre-Zero5: NativeCoinTransferred event with native coin authority address
                let expected_log = NativeCoinTransferred {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: transfer_amount,
                }
                .encode_log_data();
                assert_eq!(
                    log.address, NATIVE_COIN_AUTHORITY_ADDRESS,
                    "Pre-Zero5: log emitter should be NATIVE_COIN_AUTHORITY_ADDRESS"
                );
                assert_eq!(log.data, expected_log);
            }
        }
    }

    /// EIP-7708: self-transfers (from == to) do not emit a Transfer log under Zero5.
    /// Pre-Zero5: self-transfers still emit NativeCoinTransferred.
    #[test]
    fn transfer_self_transfer_no_log_under_zero5() {
        for hardfork_flags in ArcHardforkFlags::all_combinations() {
            let initial_supply = U256::from(1_000_000_000);
            let transfer_amount = U256::from(1000);

            let mut ctx = mock_context(hardfork_flags);

            ctx.journal_mut()
                .sstore(
                    NATIVE_COIN_AUTHORITY_ADDRESS,
                    TOTAL_SUPPLY_STORAGE_KEY.into(),
                    initial_supply,
                )
                .expect("Unable to write initial total supply");

            ctx.journal_mut()
                .load_account(ADDRESS_A)
                .expect("Cannot load account");
            ctx.journal_mut()
                .balance_incr(ADDRESS_A, initial_supply)
                .expect("Unable to write initial balance for ADDRESS_A");

            // Self-transfer: from == to == ADDRESS_A
            let inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                known_bytecode: None,
                caller: ALLOWED_CALLER_ADDRESS,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(
                    INativeCoinAuthority::transferCall {
                        from: ADDRESS_A,
                        to: ADDRESS_A,
                        amount: transfer_amount,
                    }
                    .abi_encode()
                    .into(),
                ),
                gas_limit: 100_000,
                is_static: false,
                return_memory_offset: 0..0,
            };

            let precompile_res = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags);
            assert!(precompile_res.is_ok());
            let result = precompile_res.unwrap().unwrap();
            assert_eq!(result.result, InstructionResult::Return);

            // Balance should be unchanged (self-transfer)
            let balance = ctx
                .journal_mut()
                .load_account(ADDRESS_A)
                .expect("Failed to load account A")
                .info
                .balance;
            assert_eq!(balance, initial_supply);

            let logs = ctx.journal().logs();

            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                // Zero5 + EIP-7708: self-transfers do not emit a log
                assert_eq!(logs.len(), 0, "Zero5: self-transfer should not emit a log");

                // Gas accounting: self-transfer still executes the full transfer path
                // (balance_decr + balance_incr) to preserve gas invariants across the
                // hardfork boundary — only the event emission is suppressed.
                //
                // Breakdown:
                //   blocklist(from) cold SLOAD:  2100
                //   blocklist(to)  warm SLOAD:    100  (same address, slot already warm)
                //   transfer() fixed:           10000  (2 SLOADs + 2 SSTOREs)
                //   event: skipped (from == to)
                let expected_gas =
                    2100 + 100 + 2 * PRECOMPILE_SLOAD_GAS_COST + 2 * PRECOMPILE_SSTORE_GAS_COST;
                assert_eq!(
                    result.gas.used(),
                    expected_gas,
                    "Zero5: self-transfer gas should include transfer path but no event (flags={hardfork_flags:?})",
                );
            } else {
                // Pre-Zero5: self-transfers still emit NativeCoinTransferred
                assert_eq!(
                    logs.len(),
                    1,
                    "Pre-Zero5: self-transfer emits NativeCoinTransferred"
                );
                let log = &logs[0];
                assert_eq!(log.address, NATIVE_COIN_AUTHORITY_ADDRESS);
                let expected_log = NativeCoinTransferred {
                    from: ADDRESS_A,
                    to: ADDRESS_A,
                    amount: transfer_amount,
                }
                .encode_log_data();
                assert_eq!(log.data, expected_log);

                // Gas accounting: auth SLOAD + 2 blocklist SLOADs + transfer + event
                assert_eq!(
                    result.gas.used(),
                    TRANSFER_GAS_COST,
                    "Pre-Zero5: self-transfer gas should match TRANSFER_GAS_COST (flags={hardfork_flags:?})",
                );
            }
        }
    }

    // Helper to convert bytes to a revert error string
    fn bytes_to_revert_message(input: &[u8]) -> Option<String> {
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
                "mint",
                INativeCoinAuthority::mintCall {
                    to: ADDRESS_A,
                    amount: U256::from(100),
                }
                .abi_encode()
                .into(),
            ),
            (
                "burn",
                INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: U256::from(100),
                }
                .abi_encode()
                .into(),
            ),
            (
                "transfer",
                INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(100),
                }
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
                    target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                    bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                    known_bytecode: None,
                    caller: ALLOWED_CALLER_ADDRESS,
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(calldata.clone()),
                    gas_limit: 100_000,
                    is_static: true,
                    return_memory_offset: 0..0,
                };

                let result = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags)
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

            // Read-only function (totalSupply) must succeed under static call
            {
                let mut ctx = mock_context(hardfork_flags);
                let inputs = CallInputs {
                    scheme: CallScheme::Call,
                    target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                    bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                    known_bytecode: None,
                    caller: ADDRESS_A,
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(
                        INativeCoinAuthority::totalSupplyCall {}.abi_encode().into(),
                    ),
                    gas_limit: 100_000,
                    is_static: true,
                    return_memory_offset: 0..0,
                };

                let result = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags)
                    .expect("call should not error")
                    .expect("result should be Some");

                assert_eq!(
                    result.result,
                    InstructionResult::Return,
                    "totalSupply (hardfork_flags: {hardfork_flags:?}): expected Return under static call",
                );
            }
        }
    }

    #[test]
    fn transfer_or_mint_to_selfdestructed_account_should_revert() {
        let amount = U256::from(1000);
        let hardfork_flags = ArcHardforkFlags::with(&[ArcHardfork::Zero4, ArcHardfork::Zero5]);
        let mut ctx = mock_context(hardfork_flags);
        let spec_id = ctx.cfg.spec;

        // Prepare ADDRESS_A as a destructed account.
        let journal = ctx.journal_mut();
        journal
            .load_account_mut_optional_code(ADDRESS_A, false)
            .expect("load ADDRESS_A")
            .set_balance(amount + amount);
        journal.load_account(ADDRESS_B).expect("load ADDRESS_B");
        journal
            .create_account_checkpoint(ADDRESS_A, ADDRESS_B, amount, spec_id)
            .unwrap();
        journal
            .selfdestruct(ADDRESS_B, ADDRESS_A, false)
            .expect("selfdestruct");

        // Prepare mint inputs
        let mut inputs = CallInputs {
            scheme: CallScheme::Call,
            target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            known_bytecode: None,
            caller: ALLOWED_CALLER_ADDRESS,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(
                INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount,
                }
                .abi_encode()
                .into(),
            ),
            gas_limit: 100_000,
            is_static: false,
            return_memory_offset: 0..0,
        };

        // Mint to destructed account should revert
        let precompile_res = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags);
        assert!(precompile_res.is_ok());
        let result = precompile_res.unwrap().unwrap();
        assert_eq!(result.result, InstructionResult::Revert);
        assert_eq!(
            bytes_to_revert_message(result.output.as_ref()),
            Some(ERR_SELFDESTRUCTED_BALANCE_INCREASED.to_string())
        );

        // Prepare transfer inputs
        inputs.input = CallInput::Bytes(
            INativeCoinAuthority::transferCall {
                from: ADDRESS_A,
                to: ADDRESS_B,
                amount,
            }
            .abi_encode()
            .into(),
        );

        // Transfer to destructed account should revert
        let precompile_res = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags);
        assert!(precompile_res.is_ok());
        let result = precompile_res.unwrap().unwrap();
        assert_eq!(result.result, InstructionResult::Revert);
        assert_eq!(
            bytes_to_revert_message(result.output.as_ref()),
            Some(ERR_SELFDESTRUCTED_BALANCE_INCREASED.to_string())
        );
    }
}
