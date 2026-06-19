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
    abi_decode_raw_validated, balance_decr, balance_incr, check_delegatecall, check_gas_remaining,
    check_staticcall, emit_event, new_reverted_with_early_penalty, read, transfer, write,
    PrecompileErrorOrRevert, ERR_BLOCKED_ADDRESS, ERR_EXECUTION_REVERTED,
    NATIVE_FIAT_TOKEN_ADDRESS, PRECOMPILE_EARLY_REVERT_GAS_PENALTY, PRECOMPILE_SLOAD_GAS_COST,
};
use crate::native_coin_control::{compute_is_blocklisted_storage_slot, UNBLOCKLISTED_STATUS};
use crate::precompile;
use crate::NATIVE_COIN_CONTROL_ADDRESS;
use alloy_evm::EvmInternals;
use alloy_primitives::{address, Address, StorageKey, U256};
use alloy_sol_types::{sol, SolCall, SolValue};
use reth_ethereum::evm::revm::precompile::PrecompileOutput;
use revm_interpreter::Gas;

// Native coin authority precompile address
pub const NATIVE_COIN_AUTHORITY_ADDRESS: Address =
    address!("0x1800000000000000000000000000000000000000");

use revm::handler::SYSTEM_ADDRESS;

// Allowed caller from NativeFiatToken
const ALLOWED_CALLER_ADDRESS: Address = NATIVE_FIAT_TOKEN_ADDRESS;

// Storage key for total supply
const TOTAL_SUPPLY_STORAGE_KEY: StorageKey = StorageKey::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
]);

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

    /// ERC-20 Transfer event (EIP-7708), used for native coin transfers.
    #[derive(Debug)]
    event Transfer(address indexed from, address indexed to, uint256 value);
}

fn is_blocklisted(
    internals: &mut EvmInternals,
    address: Address,
    gas_counter: &mut Gas,
    reservoir: u64,
) -> Result<bool, PrecompileErrorOrRevert> {
    // Get address storage slot for blocklist
    let storage_slot = compute_is_blocklisted_storage_slot(address);
    let storage_output = read(
        internals,
        NATIVE_COIN_CONTROL_ADDRESS,
        storage_slot,
        gas_counter,
        reservoir,
    )?;

    Ok(!U256::from_be_slice(&storage_output).eq(&UNBLOCKLISTED_STATUS))
}

precompile!(run_native_coin_authority, precompile_input, _hardfork_flags; {
    INativeCoinAuthority::mintCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let reservoir = precompile_input.reservoir;
            let mut precompile_input = precompile_input;
            // Check if static call is attempting to modify state
            check_staticcall(
                &precompile_input,
                &mut gas_counter,
            )?;

            // Decode arguments passed to mint function
            let args = abi_decode_raw_validated::<INativeCoinAuthority::mintCall>(input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, reservoir, PRECOMPILE_EARLY_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED)
                )?;

            if precompile_input.caller != ALLOWED_CALLER_ADDRESS {
                return Err(new_reverted_with_early_penalty(
                    gas_counter,
                    reservoir,
                    ERR_CANNOT_MINT,
                ));
            }

            // Prevent delegate calls
            check_delegatecall(
                NATIVE_COIN_AUTHORITY_ADDRESS,
                &precompile_input,
                &gas_counter,
            )?;

            if args.to == Address::ZERO {
                return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_ZERO_ADDRESS));
            }

            // Check blocklist
            if is_blocklisted(&mut precompile_input.internals, args.to, &mut gas_counter, reservoir)? {
                return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_BLOCKED_ADDRESS));
            }

            // Validate amount
            if args.amount == U256::ZERO {
                return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_ZERO_AMOUNT));
            }

            // Read current total supply
            let total_supply_output = read(
                &mut precompile_input.internals,
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY,
                &mut gas_counter,
                reservoir,
            )?;
            let current_total_supply = U256::from_be_slice(&total_supply_output);

            // Check for overflow
            let new_total_supply = match current_total_supply.checked_add(args.amount) {
                Some(new_total_supply) => new_total_supply,
                None => return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_OVERFLOW)),
            };

            // Write new total supply
            write(
                &mut precompile_input.internals,
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY,
                &new_total_supply.to_be_bytes_vec(),
                &mut gas_counter,
                reservoir,
            )?;

            // Update account balance
            balance_incr(&mut precompile_input.internals, args.to, args.amount, &mut gas_counter, reservoir)?;

            // Address::ZERO as `from` follows the ERC-20 convention for minting. This is
            // intentionally allowed here even though CALL/CREATE value transfers reject
            // Address::ZERO (see check_blocklist_and_create_log in evm.rs).
            emit_event(&mut precompile_input.internals, SYSTEM_ADDRESS, &Transfer {
                from: Address::ZERO,
                to: args.to,
                value: args.amount,
            }, &mut gas_counter, reservoir)?;

            let output = true.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into(), reservoir))
        })()
    },

    INativeCoinAuthority::burnCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let reservoir = precompile_input.reservoir;
            let mut precompile_input = precompile_input;
            // Check if static call is attempting to modify state
            check_staticcall(
                &precompile_input,
                &mut gas_counter,
            )?;

            // Decode arguments passed to burn function
            let args = abi_decode_raw_validated::<INativeCoinAuthority::burnCall>(input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, reservoir, PRECOMPILE_EARLY_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED,
                    )
                )?;

            if precompile_input.caller != ALLOWED_CALLER_ADDRESS {
                return Err(new_reverted_with_early_penalty(
                    gas_counter,
                    reservoir,
                    ERR_CANNOT_BURN,
                ));
            }

            // Prevent delegate calls
            check_delegatecall(
                NATIVE_COIN_AUTHORITY_ADDRESS,
                &precompile_input,
                &gas_counter,
            )?;

            if args.from == Address::ZERO {
                return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_ZERO_ADDRESS));
            }

            // Check blocklist
            if is_blocklisted(&mut precompile_input.internals, args.from, &mut gas_counter, reservoir)? {
                return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_BLOCKED_ADDRESS));
            }

            // Validate amount
            if args.amount == U256::ZERO {
                return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_ZERO_AMOUNT));
            }

            // Check balance and burn tokens
            balance_decr(&mut precompile_input.internals, args.from, args.amount, &mut gas_counter, reservoir)?;

            // Adjust total supply
            let total_supply_output = read(
                &mut precompile_input.internals,
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY,
                &mut gas_counter,
                reservoir,
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
                reservoir,
            )?;

            // Address::ZERO as `to` follows the ERC-20 convention for burning. This is
            // intentionally allowed here even though CALL/CREATE value transfers reject
            // Address::ZERO (see check_blocklist_and_create_log in evm.rs).
            emit_event(&mut precompile_input.internals, SYSTEM_ADDRESS, &Transfer {
                from: args.from,
                to: Address::ZERO,
                value: args.amount,
            }, &mut gas_counter, reservoir)?;

            // Return response
            let output = true.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into(), reservoir))
        })()
    },

    INativeCoinAuthority::transferCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let reservoir = precompile_input.reservoir;
            let mut precompile_input = precompile_input;
            // Check if static call is attempting to modify state
            check_staticcall(
                &precompile_input,
                &mut gas_counter,
            )?;

            // Decode arguments passed to transfer function
            let args = abi_decode_raw_validated::<INativeCoinAuthority::transferCall>(input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, reservoir, PRECOMPILE_EARLY_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED,
                    )
                )?;

            if precompile_input.caller != ALLOWED_CALLER_ADDRESS {
                return Err(new_reverted_with_early_penalty(
                    gas_counter,
                    reservoir,
                    ERR_CANNOT_TRANSFER,
                ));
            }

            // Prevent delegate calls
            check_delegatecall(
                NATIVE_COIN_AUTHORITY_ADDRESS,
                &precompile_input,
                &gas_counter,
            )?;

            if args.from == Address::ZERO || args.to == Address::ZERO {
                return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_ZERO_ADDRESS));
            }

            // Check blocklist
            if is_blocklisted(&mut precompile_input.internals, args.from, &mut gas_counter, reservoir)? {
                return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_BLOCKED_ADDRESS));
            }
            if is_blocklisted(&mut precompile_input.internals, args.to, &mut gas_counter, reservoir)? {
                return Err(new_reverted_with_early_penalty(gas_counter, reservoir, ERR_BLOCKED_ADDRESS));
            }

            // Zero amount transfers are allowed, but do not emit an event
            if args.amount != U256::ZERO {
                // Note on self-transfers (from == to): REVM's transfer_loaded() early-returns
                // without touching balances for self-transfers. Here we still call transfer()
                // which performs balance_decr + balance_incr (net zero change).
                transfer(&mut precompile_input.internals, args.from, args.to, args.amount, &mut gas_counter, reservoir)?;

                // EIP-7708: self-transfers (from == to) do not emit a log.
                if args.from != args.to {
                    emit_event(&mut precompile_input.internals, SYSTEM_ADDRESS, &Transfer {
                        from: args.from,
                        to: args.to,
                        value: args.amount,
                    }, &mut gas_counter, reservoir)?;
                }
            }

            // Return response
            let output = true.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into(), reservoir))
        })()
    },
    INativeCoinAuthority::totalSupplyCall => |input| {
        (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
            let mut gas_counter = Gas::new(precompile_input.gas);
            let reservoir = precompile_input.reservoir;
            let mut precompile_input = precompile_input;

            abi_decode_raw_validated::<INativeCoinAuthority::totalSupplyCall>(input)
                .map_err(|_|
                    PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, reservoir, PRECOMPILE_EARLY_REVERT_GAS_PENALTY, ERR_EXECUTION_REVERTED)
                )?;

            // Early return if not enough gas
            check_gas_remaining(&gas_counter, reservoir, TOTAL_SUPPLY_GAS_COST)?;

            // Read the total supply
            let total_supply_output = read(
                &mut precompile_input.internals,
                NATIVE_COIN_AUTHORITY_ADDRESS,
                TOTAL_SUPPLY_STORAGE_KEY,
                &mut gas_counter,
                reservoir,
            )?;
            let total_supply = U256::from_be_slice(&total_supply_output);

            // Return response
            let output = total_supply.abi_encode();
            Ok(PrecompileOutput::new(gas_counter.used(), output.into(), reservoir))
        })()
    },
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{
        ERR_CLEAR_EMPTY, ERR_DELEGATE_CALL_NOT_ALLOWED, ERR_INSUFFICIENT_FUNDS,
        ERR_SELFDESTRUCTED_BALANCE_INCREASED, PRECOMPILE_SSTORE_GAS_COST, REVERT_SELECTOR,
    };
    use crate::native_coin_control::{
        compute_is_blocklisted_storage_slot, run_native_coin_control, BLOCKLISTED_STATUS,
        NATIVE_COIN_CONTROL_ADDRESS, UNBLOCKLISTED_STATUS,
    };
    use alloy_primitives::{Bytes, B256};
    use alloy_sol_types::SolEvent;
    use arc_execution_config::hardforks::{ArcHardfork, ArcHardforkFlags};
    use reth_ethereum::evm::revm::{
        context::{Context, ContextTr, JournalTr},
        interpreter::{CallInput, CallInputs, CallScheme, CallValue, InstructionResult},
        MainContext,
    };
    use reth_evm::precompiles::{DynPrecompile, PrecompilesMap};
    use revm::{
        bytecode::Bytecode,
        handler::PrecompileProvider,
        interpreter::InterpreterResult,
        precompile::{PrecompileId, Precompiles},
    };
    use revm_context_interface::journaled_state::account::JournaledAccountTr;
    use std::collections::HashSet;

    fn mock_context(_hardfork_flags: ArcHardforkFlags) -> revm::Context {
        let mut ctx = Context::mainnet();

        // Set up native coin authority
        ctx.journal_mut()
            .load_account(NATIVE_COIN_AUTHORITY_ADDRESS)
            .expect("Unable to load native coin authority account");

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
        expected_revert_str: Option<&'static str>,
        expected_result: InstructionResult,
        return_data: Option<Bytes>,
        blocklisted_addresses: Option<HashSet<Address>>,
        gas_used: u64,
        target_address: Address,
        bytecode_address: Address,
    }

    // Test constants
    const ADDRESS_A: Address = address!("1000000000000000000000000000000000000001");
    const ADDRESS_B: Address = address!("2000000000000000000000000000000000000002");
    const ADDRESS_C: Address = address!("300000D000000000000000000000000000000003");
    const NON_EMPTY_ADDRESS: Address = address!("400000D000000000000000000000000000000004");
    const TEST_GAS_LIMIT: u64 = 100_000;
    const ZERO6_EMPTY_ACCOUNT_GAS_DELTA: u64 = crate::helpers::PRECOMPILE_EMPTY_ACCOUNT_GAS_COST;

    fn baseline_flags() -> ArcHardforkFlags {
        ArcHardforkFlags::with(&[
            ArcHardfork::Zero3,
            ArcHardfork::Zero4,
            ArcHardfork::Zero5,
            ArcHardfork::Zero6,
        ])
    }

    fn assert_precompile_result(
        precompile_res: Result<Option<InterpreterResult>, String>,
        tc: &NativeCoinAuthorityTest,
        _hardfork_flags: ArcHardforkFlags,
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

                // Skip the gas-used assertion on PrecompileOOG: under revm 38 the
                // precompile-result converter always `spend_all()`s on Halt, so
                // `result.gas.used()` is tautologically the gas_limit.
                if tc.expected_result != InstructionResult::PrecompileOOG {
                    assert_eq!(
                        result.gas.used(),
                        tc.gas_used,
                        "{tc_name}: gas used to match"
                    );
                }
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
        ctx.journal_mut().set_code(
            NON_EMPTY_ADDRESS,
            Bytecode::new_legacy(Bytes::from_static(&[0x60, 0x00, 0x60, 0x00, 0x56])),
        );
        ctx.journal_mut()
            .balance_incr(NON_EMPTY_ADDRESS, mock_initial_supply)
            .expect("Unable to write initial balance for NON_EMPTY_ADDRESS");
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
            // Authorization check is now a constant comparison (no SLOAD), then
            // charges the early-revert penalty.
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
                expected_revert_str: Some(ERR_CANNOT_MINT),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                expected_revert_str: Some(ERR_ZERO_AMOUNT),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 2100 + PRECOMPILE_EARLY_REVERT_GAS_PENALTY, // blocklist check cold SLOAD only
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                expected_revert_str: Some(ERR_OVERFLOW),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 2200 + PRECOMPILE_EARLY_REVERT_GAS_PENALTY, // blocklist cold (2100) + total_supply warm (100)
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            NativeCoinAuthorityTest {
                name: "mint() invalid params errors with Execution Reverted",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall::SELECTOR.into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            NativeCoinAuthorityTest {
                name: "mint() prevents calls if target != precompile address",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall::SELECTOR.into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: ADDRESS_B,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0, // No auth SLOAD
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: ADDRESS_B, // different bytecode address
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,               // No auth SLOAD
                target_address: ADDRESS_B, // different target address
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // Empty recipients also pay the baseline empty-account creation surcharge.
            NativeCoinAuthorityTest {
                name: "mint() success and returns true",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 9556 + ZERO6_EMPTY_ACCOUNT_GAS_DELTA,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // Baseline: NON_EMPTY_ADDRESS is initialized in test setup, so balance_incr()
            // must not charge the empty-account creation surcharge.
            NativeCoinAuthorityTest {
                name: "mint() to non-empty account succeeds without empty account surcharge",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: NON_EMPTY_ADDRESS,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 7056,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // No auth SLOAD, zero-address check precedes blocklist SLOADs
            NativeCoinAuthorityTest {
                name: "mint() to zero address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: Address::ZERO,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_ZERO_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_CANNOT_BURN),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_ZERO_AMOUNT),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 2100 + PRECOMPILE_EARLY_REVERT_GAS_PENALTY, // blocklist cold SLOAD only
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // No auth SLOAD, blocklist cold (2100), warm balance check (100)
            NativeCoinAuthorityTest {
                name: "burn() more than balance reverts with insufficient funds",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: mock_initial_supply + U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_INSUFFICIENT_FUNDS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 2200, // blocklist cold + warm balance check
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            NativeCoinAuthorityTest {
                name: "burn() with invalid params reverts with Execution Reverted",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall::SELECTOR.into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                target_address: ADDRESS_B,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: ADDRESS_B,
            },
            // Baseline burn success uses warm/cold-aware balance and total-supply storage costs.
            NativeCoinAuthorityTest {
                name: "burn() succeeds and returns true",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: ADDRESS_A,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 7056,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // No auth SLOAD, zero-address check precedes blocklist SLOADs
            NativeCoinAuthorityTest {
                name: "burn() from zero address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::burnCall {
                    from: Address::ZERO,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_ZERO_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_CANNOT_TRANSFER),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // No auth SLOAD, from/to blocklist cold (4200), warm balance check (100)
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_INSUFFICIENT_FUNDS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_SLOAD_GAS_COST * 2 + 100, // 2 blocklist cold SLOADs + warm balance check
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                // Baseline: warm-from discount drops success to 14456, use 14455
                // to trigger OOG.
                gas_limit: 14455,
                expected_revert_str: None,
                expected_result: InstructionResult::PrecompileOOG,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            NativeCoinAuthorityTest {
                name: "transfer() with invalid params reverts with Execution Reverted",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall::SELECTOR.into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_EXECUTION_REVERTED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                target_address: ADDRESS_B,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_DELEGATE_CALL_NOT_ALLOWED),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: ADDRESS_B,
            },
            // No auth SLOAD, zero-address check precedes blocklist SLOADs
            NativeCoinAuthorityTest {
                name: "transfer() to zero address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: Address::ZERO,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_ZERO_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            NativeCoinAuthorityTest {
                name: "transfer() from zero address reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: Address::ZERO,
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_ZERO_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            NativeCoinAuthorityTest {
                name: "transfer() with both zero addresses reverts",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: Address::ZERO,
                    to: Address::ZERO,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_ZERO_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: PRECOMPILE_EARLY_REVERT_GAS_PENALTY,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // Empty recipients also pay the baseline empty-account creation surcharge.
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 14456 + ZERO6_EMPTY_ACCOUNT_GAS_DELTA,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // Baseline: NON_EMPTY_ADDRESS is initialized in test setup, so transfer()
            // must not charge the empty-account creation surcharge.
            NativeCoinAuthorityTest {
                name: "transfer() to non-empty account succeeds without empty account surcharge",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::transferCall {
                    from: ADDRESS_A,
                    to: NON_EMPTY_ADDRESS,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 11956,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // No auth SLOAD, from/to blocklist cold (4200), warm balance check (100),
            // balance-decrease SSTORE reset (2900)
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_CLEAR_EMPTY),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 7200, // 2 blocklist cold SLOADs + warm balance check + SSTORE reset
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // Empty recipients also pay the baseline empty-account creation surcharge.
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 14456 + ZERO6_EMPTY_ACCOUNT_GAS_DELTA,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 4200, // 2 blocklist cold SLOADs
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // total_supply warm SLOAD (100) - test setup writes it
            NativeCoinAuthorityTest {
                name: "totalSupply() returns the total supply",
                caller: ADDRESS_A,
                calldata: INativeCoinAuthority::totalSupplyCall::SELECTOR.into(),
                gas_limit: TOTAL_SUPPLY_GAS_COST,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(mock_initial_supply.abi_encode().into()),
                blocklisted_addresses: None,
                gas_used: 100,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            NativeCoinAuthorityTest {
                name: "totalSupply() errors with OOG if insufficient gas",
                caller: ADDRESS_A,
                calldata: INativeCoinAuthority::totalSupplyCall::SELECTOR.into(),
                gas_limit: TOTAL_SUPPLY_GAS_COST - 1, // Not enough gas
                expected_revert_str: None,
                expected_result: InstructionResult::PrecompileOOG,
                return_data: None,
                blocklisted_addresses: None,
                gas_used: 0,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_BLOCKED_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: Some(HashSet::from([ADDRESS_B])),
                gas_used: 100 + PRECOMPILE_EARLY_REVERT_GAS_PENALTY, // blocklist warm (test setup wrote it)
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
            // Blocklist status is cold here; recipient balance and total-supply storage use
            // the same baseline warm/cold-aware accounting as the success case above.
            NativeCoinAuthorityTest {
                name: "mint() to non-blocklisted address succeeds",
                caller: ALLOWED_CALLER_ADDRESS,
                calldata: INativeCoinAuthority::mintCall {
                    to: ADDRESS_B,
                    amount: U256::from(1000),
                }
                .abi_encode()
                .into(),
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: None,
                expected_result: InstructionResult::Return,
                return_data: Some(true.abi_encode().into()),
                blocklisted_addresses: Some(HashSet::from([ADDRESS_C])),
                gas_used: 9556 + ZERO6_EMPTY_ACCOUNT_GAS_DELTA,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_BLOCKED_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: Some(HashSet::from([ADDRESS_B])),
                gas_used: 100 + PRECOMPILE_EARLY_REVERT_GAS_PENALTY, // blocklist warm (test setup wrote it)
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_BLOCKED_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: Some(HashSet::from([ADDRESS_A])),
                gas_used: 100 + PRECOMPILE_EARLY_REVERT_GAS_PENALTY, // from blocklist warm (test setup wrote it)
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
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
                gas_limit: TEST_GAS_LIMIT,
                expected_revert_str: Some(ERR_BLOCKED_ADDRESS),
                expected_result: InstructionResult::Revert,
                return_data: None,
                blocklisted_addresses: Some(HashSet::from([ADDRESS_B])),
                gas_used: 2200 + PRECOMPILE_EARLY_REVERT_GAS_PENALTY, // from blocklist cold (2100) + to blocklist warm (100)
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            },
        ];

        for tc in cases {
            {
                let hardfork_flags = baseline_flags();
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
                    known_bytecode: (B256::ZERO, Bytecode::default()),
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(tc.calldata.clone()),
                    gas_limit: tc.gas_limit,
                    caller: tc.caller,
                    is_static: false,
                    return_memory_offset: 0..0,
                    reservoir: 0,
                };

                let precompile_res = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags);
                assert_precompile_result(precompile_res, tc, hardfork_flags, &tc_name);

                cleanup_blocklist(&mut ctx, &tc.blocklisted_addresses);
            }
        }
    }

    #[test]
    fn mint_uses_constant_authority_regardless_of_flags() {
        let hardfork_flags = ArcHardforkFlags::default();
        let amount = U256::from(100);
        let mut ctx = mock_context(hardfork_flags);
        setup_initial_state(&mut ctx, U256::from(1_000_000_000));

        let inputs = CallInputs {
            scheme: CallScheme::Call,
            target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            known_bytecode: (B256::ZERO, Bytecode::default()),
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
            reservoir: 0,
        };

        let result = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags)
            .expect("call should not error")
            .expect("result should be Some");

        assert_eq!(result.result, InstructionResult::Return);
        assert_eq!(result.output, Bytes::from(true.abi_encode()));

        let logs = ctx.journal().logs();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].address, SYSTEM_ADDRESS);
        let expected_log = Transfer {
            from: Address::ZERO,
            to: ADDRESS_B,
            value: amount,
        }
        .encode_log_data();
        assert_eq!(logs[0].data, expected_log);
    }

    #[test]
    fn mint_side_effects() {
        {
            let hardfork_flags = baseline_flags();
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
                known_bytecode: (B256::ZERO, Bytecode::default()),
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
                reservoir: 0,
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

            assert_eq!(
                logs.len(),
                1,
                "one EIP-7708 Transfer event expected for mint"
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
        }
    }

    #[test]
    fn burn_side_effects() {
        {
            let hardfork_flags = baseline_flags();
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
                known_bytecode: (B256::ZERO, Bytecode::default()),
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
                reservoir: 0,
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

            // A burn is represented as an EIP-7708 transfer to the zero address.
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

            assert_eq!(
                logs.len(),
                1,
                "one EIP-7708 Transfer event expected for burn"
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
        }
    }

    #[test]
    fn transfer_side_effects() {
        {
            let hardfork_flags = baseline_flags();
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
                known_bytecode: (B256::ZERO, Bytecode::default()),
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
                reservoir: 0,
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
                known_bytecode: (B256::ZERO, Bytecode::default()),
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
                reservoir: 0,
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

            let expected_log = Transfer {
                from: ADDRESS_A,
                to: ADDRESS_B,
                value: transfer_amount,
            }
            .encode_log_data();
            assert_eq!(log.address, SYSTEM_ADDRESS);
            assert_eq!(log.data, expected_log);
        }
    }

    /// Self-transfers (from == to) do not emit an EIP-7708 Transfer log.
    #[test]
    fn transfer_self_transfer_no_log() {
        {
            let hardfork_flags = baseline_flags();
            let initial_supply = U256::from(1_000_000_000);
            let transfer_amount = U256::from(1000);

            let mut ctx = mock_context(hardfork_flags);
            setup_initial_state(&mut ctx, initial_supply);

            // Self-transfer: from == to == ADDRESS_A
            let inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                known_bytecode: (B256::ZERO, Bytecode::default()),
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
                reservoir: 0,
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

            assert_eq!(logs.len(), 0, "self-transfer should not emit a log");

            // Self-transfer still executes the full transfer path (balance_decr +
            // balance_incr); only event emission is suppressed. ADDRESS_A is
            // pre-warmed by the test setup, so both account loads hit the warm path.
            let expected_gas = 2100
                + 100
                + 2 * revm_interpreter::gas::WARM_STORAGE_READ_COST
                + 2 * PRECOMPILE_SSTORE_GAS_COST;
            assert_eq!(
                result.gas.used(),
                expected_gas,
                "self-transfer gas should include transfer path but no event"
            );
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

        {
            let hardfork_flags = baseline_flags();
            // State-modifying functions must revert under static call
            for (fn_name, calldata) in state_modifying_calldatas {
                let mut ctx = mock_context(hardfork_flags);
                let inputs = CallInputs {
                    scheme: CallScheme::Call,
                    target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                    bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                    known_bytecode: (B256::ZERO, Bytecode::default()),
                    caller: ALLOWED_CALLER_ADDRESS,
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(calldata.clone()),
                    gas_limit: 100_000,
                    is_static: true,
                    return_memory_offset: 0..0,
                    reservoir: 0,
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
                    known_bytecode: (B256::ZERO, Bytecode::default()),
                    caller: ADDRESS_A,
                    value: CallValue::Transfer(U256::ZERO),
                    input: CallInput::Bytes(
                        INativeCoinAuthority::totalSupplyCall {}.abi_encode().into(),
                    ),
                    gas_limit: 100_000,
                    is_static: true,
                    return_memory_offset: 0..0,
                    reservoir: 0,
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
        let hardfork_flags = baseline_flags();
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
            known_bytecode: (B256::ZERO, Bytecode::default()),
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
            reservoir: 0,
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

    fn total_supply_calldata_with_trailing_bytes(trailing: &[u8]) -> Bytes {
        let capacity = INativeCoinAuthority::totalSupplyCall::SELECTOR
            .len()
            .checked_add(trailing.len())
            .expect("selector plus test trailing bytes length should fit");
        let mut calldata = Vec::with_capacity(capacity);
        calldata.extend_from_slice(&INativeCoinAuthority::totalSupplyCall::SELECTOR);
        calldata.extend_from_slice(trailing);
        calldata.into()
    }

    /// Under Zero6, account helpers (`transfer`, `balance_incr`, `balance_decr`)
    /// charge `WARM_STORAGE_READ_COST` (100) for warm accounts and
    /// `COLD_ACCOUNT_ACCESS_COST` (2600) for cold accounts after the load.
    ///
    /// The target is pre-funded (non-empty) so the Zero6 empty-account
    /// creation surcharge does not apply — the OOG is isolated to the
    /// cold-account access charge.
    ///
    /// This test mints to both a cold and a pre-warmed address at the same
    /// gas budget (`full_cold_gas - cold_delta`). The warm mint succeeds
    /// (account load costs only 100); the cold mint OOGs at the cold-account
    /// charge after the load. Together they prove the OOG is caused
    /// specifically by the cold-account surcharge.
    #[test]
    fn account_load_cold_oog() {
        use revm_interpreter::gas::{COLD_ACCOUNT_ACCESS_COST, WARM_STORAGE_READ_COST};

        let zero6_flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5, ArcHardfork::Zero6]);
        let mock_initial_supply = U256::from(1_000_000_000);

        let cold_target: Address = address!("9999999999999999999999999999999999999999");

        let make_inputs = |target: Address, gas_limit: u64| CallInputs {
            scheme: CallScheme::Call,
            target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
            known_bytecode: (B256::ZERO, Bytecode::default()),
            caller: ALLOWED_CALLER_ADDRESS,
            value: CallValue::Transfer(U256::ZERO),
            input: CallInput::Bytes(
                INativeCoinAuthority::mintCall {
                    to: target,
                    amount: U256::from(1),
                }
                .abi_encode()
                .into(),
            ),
            gas_limit,
            is_static: false,
            return_memory_offset: 0..0,
            reservoir: 0,
        };

        /// Pre-fund cold_target so it is non-empty (avoids Zero6 empty-account
        /// creation surcharge), then commit the tx to clear warm addresses.
        fn prefund_cold_target(ctx: &mut revm::Context, target: Address) {
            ctx.journal_mut().load_account(target).expect("load target");
            ctx.journal_mut()
                .balance_incr(target, U256::from(1))
                .expect("fund target");
            ctx.journal_mut().commit_tx();
        }

        // Observe full gas for a cold, non-empty target mint.
        let mut ctx = mock_context(zero6_flags);
        setup_initial_state(&mut ctx, mock_initial_supply);
        prefund_cold_target(&mut ctx, cold_target);
        let baseline =
            call_native_coin_authority(&mut ctx, &make_inputs(cold_target, 1_000_000), zero6_flags)
                .unwrap()
                .unwrap();
        assert_eq!(
            baseline.result,
            InstructionResult::Return,
            "baseline must succeed"
        );
        let full_cold_gas = baseline.gas.used();

        // Budget that covers warm (100) but not cold (2600) at the account load.
        #[allow(clippy::arithmetic_side_effects)]
        let boundary_gas = full_cold_gas - COLD_ACCOUNT_ACCESS_COST + WARM_STORAGE_READ_COST;

        // Cold target at boundary: OOG at the cold delta charge.
        let mut ctx = mock_context(zero6_flags);
        setup_initial_state(&mut ctx, mock_initial_supply);
        prefund_cold_target(&mut ctx, cold_target);
        let res = call_native_coin_authority(
            &mut ctx,
            &make_inputs(cold_target, boundary_gas),
            zero6_flags,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            res.result,
            InstructionResult::PrecompileOOG,
            "cold account at boundary_gas must OOG"
        );

        // Warm target at the same boundary: succeeds, proving the OOG above is
        // caused specifically by the cold delta.
        let mut ctx = mock_context(zero6_flags);
        setup_initial_state(&mut ctx, mock_initial_supply);
        prefund_cold_target(&mut ctx, cold_target);
        // Pre-warm cold_target by loading it before the precompile call.
        ctx.journal_mut()
            .load_account(cold_target)
            .expect("pre-warm target");
        let res = call_native_coin_authority(
            &mut ctx,
            &make_inputs(cold_target, boundary_gas),
            zero6_flags,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            res.result,
            InstructionResult::Return,
            "warm account at boundary_gas must succeed"
        );
    }

    #[test]
    fn total_supply_accepts_extra_input() {
        let mock_initial_supply = U256::from(1_000_000_000);
        let cases: &[(&str, &[u8])] = &[
            ("empty", &[]),
            ("zero word", &[0u8; 32]),
            ("non-zero partial word", &[0xab, 0xcd, 0x01]),
            ("non-zero unaligned long", &[0x11; 33]),
        ];

        for (case_name, trailing) in cases {
            let hardfork_flags = baseline_flags();
            let mut ctx = mock_context(hardfork_flags);
            setup_initial_state(&mut ctx, mock_initial_supply);

            let inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                bytecode_address: NATIVE_COIN_AUTHORITY_ADDRESS,
                known_bytecode: (B256::ZERO, Bytecode::default()),
                caller: ADDRESS_A,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(total_supply_calldata_with_trailing_bytes(trailing)),
                gas_limit: 100_000,
                is_static: false,
                return_memory_offset: 0..0,
                reservoir: 0,
            };

            let result = call_native_coin_authority(&mut ctx, &inputs, hardfork_flags)
                .expect("call should not error")
                .expect("result should be Some");

            assert_eq!(
                result.result,
                InstructionResult::Return,
                "{case_name} ({hardfork_flags:?}): expected Return with trailing calldata",
            );
            let returned = U256::abi_decode(result.output.as_ref()).expect("decode total supply");
            assert_eq!(
                returned, mock_initial_supply,
                "{case_name} ({hardfork_flags:?}): expected initial supply returned",
            );
        }
    }
}
