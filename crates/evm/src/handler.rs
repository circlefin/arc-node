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

use alloy_primitives::{Address, U256};
use arc_execution_config::hardforks::{ArcHardfork, ArcHardforkFlags};
use arc_execution_config::native_coin_control::{
    compute_is_blocklisted_storage_slot, is_blocklisted_status,
};
use arc_precompiles::helpers::{ERR_BLOCKED_ADDRESS, PRECOMPILE_SLOAD_GAS_COST};
use arc_precompiles::NATIVE_COIN_CONTROL_ADDRESS;
use revm::inspector::{Inspector, InspectorEvmTr, InspectorHandler};
use revm::{
    context::{ContextTr, JournalTr, Transaction},
    context_interface::result::InvalidTransaction,
    context_interface::{result::HaltReason, Block},
    handler::{EthFrame, EvmTr, EvmTrError, FrameTr, Handler, MainnetHandler},
    interpreter::{interpreter::EthInterpreter, InitialAndFloorGas},
    state::EvmState,
};
use revm_primitives::TxKind;

// Handler Implementation
// See: https://github.com/bluealloy/revm/blob/v97/examples/erc20_gas/src/handler.rs#L19
pub struct ArcEvmHandler<EVM, ERROR> {
    mainnet: MainnetHandler<EVM, ERROR, EthFrame<EthInterpreter>>,
    /// Feature flags for Arc hardforks active at the current block.
    hardfork_flags: ArcHardforkFlags,
}

impl<CTX, ERROR> ArcEvmHandler<CTX, ERROR> {
    pub fn new(hardfork_flags: ArcHardforkFlags) -> Self {
        Self {
            mainnet: MainnetHandler::default(),
            hardfork_flags,
        }
    }
}

impl<EVM, ERROR> Handler for ArcEvmHandler<EVM, ERROR>
where
    EVM: EvmTr<
        Context: ContextTr<Journal: JournalTr<State = EvmState>>,
        Frame = EthFrame<EthInterpreter>,
    >,
    ERROR: EvmTrError<EVM>,
{
    type Evm = EVM;
    type Error = ERROR;
    type HaltReason = HaltReason;

    #[inline]
    fn pre_execution(&self, evm: &mut Self::Evm) -> Result<u64, Self::Error> {
        let ctx = evm.ctx();
        let tx = ctx.tx();
        let caller = tx.caller();
        let tx_kind = tx.kind();
        let tx_value = tx.value();

        if !self.can_load_account(evm, NATIVE_COIN_CONTROL_ADDRESS) {
            return self.mainnet.pre_execution(evm);
        }
        self.check_blocklist(evm, caller, &tx_kind, tx_value)?;

        self.mainnet.pre_execution(evm)
    }

    #[inline]
    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        exec_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        // Redirect both base fee and priority fee to the specified beneficiary
        // This overrides the default EIP-1559 behavior which burns the base fee
        let ctx = evm.ctx();
        let beneficiary = ctx.block().beneficiary();
        let basefee = ctx.block().basefee() as u128;
        let effective_gas_price = ctx.tx().effective_gas_price(basefee);
        let gas_used = exec_result.gas().used() as u128;

        // Calculate total fee (base fee + priority fee) instead of just priority fee
        let total_fee_amount = U256::from(effective_gas_price * gas_used);

        // Transfer the total fee to the beneficiary (both base fee and priority fee)
        evm.ctx_mut()
            .journal_mut()
            .balance_incr(beneficiary, total_fee_amount)
            .map_err(From::from)
    }

    /// Calculates initial gas costs including blocklist SLOAD costs for Zero6+.
    ///
    /// For Zero6 hardfork and later:
    /// - Always adds 1 SLOAD cost (2,100 gas) for caller blocklist check
    /// - Adds 1 additional SLOAD cost (2,100 gas) for recipient blocklist check if value > 0
    ///
    /// This ensures gas accounting matches the actual blocklist checks performed in pre_execution.
    #[inline]
    fn validate_initial_tx_gas(
        &self,
        evm: &mut Self::Evm,
    ) -> Result<InitialAndFloorGas, Self::Error> {
        // Get base intrinsic gas from mainnet handler
        let mut init_and_floor_gas = self.mainnet.validate_initial_tx_gas(evm)?;

        // Add blocklist SLOAD costs if Zero6 is active
        if self.hardfork_flags.is_active(ArcHardfork::Zero6) {
            let ctx = evm.ctx_ref();
            let tx = ctx.tx();

            // Always charge for caller blocklist check
            let mut extra_gas = PRECOMPILE_SLOAD_GAS_COST;

            // Charge for recipient blocklist check if value > 0
            // This applies to both Call and Create transactions with value
            if !tx.value().is_zero() {
                extra_gas += PRECOMPILE_SLOAD_GAS_COST;
            }

            // Add extra gas to initial gas
            init_and_floor_gas.initial_gas = init_and_floor_gas
                .initial_gas
                .checked_add(extra_gas)
                .ok_or(InvalidTransaction::CallGasCostMoreThanGasLimit {
                    gas_limit: tx.gas_limit(),
                    initial_gas: u64::MAX,
                })?;

            // Verify total doesn't exceed gas limit
            if init_and_floor_gas.initial_gas > tx.gas_limit() {
                return Err(InvalidTransaction::CallGasCostMoreThanGasLimit {
                    gas_limit: tx.gas_limit(),
                    initial_gas: init_and_floor_gas.initial_gas,
                }
                .into());
            }
        }

        Ok(init_and_floor_gas)
    }
}

// Implementing InspectorHandler for ArcEvmHandler.
impl<EVM, ERROR> InspectorHandler for ArcEvmHandler<EVM, ERROR>
where
    EVM: InspectorEvmTr<
        Context: ContextTr<Journal: JournalTr<State = EvmState>>,
        Frame = EthFrame<EthInterpreter>,
        Inspector: Inspector<<<Self as Handler>::Evm as EvmTr>::Context, EthInterpreter>,
    >,
    ERROR: EvmTrError<EVM>,
{
    type IT = EthInterpreter;
}

impl<EVM, ERROR> ArcEvmHandler<EVM, ERROR>
where
    EVM: EvmTr<
        Context: ContextTr<Journal: JournalTr<State = EvmState>>,
        Frame = EthFrame<EthInterpreter>,
    >,
    ERROR: EvmTrError<EVM>,
{
    fn can_load_account(&self, evm: &mut EVM, address: Address) -> bool {
        evm.ctx_mut().journal_mut().load_account(address).is_ok()
    }

    fn check_blocklist(
        &self,
        evm: &mut EVM,
        caller: Address,
        tx_kind: &TxKind,
        tx_value: U256,
    ) -> Result<(), ERROR> {
        let (caller_blocklisted, _) = self.is_address_blocklisted(evm, caller)?;
        if caller_blocklisted {
            return Err(InvalidTransaction::Str(ERR_BLOCKED_ADDRESS.into()).into());
        }
        if let TxKind::Call(to_address) = tx_kind {
            if !tx_value.is_zero() {
                let (to_blocklisted, _) = self.is_address_blocklisted(evm, *to_address)?;
                if to_blocklisted {
                    return Err(InvalidTransaction::Str(ERR_BLOCKED_ADDRESS.into()).into());
                }
            }
        }
        Ok(())
    }

    /// Checks if an address is blocklisted by reading from the native coin control precompile storage.
    ///
    /// Returns `(is_blocklisted, is_cold)`. The `is_cold` flag is available for gas accounting
    /// but is not used at depth 0 (where `validate_initial_tx_gas` charges fixed cold SLOAD costs).
    fn is_address_blocklisted(
        &self,
        evm: &mut EVM,
        address: Address,
    ) -> Result<(bool, bool), ERROR> {
        let storage_slot = compute_is_blocklisted_storage_slot(address).into();
        let journal = evm.ctx_mut().journal_mut();

        let state_load = journal.sload(NATIVE_COIN_CONTROL_ADDRESS, storage_slot)?;
        Ok((is_blocklisted_status(state_load.data), state_load.is_cold))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, U256};
    use arc_precompiles::NATIVE_COIN_CONTROL_ADDRESS;
    use reth_ethereum::evm::revm::db::{CacheDB, EmptyDB};
    use revm::{
        context::Context,
        context_interface::result::{EVMError, InvalidTransaction},
        database::EmptyDBTyped,
        handler::FrameResult,
        interpreter::{CallOutcome, Gas, InstructionResult, InterpreterResult},
        MainBuilder, MainContext,
    };
    use std::convert::Infallible;

    #[test]
    fn test_reward_beneficiary_basic_functionality() {
        let beneficiary = address!("3000000000000000000000000000000000000003");
        let caller = address!("4000000000000000000000000000000000000004");
        let gas_price = 10u128; // 10 wei per gas
        let gas_used = 21000u64; // standard transaction gas

        let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
        let mut evm = Context::mainnet().with_db(db).build_mainnet();

        // Set up the block environment with our beneficiary
        evm.block.beneficiary = beneficiary;
        evm.block.basefee = 7; // 7 wei base fee

        // Set up transaction environment
        evm.tx.caller = caller;
        evm.tx.gas_price = gas_price;
        evm.tx.gas_priority_fee = Some(3); // priority fee = gas_price - basefee = 10 - 7 = 3

        // Create a call outcome with gas usage
        let interpreter_result = InterpreterResult::new(
            InstructionResult::Return,
            alloy_primitives::Bytes::new(),
            Gas::new_spent(gas_used),
        );
        let call_outcome = CallOutcome::new(interpreter_result, 0..0);
        let mut exec_result = FrameResult::Call(call_outcome);

        // Get initial beneficiary balance
        let initial_balance = evm
            .journaled_state
            .load_account(beneficiary)
            .unwrap()
            .info
            .balance;

        // Test the reward_beneficiary function
        let handler: ArcEvmHandler<_, EVMError<Infallible>> =
            ArcEvmHandler::new(ArcHardforkFlags::default());
        let result = handler.reward_beneficiary(&mut evm, &mut exec_result);

        // Assert success
        assert!(result.is_ok(), "reward_beneficiary should succeed");

        // Calculate expected fee (total fee = effective_gas_price * gas_used)
        // effective_gas_price = gas_price = 10 wei
        let expected_fee = U256::from(gas_price * gas_used as u128);

        // Check that beneficiary balance increased by the expected fee amount
        let final_balance = evm
            .journaled_state
            .load_account(beneficiary)
            .unwrap()
            .info
            .balance;
        let balance_increase = final_balance - initial_balance;

        assert_eq!(
            balance_increase, expected_fee,
            "Beneficiary should receive the full transaction fee (base fee + priority fee)"
        );
    }

    #[test]
    fn test_reward_beneficiary_eip1559_full_fee_redirect() {
        let beneficiary = address!("5000000000000000000000000000000000000005");
        let caller = address!("6000000000000000000000000000000000000006");
        let base_fee = 50u64;
        let max_fee_per_gas = 100u128;
        let max_priority_fee_per_gas = 20u128;
        let gas_used = 30000u64;

        let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
        let mut evm = Context::mainnet().with_db(db).build_mainnet();

        // Set up EIP-1559 transaction
        evm.block.beneficiary = beneficiary;
        evm.block.basefee = base_fee;
        evm.tx.caller = caller;
        evm.tx.gas_price = max_fee_per_gas;
        evm.tx.gas_priority_fee = Some(max_priority_fee_per_gas);

        // Create call outcome
        let interpreter_result = InterpreterResult::new(
            InstructionResult::Return,
            alloy_primitives::Bytes::new(),
            Gas::new_spent(gas_used),
        );
        let call_outcome = CallOutcome::new(interpreter_result, 0..0);
        let mut exec_result = FrameResult::Call(call_outcome);

        let initial_balance = evm
            .journaled_state
            .load_account(beneficiary)
            .unwrap()
            .info
            .balance;

        // Test reward_beneficiary
        let handler: ArcEvmHandler<_, EVMError<Infallible>> =
            ArcEvmHandler::new(ArcHardforkFlags::default());
        let result = handler.reward_beneficiary(&mut evm, &mut exec_result);

        assert!(result.is_ok(), "reward_beneficiary should succeed");

        // The effective gas price is what revm actually calculates
        // In this test setup, it appears to be using gas_price (100) directly
        let expected_total_fee = U256::from(max_fee_per_gas * gas_used as u128);

        let final_balance = evm
            .journaled_state
            .load_account(beneficiary)
            .unwrap()
            .info
            .balance;
        let balance_increase = final_balance - initial_balance;

        assert_eq!(
            balance_increase, expected_total_fee,
            "Beneficiary should receive the full effective gas price (including base fee), not just priority fee"
        );

        // Verify this is MORE than just the priority fee
        let priority_fee_only = U256::from(max_priority_fee_per_gas * gas_used as u128);
        assert!(
            balance_increase > priority_fee_only,
            "Fee redirect should include base fee, not just priority fee"
        );
    }

    #[test]
    fn test_reward_beneficiary_zero_gas_used() {
        let beneficiary = address!("7000000000000000000000000000000000000007");
        let caller = address!("8000000000000000000000000000000000000008");

        let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
        let mut evm = Context::mainnet().with_db(db).build_mainnet();

        evm.block.beneficiary = beneficiary;
        evm.block.basefee = 10;
        evm.tx.caller = caller;
        evm.tx.gas_price = 20;

        // Create call outcome with zero gas used
        let interpreter_result = InterpreterResult::new(
            InstructionResult::Return,
            alloy_primitives::Bytes::new(),
            Gas::new_spent(0), // Zero gas used
        );
        let call_outcome = CallOutcome::new(interpreter_result, 0..0);
        let mut exec_result = FrameResult::Call(call_outcome);

        let initial_balance = evm
            .journaled_state
            .load_account(beneficiary)
            .unwrap()
            .info
            .balance;

        let handler: ArcEvmHandler<_, EVMError<Infallible>> =
            ArcEvmHandler::new(ArcHardforkFlags::default());
        let result = handler.reward_beneficiary(&mut evm, &mut exec_result);

        assert!(
            result.is_ok(),
            "reward_beneficiary should succeed even with zero gas"
        );

        let final_balance = evm
            .journaled_state
            .load_account(beneficiary)
            .unwrap()
            .info
            .balance;
        let balance_increase = final_balance - initial_balance;

        assert_eq!(
            balance_increase,
            U256::ZERO,
            "No fee should be rewarded when zero gas is used"
        );
    }

    #[test]
    fn test_reward_beneficiary_different_beneficiaries() {
        let beneficiary1 = address!("9000000000000000000000000000000000000009");
        let beneficiary2 = address!("A000000000000000000000000000000000000000");
        let caller = address!("B000000000000000000000000000000000000000");
        let gas_used = 25000u64;
        let gas_price = 15u128;

        let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
        let mut evm = Context::mainnet().with_db(db).build_mainnet();

        evm.tx.caller = caller;
        evm.tx.gas_price = gas_price;
        evm.block.basefee = 5;

        // Create call outcome
        let interpreter_result = InterpreterResult::new(
            InstructionResult::Return,
            alloy_primitives::Bytes::new(),
            Gas::new_spent(gas_used),
        );
        let call_outcome = CallOutcome::new(interpreter_result, 0..0);
        let mut exec_result = FrameResult::Call(call_outcome);

        let handler: ArcEvmHandler<_, EVMError<Infallible>> =
            ArcEvmHandler::new(ArcHardforkFlags::default());

        // Test with first beneficiary
        evm.block.beneficiary = beneficiary1;
        let initial_balance1 = evm
            .journaled_state
            .load_account(beneficiary1)
            .unwrap()
            .info
            .balance;
        let initial_balance2 = evm
            .journaled_state
            .load_account(beneficiary2)
            .unwrap()
            .info
            .balance;

        let result = handler.reward_beneficiary(&mut evm, &mut exec_result);
        assert!(result.is_ok());

        let balance1_after = evm
            .journaled_state
            .load_account(beneficiary1)
            .unwrap()
            .info
            .balance;
        let balance2_after = evm
            .journaled_state
            .load_account(beneficiary2)
            .unwrap()
            .info
            .balance;

        let expected_fee = U256::from(gas_price * gas_used as u128);

        // Beneficiary1 should receive the fee
        assert_eq!(
            balance1_after - initial_balance1,
            expected_fee,
            "First beneficiary should receive the fee"
        );

        // Beneficiary2 should not receive anything
        assert_eq!(
            balance2_after, initial_balance2,
            "Second beneficiary should not receive any fee"
        );

        // Now test with second beneficiary (simulate next transaction)
        evm.block.beneficiary = beneficiary2;
        let result = handler.reward_beneficiary(&mut evm, &mut exec_result);
        assert!(result.is_ok());

        let balance2_final = evm
            .journaled_state
            .load_account(beneficiary2)
            .unwrap()
            .info
            .balance;

        // Beneficiary2 should now receive the fee
        assert_eq!(
            balance2_final - balance2_after,
            expected_fee,
            "Second beneficiary should receive the fee when set as beneficiary"
        );
    }

    #[derive(Debug)]
    struct BlocklistTestCase {
        name: &'static str,
        caller: Address,
        recipient: Option<Address>, // None for Create transactions
        value: U256,
        caller_blocklisted: bool,
        recipient_blocklisted: bool,
        expected_result: BlocklistExpectedResult,
    }

    #[derive(Debug, PartialEq)]
    enum BlocklistExpectedResult {
        Error,   // Should return "address is blocklisted" error
        Success, // Should return Ok(gas_value)
    }

    #[test]
    fn test_pre_execution_blocklist_scenarios() {
        let test_cases = vec![
            BlocklistTestCase {
                name: "caller_blocked",
                caller: address!("C000000000000000000000000000000000000001"),
                recipient: Some(address!("D000000000000000000000000000000000000002")),
                value: U256::from(1000),
                caller_blocklisted: true,
                recipient_blocklisted: false,
                expected_result: BlocklistExpectedResult::Error,
            },
            BlocklistTestCase {
                name: "recipient_blocked_with_value",
                caller: address!("E000000000000000000000000000000000000001"),
                recipient: Some(address!("F000000000000000000000000000000000000002")),
                value: U256::from(1000),
                caller_blocklisted: false,
                recipient_blocklisted: true,
                expected_result: BlocklistExpectedResult::Error,
            },
            BlocklistTestCase {
                name: "recipient_blocked_zero_value_allowed",
                caller: address!("a000000000000000000000000000000000000001"),
                recipient: Some(address!("b000000000000000000000000000000000000002")),
                value: U256::ZERO,
                caller_blocklisted: false,
                recipient_blocklisted: true,
                expected_result: BlocklistExpectedResult::Success,
            },
            BlocklistTestCase {
                name: "create_transaction_caller_blocked",
                caller: address!("c000000000000000000000000000000000000001"),
                recipient: None, // Create transaction
                value: U256::from(1000),
                caller_blocklisted: true,
                recipient_blocklisted: false,
                expected_result: BlocklistExpectedResult::Error,
            },
            BlocklistTestCase {
                name: "unblocklisted_addresses",
                caller: address!("d000000000000000000000000000000000000001"),
                recipient: Some(address!("e000000000000000000000000000000000000002")),
                value: U256::from(1000),
                caller_blocklisted: false,
                recipient_blocklisted: false,
                expected_result: BlocklistExpectedResult::Success,
            },
            BlocklistTestCase {
                name: "storage_default_behavior",
                caller: address!("f000000000000000000000000000000000000001"),
                recipient: Some(address!("1000000000000000000000000000000000000002")),
                value: U256::from(1000),
                caller_blocklisted: false,
                recipient_blocklisted: false,
                expected_result: BlocklistExpectedResult::Success,
            },
        ];

        for test_case in test_cases {
            println!("Running test case: {}", test_case.name);

            // Set up EVM context
            let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
            let mut evm = Context::mainnet().with_db(db).build_mainnet();

            // Configure transaction
            evm.tx.caller = test_case.caller;
            evm.tx.kind = match test_case.recipient {
                Some(addr) => TxKind::Call(addr),
                None => TxKind::Create,
            };
            evm.tx.value = test_case.value;
            evm.tx.gas_limit = 21000u64;
            evm.tx.gas_price = 1;

            // Set up caller balance
            evm.journaled_state.load_account(test_case.caller).unwrap();
            let initial_caller_balance = U256::from(100_000);
            evm.journaled_state
                .balance_incr(test_case.caller, initial_caller_balance)
                .unwrap();

            // Load native coin control account
            evm.journaled_state
                .load_account(NATIVE_COIN_CONTROL_ADDRESS)
                .unwrap();

            // Set caller blocklist status
            if test_case.caller_blocklisted {
                let storage_slot = compute_is_blocklisted_storage_slot(test_case.caller).into();
                evm.journaled_state
                    .sstore(NATIVE_COIN_CONTROL_ADDRESS, storage_slot, U256::from(1))
                    .unwrap();
            }

            // Set recipient blocklist status
            if let Some(recipient_addr) = test_case.recipient {
                if test_case.recipient_blocklisted {
                    let storage_slot = compute_is_blocklisted_storage_slot(recipient_addr).into();
                    evm.journaled_state
                        .sstore(NATIVE_COIN_CONTROL_ADDRESS, storage_slot, U256::from(1))
                        .unwrap();
                }
            }

            let handler: ArcEvmHandler<_, EVMError<Infallible>> =
                ArcEvmHandler::new(ArcHardforkFlags::default());
            let result = handler.pre_execution(&mut evm);

            // Check caller balance after pre_execution to verify correct gas deduction behavior
            let final_caller_balance = evm
                .journaled_state
                .load_account(test_case.caller)
                .unwrap()
                .info
                .balance;

            match test_case.expected_result {
                BlocklistExpectedResult::Error => {
                    // Verify that the error is an InvalidTransaction
                    let err = result.expect_err(&format!(
                        "Test case '{}' should return an error",
                        test_case.name
                    ));
                    match err {
                        EVMError::Transaction(InvalidTransaction::Str(msg)) => {
                            assert!(
                                msg.contains(ERR_BLOCKED_ADDRESS),
                                "Test case '{}' error should contain blocklisted message, got: {}",
                                test_case.name,
                                msg
                            );
                        }
                        _ => {
                            panic!(
                                "Test case '{}' should return EVMError::Transaction(InvalidTransaction::Str), got: {:?}",
                                test_case.name,
                                err
                            );
                        }
                    }

                    // Verify that no gas fees were deducted when pre_execution fails
                    assert_eq!(
                        final_caller_balance, initial_caller_balance,
                        "Test case '{}': Caller balance should not change when pre_execution fails (no gas should be deducted)",
                        test_case.name
                    );
                }
                BlocklistExpectedResult::Success => {
                    assert!(
                        result.is_ok(),
                        "Test case '{}' should return success",
                        test_case.name
                    );
                    // Note: In test environment, mainnet.pre_execution returns 0
                    assert_eq!(
                        result.unwrap(),
                        0,
                        "Test case '{}' should return expected gas value",
                        test_case.name
                    );

                    // For successful pre_execution, mainnet.pre_execution is called which deducts gas
                    let expected_gas_deduction = evm.tx.gas_limit as u128 * evm.tx.gas_price;
                    let expected_final_balance =
                        initial_caller_balance.saturating_sub(U256::from(expected_gas_deduction));
                    assert_eq!(
                        final_caller_balance, expected_final_balance,
                        "Test case '{}': Caller balance should be reduced by gas cost ({}) when pre_execution succeeds and calls mainnet handler",
                        test_case.name, expected_gas_deduction
                    );
                }
            }
        }
    }

    #[test]
    fn test_validate_initial_tx_gas_pre_zero6_no_extra_gas() {
        // Pre-Zero6: No extra gas should be charged for blocklist checks
        let caller = address!("1000000000000000000000000000000000000001");
        let recipient = address!("2000000000000000000000000000000000000002");

        let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
        let mut evm = Context::mainnet().with_db(db).build_mainnet();

        // Set up transaction with value
        evm.tx.caller = caller;
        evm.tx.kind = TxKind::Call(recipient);
        evm.tx.value = U256::from(1000);
        evm.tx.gas_limit = 100_000u64;
        evm.tx.gas_price = 1;

        // Pre-Zero6 handler (Zero5 active but Zero6 not yet active)
        let handler: ArcEvmHandler<_, EVMError<Infallible>> =
            ArcEvmHandler::new(ArcHardforkFlags::with(&[ArcHardfork::Zero5]));
        let result = handler.validate_initial_tx_gas(&mut evm);

        assert!(result.is_ok(), "validate_initial_tx_gas should succeed");
        let init_gas = result.unwrap();

        // Base intrinsic gas for a simple call is 21000
        // Pre-Zero6: no extra SLOAD costs added
        assert_eq!(
            init_gas.initial_gas, 21000,
            "Pre-Zero6: Should use standard intrinsic gas without blocklist SLOAD costs"
        );
    }

    #[test]
    fn test_validate_initial_tx_gas_zero6_value_transfer() {
        // Zero6: Value transfer should charge 2 SLOADs (caller + recipient)
        let caller = address!("3000000000000000000000000000000000000003");
        let recipient = address!("4000000000000000000000000000000000000004");

        let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
        let mut evm = Context::mainnet().with_db(db).build_mainnet();

        // Set up transaction with value
        evm.tx.caller = caller;
        evm.tx.kind = TxKind::Call(recipient);
        evm.tx.value = U256::from(1000);
        evm.tx.gas_limit = 100_000u64;
        evm.tx.gas_price = 1;

        // Zero6 active handler
        let handler: ArcEvmHandler<_, EVMError<Infallible>> =
            ArcEvmHandler::new(ArcHardforkFlags::with(&[ArcHardfork::Zero6]));
        let result = handler.validate_initial_tx_gas(&mut evm);

        assert!(result.is_ok(), "validate_initial_tx_gas should succeed");
        let init_gas = result.unwrap();

        // Base intrinsic gas (21000) + 2 SLOADs (2 * 2100 = 4200) = 25200
        let expected_gas = 21000 + 2 * PRECOMPILE_SLOAD_GAS_COST;
        assert_eq!(
            init_gas.initial_gas, expected_gas,
            "Zero6 value transfer: Should add 2 SLOAD costs for blocklist checks"
        );
    }

    #[test]
    fn test_validate_initial_tx_gas_zero6_zero_value_call() {
        // Zero6: Zero-value call should charge 1 SLOAD (caller only)
        let caller = address!("5000000000000000000000000000000000000005");
        let recipient = address!("6000000000000000000000000000000000000006");

        let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
        let mut evm = Context::mainnet().with_db(db).build_mainnet();

        // Set up transaction without value
        evm.tx.caller = caller;
        evm.tx.kind = TxKind::Call(recipient);
        evm.tx.value = U256::ZERO;
        evm.tx.gas_limit = 100_000u64;
        evm.tx.gas_price = 1;

        // Zero6 active handler
        let handler: ArcEvmHandler<_, EVMError<Infallible>> =
            ArcEvmHandler::new(ArcHardforkFlags::with(&[ArcHardfork::Zero6]));
        let result = handler.validate_initial_tx_gas(&mut evm);

        assert!(result.is_ok(), "validate_initial_tx_gas should succeed");
        let init_gas = result.unwrap();

        // Base intrinsic gas (21000) + 1 SLOAD (2100) = 23100
        let expected_gas = 21000 + PRECOMPILE_SLOAD_GAS_COST;
        assert_eq!(
            init_gas.initial_gas, expected_gas,
            "Zero6 zero-value call: Should add 1 SLOAD cost for caller blocklist check only"
        );
    }

    #[test]
    fn test_validate_initial_tx_gas_zero6_create_with_value() {
        // Zero6: CREATE with value should charge 2 SLOADs (caller + recipient)
        let caller = address!("7000000000000000000000000000000000000007");

        let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
        let mut evm = Context::mainnet().with_db(db).build_mainnet();

        // Set up CREATE transaction with value
        evm.tx.caller = caller;
        evm.tx.kind = TxKind::Create;
        evm.tx.value = U256::from(1000);
        evm.tx.gas_limit = 100_000u64;
        evm.tx.gas_price = 1;

        // Zero6 active handler
        let handler: ArcEvmHandler<_, EVMError<Infallible>> =
            ArcEvmHandler::new(ArcHardforkFlags::with(&[ArcHardfork::Zero6]));
        let result = handler.validate_initial_tx_gas(&mut evm);

        assert!(result.is_ok(), "validate_initial_tx_gas should succeed");
        let init_gas = result.unwrap();

        // Base intrinsic gas for CREATE (53000) + 2 SLOADs (4200) = 57200
        let expected_gas = 53000 + 2 * PRECOMPILE_SLOAD_GAS_COST;
        assert_eq!(
            init_gas.initial_gas, expected_gas,
            "Zero6 CREATE with value: Should add 2 SLOAD costs for blocklist checks"
        );
    }

    #[test]
    fn test_validate_initial_tx_gas_zero6_gas_limit_exceeded() {
        // Zero6: Should fail if gas limit is too low for blocklist checks
        let caller = address!("8000000000000000000000000000000000000008");
        let recipient = address!("9000000000000000000000000000000000000009");

        let db: CacheDB<EmptyDBTyped<Infallible>> = CacheDB::new(EmptyDB::default());
        let mut evm = Context::mainnet().with_db(db).build_mainnet();

        // Set up transaction with value but gas limit just below required
        // Base (21000) + 2 SLOADs (4200) = 25200 required
        evm.tx.caller = caller;
        evm.tx.kind = TxKind::Call(recipient);
        evm.tx.value = U256::from(1000);
        evm.tx.gas_limit = 25199u64; // Just below 25200
        evm.tx.gas_price = 1;

        // Zero6 active handler
        let handler: ArcEvmHandler<_, EVMError<Infallible>> =
            ArcEvmHandler::new(ArcHardforkFlags::with(&[ArcHardfork::Zero6]));
        let result = handler.validate_initial_tx_gas(&mut evm);

        assert!(
            result.is_err(),
            "validate_initial_tx_gas should fail with insufficient gas"
        );
        match result.unwrap_err() {
            EVMError::Transaction(InvalidTransaction::CallGasCostMoreThanGasLimit { .. }) => {
                // Expected error
            }
            err => panic!("Expected CallGasCostMoreThanGasLimit error, got: {:?}", err),
        }
    }
}
