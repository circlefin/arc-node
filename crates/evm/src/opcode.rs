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

use crate::log::create_eip7708_transfer_log;
use alloy_evm::eth::EthEvmContext;
use arc_execution_config::native_coin_control::{
    compute_is_blocklisted_storage_slot, is_blocklisted_status,
};
use arc_precompiles::helpers::{
    self, ERR_BLOCKED_ADDRESS, ERR_SELFDESTRUCTED_BALANCE_INCREASED, ERR_ZERO_ADDRESS,
};
use arc_precompiles::native_coin_control;
use revm_context_interface::{
    journaled_state::{account::JournaledAccountTr, JournalLoadError},
    ContextTr, JournalTr,
};

use reth_ethereum::evm::primitives::Database;
use reth_evm::revm::{
    context_interface::{host::LoadError, Host},
    interpreter::{
        gas,
        instructions::utility::IntoAddress,
        interpreter_action::InterpreterAction,
        interpreter_types::{InputsTr, RuntimeFlag, StackTr},
        popn, require_non_staticcall, InstructionContext, InstructionResult, InterpreterTypes,
    },
    primitives::hardfork::SpecId,
};
use revm::interpreter::interpreter_types::LoopControl;
use revm_interpreter::StateLoad;

// Overridden SELFDESTRUCT that applies Arc Network-specific functionality
#[derive(Clone, Copy, Debug)]
enum BlocklistReadPolicy {
    FailOpen,
    FailClosed,
}

#[derive(Clone, Copy, Debug)]
enum TargetWarmthPolicy {
    AccessListOnly,
    Transaction,
}

// Forked from: https://github.com/bluealloy/revm/blob/v97/crates/interpreter/src/instructions/host.rs#L387,
// with the following modifications:
//  - Add event emissions for non-zero native value transfers
//  - Add blocklist checks on host address and target
//  - Disallow selfdestruct if target == addr, and amount is non-zero
fn arc_network_selfdestruct_impl<WIRE: InterpreterTypes, DB: Database>(
    mut context: InstructionContext<'_, EthEvmContext<DB>, WIRE>,
    blocklist_read_policy: BlocklistReadPolicy,
    target_warmth_policy: TargetWarmthPolicy,
) {
    require_non_staticcall!(context.interpreter);
    popn!([target], context.interpreter);
    let target = target.into_address();
    let spec = context.interpreter.runtime_flag.spec_id();
    let cold_load_gas = context.host.gas_params().selfdestruct_cold_cost();
    let skip_cold_load = context.interpreter.gas.remaining() < cold_load_gas;

    // MODIFIED CODE
    let addr = context.interpreter.input.target_address();
    let addr_balance = context.host.balance(addr);
    let is_cold = match addr_balance.clone() {
        Some(balance) if !balance.is_zero() => {
            if target == alloy_primitives::Address::ZERO {
                context
                    .interpreter
                    .bytecode
                    .set_action(InterpreterAction::new_return(
                        InstructionResult::Revert,
                        helpers::revert_message_to_bytes(ERR_ZERO_ADDRESS),
                        context.interpreter.gas,
                    ));
                return;
            }

            // Checks the source and target account is valid or not.
            let Ok(is_target_cold) = check_selfdestruct_accounts(
                &mut context,
                addr,
                target,
                skip_cold_load,
                blocklist_read_policy,
                target_warmth_policy,
            ) else {
                // The next action is set in the check_selfdestruct_accounts.
                return;
            };

            is_target_cold
        }
        None => {
            context
                .interpreter
                .halt(InstructionResult::FatalExternalError);
            return;
        }
        _ => None,
    };

    let res = match context
        .host
        .selfdestruct(
            context.interpreter.input.target_address(),
            target,
            skip_cold_load,
        )
        .map(|res| StateLoad {
            data: res.data,
            is_cold: is_cold.unwrap_or(res.is_cold),
        }) {
        Ok(res) => res,
        Err(LoadError::ColdLoadSkipped) => return context.interpreter.halt_oog(),
        Err(LoadError::DBError) => return context.interpreter.halt_fatal(),
    };

    // Emit the transfer log after host.selfdestruct() succeeds, matching REVM's ordering
    // where the log is emitted after the balance is zeroed and transferred in the journal.
    // The balance was captured before selfdestruct zeroed it.
    if let Some(balance) = addr_balance {
        if !balance.is_zero() {
            context
                .host
                .log(create_eip7708_transfer_log(addr, target, balance.data));
        }
    }
    // END MODIFIED CODE

    // EIP-161: State trie clearing (invariant-preserving alternative)
    let should_charge_topup = if spec.is_enabled_in(SpecId::SPURIOUS_DRAGON) {
        res.had_value && !res.target_exists
    } else {
        !res.target_exists
    };

    gas!(
        context.interpreter,
        context
            .host
            .gas_params()
            .selfdestruct_cost(should_charge_topup, res.is_cold)
    );

    if !res.previously_destroyed {
        context
            .interpreter
            .gas
            .record_refund(context.host.gas_params().selfdestruct_refund());
    }

    context.interpreter.halt(InstructionResult::SelfDestruct);
}

/// Pre-Zero7 SELFDESTRUCT handler with fail-open blocklist reads.
pub(crate) fn arc_network_selfdestruct<WIRE: InterpreterTypes, DB: Database>(
    context: InstructionContext<'_, EthEvmContext<DB>, WIRE>,
) {
    arc_network_selfdestruct_impl(
        context,
        BlocklistReadPolicy::FailOpen,
        TargetWarmthPolicy::AccessListOnly,
    );
}

/// Zero7+ SELFDESTRUCT handler with fail-closed blocklist reads.
pub(crate) fn arc_network_selfdestruct_zero7<WIRE: InterpreterTypes, DB: Database>(
    context: InstructionContext<'_, EthEvmContext<DB>, WIRE>,
) {
    arc_network_selfdestruct_impl(
        context,
        BlocklistReadPolicy::FailClosed,
        TargetWarmthPolicy::AccessListOnly,
    );
}

/// Zero8+ SELFDESTRUCT handler with transaction-warm target checks.
pub(crate) fn arc_network_selfdestruct_zero8<WIRE: InterpreterTypes, DB: Database>(
    context: InstructionContext<'_, EthEvmContext<DB>, WIRE>,
) {
    arc_network_selfdestruct_impl(
        context,
        BlocklistReadPolicy::FailClosed,
        TargetWarmthPolicy::Transaction,
    );
}

/// Checks whether a given account is currently on the blocklist
fn is_blocklisted<WIRE: InterpreterTypes, H: Host + ?Sized>(
    context: &mut InstructionContext<'_, H, WIRE>,
    account: alloy_primitives::Address,
    blocklist_read_policy: BlocklistReadPolicy,
) -> Result<bool, LoadError> {
    let slot = compute_is_blocklisted_storage_slot(account).into();
    match blocklist_read_policy {
        BlocklistReadPolicy::FailOpen => Ok(context
            .host
            .sload(native_coin_control::NATIVE_COIN_CONTROL_ADDRESS, slot)
            .is_some_and(|state_load| is_blocklisted_status(state_load.data))),
        BlocklistReadPolicy::FailClosed => {
            let state_load = match context.host.sload_skip_cold_load(
                native_coin_control::NATIVE_COIN_CONTROL_ADDRESS,
                slot,
                false,
            ) {
                Ok(state_load) => state_load,
                Err(LoadError::ColdLoadSkipped) => {
                    let native_coin_control_was_cold = {
                        let native_coin_control_load =
                            context.host.load_account_info_skip_cold_load(
                                native_coin_control::NATIVE_COIN_CONTROL_ADDRESS,
                                false,
                                false,
                            )?;
                        native_coin_control_load.is_cold
                    };
                    debug_assert!(
                        !native_coin_control_was_cold,
                        "NativeCoinControl should be preloaded before SELFDESTRUCT blocklist reads"
                    );
                    context.host.sload_skip_cold_load(
                        native_coin_control::NATIVE_COIN_CONTROL_ADDRESS,
                        slot,
                        false,
                    )?
                }
                Err(LoadError::DBError) => return Err(LoadError::DBError),
            };

            Ok(is_blocklisted_status(state_load.data))
        }
    }
}

/// Checks the source and target addresses are valid or not, and return target is cold or warm if the target account is loaded.
///
/// Checking rules
/// - source and target is not the same
/// - source and target are not blocklisted.
/// - target is not self-destructed.
///
/// - returns Ok(None) pass the check, and we did not load the target account.
/// - returns Ok(bool) pass the check, return `true` if the account is cold, `false` if it is warm.
/// - returns Err(()) if the source or target account is invalid. The error should update in the context.interpreter.
/// - returns Err(()) if a fail-closed blocklist load fails; the interpreter is halted fatally.
fn check_selfdestruct_accounts<WIRE: InterpreterTypes, DB: Database>(
    context: &mut InstructionContext<'_, EthEvmContext<DB>, WIRE>,
    source: alloy_primitives::Address,
    target: alloy_primitives::Address,
    skip_cold_load: bool,
    blocklist_read_policy: BlocklistReadPolicy,
    target_warmth_policy: TargetWarmthPolicy,
) -> Result<Option<bool>, ()> {
    // Disallow selfdestruct if target == source
    if source == target {
        context.interpreter.halt(InstructionResult::Revert);
        return Err(());
    }

    let target_blocklisted = match is_blocklisted(context, target, blocklist_read_policy) {
        Ok(is_blocklisted) => is_blocklisted,
        Err(err) => {
            tracing::error!(
                address = %target,
                err = ?err,
                "blocklist read failed for selfdestruct target"
            );
            context.interpreter.halt_fatal();
            return Err(());
        }
    };
    if target_blocklisted {
        context
            .interpreter
            .bytecode
            .set_action(InterpreterAction::new_return(
                InstructionResult::Revert,
                helpers::revert_message_to_bytes(ERR_BLOCKED_ADDRESS),
                context.interpreter.gas,
            ));
        return Err(());
    }

    let source_blocklisted = match is_blocklisted(context, source, blocklist_read_policy) {
        Ok(is_blocklisted) => is_blocklisted,
        Err(err) => {
            tracing::error!(
                address = %source,
                err = ?err,
                "blocklist read failed for selfdestruct source"
            );
            context.interpreter.halt_fatal();
            return Err(());
        }
    };
    if source_blocklisted {
        context
            .interpreter
            .bytecode
            .set_action(InterpreterAction::new_return(
                InstructionResult::Revert,
                helpers::revert_message_to_bytes(ERR_BLOCKED_ADDRESS),
                context.interpreter.gas,
            ));
        return Err(());
    }

    if matches!(target_warmth_policy, TargetWarmthPolicy::Transaction) {
        // Load target account and check if it is destructed.
        match context
            .host
            .journal_mut()
            .load_account_mut_skip_cold_load(target, skip_cold_load)
        {
            Ok(acc) => {
                if acc.data.account().is_selfdestructed() {
                    context
                        .interpreter
                        .bytecode
                        .set_action(InterpreterAction::new_return(
                            InstructionResult::Revert,
                            helpers::revert_message_to_bytes(ERR_SELFDESTRUCTED_BALANCE_INCREASED),
                            context.interpreter.gas,
                        ));
                    return Err(());
                }
                Ok(Some(acc.is_cold))
            }
            Err(JournalLoadError::ColdLoadSkipped) => {
                context.interpreter.halt_oog();
                Err(())
            }
            Err(JournalLoadError::DBError(e)) => {
                // Follow the original error handling on Host::selfdestruct for Context,
                tracing::error!("load account failed: {:?}", e);
                context.interpreter.halt_fatal();
                Err(())
            }
        }
    } else {
        if context
            .host
            .journal_mut()
            .warm_addresses
            .check_is_cold::<DB::Error>(&target, skip_cold_load)
            .is_err()
        {
            context.interpreter.halt_oog();
            return Err(());
        }

        // Load target account and check if it is destructed.
        match context.host.journal_mut().load_account(target) {
            Ok(acc) => {
                if acc.is_selfdestructed() {
                    context
                        .interpreter
                        .bytecode
                        .set_action(InterpreterAction::new_return(
                            InstructionResult::Revert,
                            helpers::revert_message_to_bytes(ERR_SELFDESTRUCTED_BALANCE_INCREASED),
                            context.interpreter.gas,
                        ));
                    return Err(());
                }
                Ok(Some(acc.is_cold))
            }
            Err(e) => {
                // Follow the original error handling on Host::selfdestruct for Context,
                tracing::error!("load account failed: {:?}", e);
                context.interpreter.halt_fatal();
                Err(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address, U256};
    use reth_ethereum::evm::revm::db::{EmptyDB, InMemoryDB};
    use reth_ethereum::evm::revm::{
        context::{Context, ContextTr, JournalTr},
        interpreter::{interpreter::EthInterpreter, Interpreter},
    };
    use revm::state::AccountStatus;
    use revm::DatabaseCommit;
    use revm_context_interface::journaled_state::account::JournaledAccountTr;
    use revm_interpreter::{Gas, InterpreterResult, SelfDestructResult};

    const ACCOUNT: Address = address!("0x1000000000000000000000000000000000000001");
    const TARGET: Address = address!("0x2000000000000000000000000000000000000002");
    const STATIC_GAS_COST: u64 = 5000;

    /// Helper struct to create different AccountStatus combinations for testing.
    struct State {}

    impl State {
        /// AccountStatus is empty.
        fn loaded() -> AccountStatus {
            AccountStatus::empty()
        }

        /// AccountStatus is loaded as not existing.
        fn loaded_new() -> AccountStatus {
            AccountStatus::LoadedAsNotExisting
        }

        /// Returns AccountStatus for a newly touched account that did not previously exist.
        /// This is represented by `AccountStatus::Touched | AccountStatus::LoadedAsNotExisting`,
        /// indicating the account was created during this transaction and its changes must be persisted.
        fn touch_new() -> AccountStatus {
            AccountStatus::Touched | AccountStatus::LoadedAsNotExisting
        }

        /// Returns AccountStatus for a newly created account (CREATE, CREATE2).
        /// This is represented by `AccountStatus::Touched | AccountStatus::Created | AccountStatus::CreatedLocal`,
        /// indicating the account was created during this transaction and its changes must be persisted.
        fn touch_created() -> AccountStatus {
            AccountStatus::Touched | AccountStatus::Created | AccountStatus::CreatedLocal
        }

        /// Returns AccountStatus for a newly destructed account.
        /// This is represented by `AccountStatus::Touched | AccountStatus::Created | AccountStatus::CreatedLocal | AccountStatus::SelfDestructed | AccountStatus::SelfDestructedLocal`,
        /// indicating the account was created then destroyed during this transaction and its changes must be persisted.
        fn touch_destructed() -> AccountStatus {
            AccountStatus::Touched
                | AccountStatus::Created
                | AccountStatus::CreatedLocal
                | AccountStatus::SelfDestructed
                | AccountStatus::SelfDestructedLocal
        }

        /// Similar to `touch_destructed`, but the account was not existing before.
        fn touch_new_destructed() -> AccountStatus {
            AccountStatus::LoadedAsNotExisting | Self::touch_destructed()
        }

        /// Returns AccountStatus for a account which was destructed before current transaction.
        /// This is represented by `AccountStatus::Touched | AccountStatus::Created | AccountStatus::SelfDestructed`,
        fn destructed_before() -> AccountStatus {
            AccountStatus::Touched | AccountStatus::Created | AccountStatus::SelfDestructed
        }

        /// Similar to `destructed_before`, but the account was not existing before.
        fn destructed_new_before() -> AccountStatus {
            AccountStatus::LoadedAsNotExisting | Self::destructed_before()
        }
    }

    /// Wrapper for the host context used in selfdestruct/account lifecycle state tests.
    struct HostTestEnv<DB: Database> {
        spec: SpecId,
        host: EthEvmContext<DB>,
    }

    #[derive(Clone, Copy)]
    enum SelfdestructHandler {
        Legacy,
        Zero7,
        Zero8,
    }

    impl<DB: Database> HostTestEnv<DB> {
        fn new(db: DB) -> Self {
            Self::new_with_spec(db, SpecId::PRAGUE)
        }

        fn new_with_spec(db: DB, spec: SpecId) -> Self {
            let host = Context::new(db, spec);
            Self { host, spec }
        }

        /// Sets the balance of an account in the host context.
        fn set_account_balance(&mut self, account: Address, balance: U256) {
            self.host
                .journal_mut()
                .load_account_mut_optional_code(account, false)
                .expect("load account")
                .set_balance(balance);
        }

        /// Set account to blocklist
        fn set_blocklist(&mut self, account: Address) {
            self.host
                .journal_mut()
                .load_account(native_coin_control::NATIVE_COIN_CONTROL_ADDRESS)
                .expect("load account to state");

            // Blocklist the target address
            let storage_slot = native_coin_control::compute_is_blocklisted_storage_slot(account);
            self.host
                .journal_mut()
                .sstore(
                    native_coin_control::NATIVE_COIN_CONTROL_ADDRESS,
                    storage_slot.into(),
                    U256::from(1),
                )
                .expect("sstore set blocklist");
        }

        /// Simulates the creation of a new contract account.
        /// This does not assign contract code, but initializes the account status only.
        fn simulate_create_account(&mut self, caller: Address, account: Address, balance: U256) {
            let spec_id = self.host.cfg.spec;
            self.host
                .journal_mut()
                .load_account(caller)
                .expect("load account");
            self.host
                .journal_mut()
                .load_account(account)
                .expect("load account");
            self.host
                .journal_mut()
                .create_account_checkpoint(caller, account, balance, spec_id)
                .expect("create account checkpoint");
        }

        /// Wrapper for journal selfdestruct, not include the Arc modifications.
        /// This is only for testing the account status.
        fn journal_selfdestruct(
            &mut self,
            account: Address,
            target: Address,
        ) -> StateLoad<SelfDestructResult> {
            self.host
                .journal_mut()
                .load_account(account)
                .expect("load account");
            self.host
                .journal_mut()
                .selfdestruct(account, target, false)
                .expect("selfdestruct")
        }

        /// Simulates a call to `arc_network_selfdestruct` for testing.
        /// Executes SELFDESTRUCT from `account` to `target` using the arc logic,
        /// Returns the resulting `InterpreterResult`.
        fn simulate_arc_selfdestruct(
            &mut self,
            account: Address,
            target: Address,
        ) -> InterpreterResult {
            self.simulate_arc_selfdestruct_full(account, target, None)
        }

        /// Like `simulate_arc_selfdestruct` but with a custom gas limit.
        /// Used to trigger ColdLoadSkipped when gas is below cold load cost after static cost.
        fn simulate_arc_selfdestruct_with_gas(
            &mut self,
            account: Address,
            target: Address,
            initial_gas_limit: Option<u64>,
        ) -> InterpreterResult {
            self.simulate_arc_selfdestruct_full_with_preload(
                account,
                target,
                initial_gas_limit,
                true,
                SelfdestructHandler::Zero8,
            )
        }

        /// Like `simulate_arc_selfdestruct_with_gas`, but uses the Zero7 handler.
        fn simulate_arc_selfdestruct_pre_zero8_with_gas(
            &mut self,
            account: Address,
            target: Address,
            initial_gas_limit: Option<u64>,
        ) -> InterpreterResult {
            self.simulate_arc_selfdestruct_full_with_preload(
                account,
                target,
                initial_gas_limit,
                true,
                SelfdestructHandler::Zero7,
            )
        }

        /// Full selfdestruct simulation with all parameters.
        fn simulate_arc_selfdestruct_full(
            &mut self,
            account: Address,
            target: Address,
            initial_gas_limit: Option<u64>,
        ) -> InterpreterResult {
            self.simulate_arc_selfdestruct_full_with_preload(
                account,
                target,
                initial_gas_limit,
                true,
                SelfdestructHandler::Zero8,
            )
        }

        fn simulate_arc_selfdestruct_without_native_coin_control_preload(
            &mut self,
            account: Address,
            target: Address,
        ) -> InterpreterResult {
            self.simulate_arc_selfdestruct_full_with_preload(
                account,
                target,
                None,
                false,
                SelfdestructHandler::Legacy,
            )
        }

        fn simulate_arc_selfdestruct_zero7_without_native_coin_control_preload(
            &mut self,
            account: Address,
            target: Address,
        ) -> InterpreterResult {
            self.simulate_arc_selfdestruct_full_with_preload(
                account,
                target,
                None,
                false,
                SelfdestructHandler::Zero7,
            )
        }

        fn simulate_arc_selfdestruct_zero8_without_native_coin_control_preload(
            &mut self,
            account: Address,
            target: Address,
        ) -> InterpreterResult {
            self.simulate_arc_selfdestruct_full_with_preload(
                account,
                target,
                None,
                false,
                SelfdestructHandler::Zero8,
            )
        }

        fn simulate_arc_selfdestruct_full_with_preload(
            &mut self,
            account: Address,
            target: Address,
            initial_gas_limit: Option<u64>,
            preload_native_coin_control: bool,
            handler: SelfdestructHandler,
        ) -> InterpreterResult {
            if preload_native_coin_control {
                self.host
                    .journal_mut()
                    .load_account(native_coin_control::NATIVE_COIN_CONTROL_ADDRESS)
                    .expect("load account to state");
            }

            // Builds an interpreter instance with the SELFDESTRUCT target already on the stack.
            let mut interpreter = Interpreter::<EthInterpreter>::default();
            if let Some(initial_gas_limit) = initial_gas_limit {
                interpreter.gas = Gas::new(initial_gas_limit);
            }
            interpreter.runtime_flag.spec_id = self.spec;

            interpreter.input.target_address = account;
            interpreter.input.caller_address = account;
            let success = interpreter
                .stack
                .push(U256::from_be_slice(target.into_word().as_ref()));
            if !success {
                panic!("push target to stack failed");
            }

            // Deduct the static cost of selfdestruct first to simulate the full op cost.
            assert!(interpreter.gas.record_regular_cost(STATIC_GAS_COST));

            // Prepare context and execute.
            let context = InstructionContext {
                interpreter: &mut interpreter,
                host: &mut self.host,
            };
            match handler {
                SelfdestructHandler::Legacy => arc_network_selfdestruct(context),
                SelfdestructHandler::Zero7 => arc_network_selfdestruct_zero7(context),
                SelfdestructHandler::Zero8 => arc_network_selfdestruct_zero8(context),
            }

            // The selfdestruct should halt and return a Return action.
            let next_action = interpreter.take_next_action();
            match next_action {
                InterpreterAction::Return(result, ..) => result,
                _ => panic!("Expected Return action"),
            }
        }
    }

    impl HostTestEnv<InMemoryDB> {
        /// Drop the data of transction context.
        fn commit_tx(&mut self) {
            self.host.journal_mut().commit_tx();
        }

        /// Set the changes to the database and drop execution context.
        fn commit_block(&mut self) {
            let state = self.host.journal_mut().finalize();
            self.host.db_mut().commit(state);
        }
    }

    /// Asserts that the account's status and balance match the expected values.
    /// Defined as a macro so assertions keep the test's original line number for accurate error reporting.
    ///
    /// Note: Invokes `load_account`, which causes the account to become warm.
    /// Avoid using when a cold account is required.
    macro_rules! assert_account_matches {
        ($env:ident, $account:expr, $expected_status:expr, $expected_balance:expr) => {
            assert_account_matches!($env, $account, $expected_status, $expected_balance, "");
        };
        ($env:ident, $account:expr, $expected_status:expr, $expected_balance:expr, $($msg:tt)+) => {
            let acc = $env.host
                .journal_mut()
                .load_account($account)
                .expect("load account");
            assert_eq!(
                (acc.status, acc.data.info.balance),
                ($expected_status, $expected_balance),
                $($msg)+
            );
        };
    }

    #[test]
    fn demo_account_selfdestruct_status_change() {
        let amount = U256::from(42);
        let mut env = HostTestEnv::new(InMemoryDB::default());

        assert_account_matches!(env, ACCOUNT, State::loaded_new(), U256::ZERO);

        // 1. Init ACCOUNT with balance, and commit to cache DB.
        env.set_account_balance(ACCOUNT, amount);
        assert_account_matches!(env, ACCOUNT, State::touch_new(), amount);
        env.commit_block();

        // 2. Verify the account status should be clear after finalize
        assert_account_matches!(env, ACCOUNT, State::loaded(), amount);
        env.commit_block();

        // 3. Selfdestruct ACCOUNT, transferring its entire balance to TARGET.
        let result = env.journal_selfdestruct(ACCOUNT, TARGET);
        assert_eq!(
            result,
            StateLoad {
                data: SelfDestructResult {
                    had_value: true,
                    target_exists: false,
                    previously_destroyed: false
                },
                is_cold: true,
            }
        );
        // selfdestruct does not touch the account, we need to touch it manually.
        env.host.journal_mut().touch_account(ACCOUNT);
        assert_account_matches!(env, ACCOUNT, AccountStatus::Touched, U256::ZERO);
        assert_account_matches!(env, TARGET, State::touch_new(), amount);
        env.commit_block();

        // 4. Create account first then selfdestruct ACCOUNT to create the destructed locally account.
        env.simulate_create_account(TARGET, ACCOUNT, amount);
        assert_account_matches!(env, ACCOUNT, State::touch_created(), amount);
        assert_account_matches!(env, TARGET, State::loaded(), U256::ZERO);
        assert_eq!(
            env.journal_selfdestruct(ACCOUNT, TARGET),
            StateLoad {
                data: SelfDestructResult {
                    had_value: true,
                    target_exists: false,
                    previously_destroyed: false
                },
                is_cold: false,
            }
        );
        assert_account_matches!(env, ACCOUNT, State::touch_destructed(), U256::ZERO);
        assert_account_matches!(env, TARGET, AccountStatus::Touched, amount);

        // 5. selfdestruct again in the same transaction
        assert_eq!(
            env.journal_selfdestruct(ACCOUNT, TARGET),
            StateLoad {
                data: SelfDestructResult {
                    had_value: false,
                    target_exists: true,
                    previously_destroyed: true,
                },
                is_cold: false,
            }
        );

        // 6. Simulate another transaction started in the same block.
        //    The ACCOUNT should be destroyed and local flags are removed.
        env.commit_tx();
        assert_account_matches!(env, ACCOUNT, State::destructed_before(), U256::ZERO);

        // 7. Create again, and destruct again
        env.simulate_create_account(TARGET, ACCOUNT, amount);
        env.journal_selfdestruct(ACCOUNT, TARGET);
        assert_account_matches!(env, ACCOUNT, State::touch_destructed(), U256::ZERO);
        assert_account_matches!(env, TARGET, AccountStatus::Touched, amount);
    }

    #[test]
    fn selfdestruct_no_event_emitted_when_balance_zero() {
        let mut env = HostTestEnv::new(EmptyDB::new());

        let res = env.simulate_arc_selfdestruct(ACCOUNT, TARGET);
        assert_eq!(res.result, InstructionResult::SelfDestruct);
        assert_eq!(res.gas.total_gas_spent(), 7600u64); // 5000 static cost + 2600 cold target, no transfer to create new account
        assert_eq!(res.gas.refunded(), 0i64);

        assert!(
            env.host.journal_mut().take_logs().is_empty(),
            "no transfer event expected"
        );
    }

    /// Selfdestruct with non-zero balance emits an EIP-7708 Transfer log.
    #[test]
    fn selfdestruct_emits_eip7708_transfer_log() {
        let amount = U256::from(42);
        let mut env = HostTestEnv::new(EmptyDB::new());
        env.set_account_balance(ACCOUNT, amount);

        let res = env.simulate_arc_selfdestruct_full(ACCOUNT, TARGET, None);
        assert_eq!(res.result, InstructionResult::SelfDestruct);

        // Verify EIP-7708 Transfer log was emitted
        let logs = env.host.journal_mut().take_logs();
        assert_eq!(
            logs.len(),
            1,
            "exactly one EIP-7708 Transfer log expected from selfdestruct"
        );
        assert_eq!(
            logs[0].address,
            revm::handler::SYSTEM_ADDRESS,
            "Log should be from EIP-7708 system address"
        );

        // Verify balance was still transferred
        assert_account_matches!(env, ACCOUNT, State::touch_new(), U256::ZERO);
        assert_account_matches!(env, TARGET, State::touch_new(), amount);
    }

    /// SELFDESTRUCT to Address::ZERO with non-zero balance should revert.
    #[test]
    fn selfdestruct_to_zero_address_reverts() {
        let amount = U256::from(42);
        let mut env = HostTestEnv::new(EmptyDB::new());
        env.set_account_balance(ACCOUNT, amount);

        let res = env.simulate_arc_selfdestruct_full(ACCOUNT, Address::ZERO, None);
        assert_eq!(
            res.result,
            InstructionResult::Revert,
            "SELFDESTRUCT to zero address should revert"
        );

        // Balance should NOT have been transferred (account was touched during setup)
        assert_account_matches!(env, ACCOUNT, State::touch_new(), amount);
    }

    #[test]
    fn selfdestruct_cold_load_skipped_halts_oog() {
        // Gas limit so that after static cost, remaining < cold load cost (2600) → skip_cold_load = true.
        // Target is cold (not loaded), so host.selfdestruct returns ColdLoadSkipped.
        let initial_gas = STATIC_GAS_COST + 100;

        let mut env = HostTestEnv::new(EmptyDB::new());
        env.set_account_balance(ACCOUNT, U256::from(1));

        let res = env.simulate_arc_selfdestruct_with_gas(ACCOUNT, TARGET, Some(initial_gas));

        assert_eq!(
            res.result,
            InstructionResult::OutOfGas,
            "ColdLoadSkipped from host.selfdestruct should halt with OutOfGas"
        );
        assert!(
            res.gas.total_gas_spent() <= initial_gas,
            "gas spent should not exceed initial limit"
        );
    }

    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "NativeCoinControl should be preloaded")
    )]
    #[test]
    fn selfdestruct_zero7_recovers_or_debug_asserts_when_native_coin_control_is_not_loaded() {
        let mut db = InMemoryDB::default();
        let slot = native_coin_control::compute_is_blocklisted_storage_slot(TARGET);
        db.insert_account_storage(
            native_coin_control::NATIVE_COIN_CONTROL_ADDRESS,
            slot.into(),
            U256::from(1),
        )
        .expect("insert blocklist storage");
        let mut env = HostTestEnv::new(db);
        env.set_account_balance(ACCOUNT, U256::from(1));

        let res = env
            .simulate_arc_selfdestruct_zero7_without_native_coin_control_preload(ACCOUNT, TARGET);

        #[cfg(debug_assertions)]
        let _ = res;

        #[cfg(not(debug_assertions))]
        {
            assert_eq!(res.result, InstructionResult::Revert);
            assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST);
            assert_eq!(res.gas.refunded(), 0);
            assert_account_matches!(env, ACCOUNT, State::touch_new(), U256::from(1));
            assert_account_matches!(env, TARGET, State::loaded_new(), U256::ZERO);
        }
    }

    #[test]
    fn selfdestruct_pre_zero7_preserves_fail_open_without_native_coin_control_loaded() {
        let mut db = InMemoryDB::default();
        let slot = native_coin_control::compute_is_blocklisted_storage_slot(TARGET);
        db.insert_account_storage(
            native_coin_control::NATIVE_COIN_CONTROL_ADDRESS,
            slot.into(),
            U256::from(1),
        )
        .expect("insert blocklist storage");
        let mut env = HostTestEnv::new(db);
        env.set_account_balance(ACCOUNT, U256::from(1));

        let res =
            env.simulate_arc_selfdestruct_without_native_coin_control_preload(ACCOUNT, TARGET);

        assert_eq!(res.result, InstructionResult::SelfDestruct);
        assert_account_matches!(env, ACCOUNT, State::touch_new(), U256::ZERO);
        assert_account_matches!(env, TARGET, State::touch_new(), U256::from(1));
    }

    /// Database wrapper that forces a storage-read failure for the Native Coin Control
    /// blocklist account, while delegating every other access to an inner `InMemoryDB`.
    ///
    /// This lets the SELFDESTRUCT opcode tests inject a `LoadError::DBError` on the
    /// blocklist SLOAD: `basic()` (used to preload/warm the NCC account) is delegated so
    /// the account loads cleanly, but `storage()` for `NATIVE_COIN_CONTROL_ADDRESS` errors,
    /// which the journal surfaces to the host as `LoadError::DBError`.
    #[derive(Debug, thiserror::Error)]
    #[error("forced blocklist storage read failure")]
    struct ForcedBlocklistReadError;
    impl revm::database_interface::DBErrorMarker for ForcedBlocklistReadError {}

    #[derive(Debug)]
    struct BlocklistReadFailingDb {
        inner: InMemoryDB,
    }

    impl BlocklistReadFailingDb {
        fn new(inner: InMemoryDB) -> Self {
            Self { inner }
        }
    }

    impl revm::Database for BlocklistReadFailingDb {
        type Error = ForcedBlocklistReadError;

        fn basic(
            &mut self,
            address: Address,
        ) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
            <InMemoryDB as revm::Database>::basic(&mut self.inner, address)
                .map_err(|infallible: core::convert::Infallible| match infallible {})
        }

        fn code_by_hash(
            &mut self,
            code_hash: alloy_primitives::B256,
        ) -> Result<revm::state::Bytecode, Self::Error> {
            <InMemoryDB as revm::Database>::code_by_hash(&mut self.inner, code_hash)
                .map_err(|infallible: core::convert::Infallible| match infallible {})
        }

        fn storage(
            &mut self,
            address: Address,
            index: revm_primitives::StorageKey,
        ) -> Result<revm_primitives::StorageValue, Self::Error> {
            // Force the failure only for the blocklist account so the test exercises the
            // exact SLOAD that `is_blocklisted` performs.
            if address == native_coin_control::NATIVE_COIN_CONTROL_ADDRESS {
                return Err(ForcedBlocklistReadError);
            }
            <InMemoryDB as revm::Database>::storage(&mut self.inner, address, index)
                .map_err(|infallible: core::convert::Infallible| match infallible {})
        }

        fn block_hash(&mut self, number: u64) -> Result<alloy_primitives::B256, Self::Error> {
            <InMemoryDB as revm::Database>::block_hash(&mut self.inner, number)
                .map_err(|infallible: core::convert::Infallible| match infallible {})
        }
    }

    /// A blocklist SLOAD that hits a DB error during SELFDESTRUCT must
    /// hard-halt under the Zero7 `FailClosed` policy (the error propagates) and must be
    /// swallowed under the pre-Zero7 `FailOpen` policy (treated as not-blocklisted, so
    /// SELFDESTRUCT proceeds). The DB error is injected via `BlocklistReadFailingDb`,
    /// which errors on `storage()` for `NATIVE_COIN_CONTROL_ADDRESS`.
    #[test]
    fn selfdestruct_blocklist_db_error_fail_closed_halts_fail_open_swallows() {
        // FailClosed (Zero7): the blocklist read error must propagate as a fatal halt.
        {
            let db = BlocklistReadFailingDb::new(InMemoryDB::default());
            let mut env = HostTestEnv::new(db);
            env.set_account_balance(ACCOUNT, U256::from(1));

            // args: (account, target, gas, preload_native_coin_control, handler)
            // SelfdestructHandler::Zero7 selects the FailClosed handler.
            let res = env.simulate_arc_selfdestruct_full_with_preload(
                ACCOUNT,
                TARGET,
                None,
                true,
                SelfdestructHandler::Zero7,
            );

            assert_eq!(
                res.result,
                InstructionResult::FatalExternalError,
                "Zero7 FailClosed: a blocklist SLOAD DBError must hard-halt (fatal), not be swallowed"
            );
            // The fatal halt occurs before any balance transfer, so no transfer log.
            assert!(
                env.host.journal_mut().take_logs().is_empty(),
                "no transfer log expected when the read fails closed"
            );
        }

        // FailOpen (pre-Zero7): the same read error is swallowed; SELFDESTRUCT proceeds.
        {
            let db = BlocklistReadFailingDb::new(InMemoryDB::default());
            let mut env = HostTestEnv::new(db);
            env.set_account_balance(ACCOUNT, U256::from(1));

            // SelfdestructHandler::Legacy selects the pre-Zero7 FailOpen handler.
            let res = env.simulate_arc_selfdestruct_full_with_preload(
                ACCOUNT,
                TARGET,
                None,
                true,
                SelfdestructHandler::Legacy,
            );

            assert_eq!(
                res.result,
                InstructionResult::SelfDestruct,
                "Pre-Zero7 FailOpen: a blocklist SLOAD DBError must be swallowed so SELFDESTRUCT succeeds"
            );
            assert_account_matches!(env, ACCOUNT, State::touch_new(), U256::ZERO);
            assert_account_matches!(env, TARGET, State::touch_new(), U256::from(1));
        }
    }

    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "NativeCoinControl should be preloaded")
    )]
    #[test]
    fn selfdestruct_zero8_includes_zero7_fail_closed_blocklist_reads() {
        let mut db = InMemoryDB::default();
        let slot = native_coin_control::compute_is_blocklisted_storage_slot(TARGET);
        db.insert_account_storage(
            native_coin_control::NATIVE_COIN_CONTROL_ADDRESS,
            slot.into(),
            U256::from(1),
        )
        .expect("insert blocklist storage");
        let mut env = HostTestEnv::new(db);
        env.set_account_balance(ACCOUNT, U256::from(1));

        let res = env
            .simulate_arc_selfdestruct_zero8_without_native_coin_control_preload(ACCOUNT, TARGET);

        #[cfg(debug_assertions)]
        let _ = res;

        #[cfg(not(debug_assertions))]
        {
            assert_eq!(res.result, InstructionResult::Revert);
            assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST);
            assert_eq!(res.gas.refunded(), 0);
            assert_account_matches!(env, ACCOUNT, State::touch_new(), U256::from(1));
            assert_account_matches!(env, TARGET, State::loaded_new(), U256::ZERO);
        }
    }

    #[test]
    fn selfdestruct_to_transaction_warm_target_with_low_gas_succeeds() {
        // Gas limit so that after static cost, remaining < cold load cost (2600), so skip_cold_load = true.
        // TARGET was warmed earlier in this transaction, so SELFDESTRUCT must not require a cold load.
        let initial_gas = STATIC_GAS_COST + 100;

        let mut env = HostTestEnv::new(EmptyDB::new());
        env.set_account_balance(ACCOUNT, U256::from(1));
        env.set_account_balance(TARGET, U256::from(1));

        let res = env.simulate_arc_selfdestruct_with_gas(ACCOUNT, TARGET, Some(initial_gas));

        assert_eq!(
            res.result,
            InstructionResult::SelfDestruct,
            "transaction-warm target should not require cold selfdestruct gas"
        );
        assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST);
    }

    #[test]
    fn selfdestruct_to_transaction_warm_target_before_zero8_halts_oog() {
        // Same setup as the Zero8 behavior, but the legacy handler only consults
        // warm_addresses and therefore still treats TARGET as cold.
        let initial_gas = STATIC_GAS_COST + 100;

        let mut env = HostTestEnv::new(EmptyDB::new());
        env.set_account_balance(ACCOUNT, U256::from(1));
        env.set_account_balance(TARGET, U256::from(1));

        let res =
            env.simulate_arc_selfdestruct_pre_zero8_with_gas(ACCOUNT, TARGET, Some(initial_gas));

        assert_eq!(
            res.result,
            InstructionResult::OutOfGas,
            "pre-Zero8 SELFDESTRUCT should not use transaction-warm target state"
        );
        assert!(
            res.gas.total_gas_spent() <= initial_gas,
            "gas spent should not exceed initial limit"
        );
    }

    #[test]
    fn selfdestruct_to_transaction_warm_target_before_zero8_preserves_legacy_warm_charge() {
        let initial_gas = STATIC_GAS_COST + 2600;

        let mut env = HostTestEnv::new(EmptyDB::new());
        env.set_account_balance(ACCOUNT, U256::from(1));
        env.set_account_balance(TARGET, U256::from(1));

        let res =
            env.simulate_arc_selfdestruct_pre_zero8_with_gas(ACCOUNT, TARGET, Some(initial_gas));

        assert_eq!(res.result, InstructionResult::SelfDestruct);
        assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST);
    }

    // Regression tests against existing behavior

    #[test]
    fn selfdestruct_refund_recorded_pre_london() {
        let mut env = HostTestEnv::new_with_spec(EmptyDB::new(), SpecId::ISTANBUL);

        let res = env.simulate_arc_selfdestruct(ACCOUNT, TARGET);
        assert_eq!(res.result, InstructionResult::SelfDestruct);
        assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST);
        assert_eq!(res.gas.refunded(), gas::SELFDESTRUCT_REFUND);
    }

    #[test]
    fn selfdestruct_no_refund_after_london() {
        let mut env = HostTestEnv::new_with_spec(EmptyDB::new(), SpecId::LONDON);

        let res = env.simulate_arc_selfdestruct(ACCOUNT, TARGET);
        assert_eq!(res.result, InstructionResult::SelfDestruct);
        assert_eq!(res.gas.total_gas_spent(), 7600u64); // 5000 static cost + 2600 cold target
        assert_eq!(res.gas.refunded(), 0i64);
    }

    #[test]
    fn selfdestruct_transfers_balance() {
        let initial_account_balance = U256::from(123u64);
        let initial_target_balance = U256::from(456u64);

        let mut env = HostTestEnv::new(EmptyDB::new());

        // Set balances
        env.set_account_balance(ACCOUNT, initial_account_balance);
        env.set_account_balance(TARGET, initial_target_balance);

        let res = env.simulate_arc_selfdestruct(ACCOUNT, TARGET);
        assert_eq!(res.result, InstructionResult::SelfDestruct);
        assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST);
        assert_eq!(res.gas.refunded(), 0i64);

        // Verify balances
        assert_account_matches!(env, ACCOUNT, State::touch_new(), U256::ZERO);
        assert_account_matches!(
            env,
            TARGET,
            State::touch_new(),
            initial_target_balance + initial_account_balance
        );
    }

    #[test]
    fn selfdestruct_rejects_transfers_to_self() {
        let initial_account_balance = U256::from(123u64);

        let mut env = HostTestEnv::new(EmptyDB::new());

        // Set balances
        env.set_account_balance(ACCOUNT, initial_account_balance);

        let res = env.simulate_arc_selfdestruct(ACCOUNT, ACCOUNT);
        assert_eq!(res.result, InstructionResult::Revert);
        assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST);
        assert_eq!(res.gas.refunded(), 0i64);

        // Verify balance is unchanged
        assert_account_matches!(env, ACCOUNT, State::touch_new(), initial_account_balance);
    }

    #[test]
    fn selfdestruct_blocked_when_target_is_blocklisted() {
        let initial_account_balance = U256::from(456u64);
        let initial_target_balance = U256::from(789u64);

        let mut env = HostTestEnv::new(EmptyDB::new());

        // Set balances
        env.set_account_balance(ACCOUNT, initial_account_balance);
        env.set_account_balance(TARGET, initial_target_balance);

        // Blocklist the target address
        env.set_blocklist(TARGET);

        let res = env.simulate_arc_selfdestruct(ACCOUNT, TARGET);
        assert_eq!(
            res.result,
            InstructionResult::Revert,
            "Selfdestruct should revert when target is blocklisted"
        );
        assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST);
        assert_eq!(res.gas.refunded(), 0i64);

        // Verify balances are unchanged
        assert_account_matches!(env, ACCOUNT, State::touch_new(), initial_account_balance);
        assert_account_matches!(env, TARGET, State::touch_new(), initial_target_balance);
    }

    #[test]
    fn selfdestruct_blocked_when_selfdestructing_address_is_blocklisted() {
        let initial_account_balance = U256::from(321u64);
        let initial_target_balance = U256::from(654u64);

        let mut env = HostTestEnv::new(EmptyDB::new());

        // Set balances
        env.set_account_balance(ACCOUNT, initial_account_balance);
        env.set_account_balance(TARGET, initial_target_balance);

        // Blocklist the selfdestructing address (ACCOUNT)
        env.set_blocklist(ACCOUNT);

        // Run interpreter
        let res = env.simulate_arc_selfdestruct(ACCOUNT, TARGET);

        // Verify the operation was reverted by checking the action set on bytecode
        assert_eq!(
            res.result,
            InstructionResult::Revert,
            "Selfdestruct should revert when selfdestructing address is blocklisted"
        );
        assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST);
        assert_eq!(res.gas.refunded(), 0i64);

        // Verify balances are unchanged
        assert_account_matches!(env, ACCOUNT, State::touch_new(), initial_account_balance);
        assert_account_matches!(env, TARGET, State::touch_new(), initial_target_balance);
    }

    // transfer to a destructed account

    #[test]
    fn selfdestruct_transfer_to_not_destructed_account() {
        let amount = U256::from(234);

        // Prepare host context with caller balance.
        let mut env = HostTestEnv::new(EmptyDB::new());
        env.set_account_balance(ACCOUNT, amount);

        // Destruct `ACCOUNT`, transfer to empty, cold account `TARGET`
        // After Cancun, the account will not be destructed if it was not created on the same transaction.
        let res = env.simulate_arc_selfdestruct(ACCOUNT, TARGET);
        assert_eq!(res.result, InstructionResult::SelfDestruct);
        assert_eq!(res.gas.total_gas_spent(), 32600u64);
        assert_eq!(res.gas.refunded(), 0i64);
        assert_account_matches!(env, ACCOUNT, State::touch_new(), U256::ZERO);
        assert_account_matches!(env, TARGET, State::touch_new(), amount);

        // Destruct `TARGET`, transfer to warm, not destructed account `ACCOUNT`
        let res = env.simulate_arc_selfdestruct(TARGET, ACCOUNT);
        assert_eq!(res.result, InstructionResult::SelfDestruct);
        assert_eq!(res.gas.total_gas_spent(), 30000u64); // // 5000 static + 25000 new account, warm target
        assert_eq!(res.gas.refunded(), 0i64);
        assert_account_matches!(env, ACCOUNT, State::touch_new(), amount);
        assert_account_matches!(env, TARGET, State::touch_new(), U256::ZERO);
    }

    struct TransferToDestructedTestCase {
        locally: bool,
        amount: U256,
        expect_state_after_selfdestructed: (AccountStatus, AccountStatus),
        expect_state_after_committed: (AccountStatus, AccountStatus),
    }

    impl TransferToDestructedTestCase {
        fn expect_revert(&self) -> bool {
            !self.amount.is_zero()
        }
    }

    #[test]
    fn selfdestruct_transfer_to_destructed_account() {
        let sender = address!("0x3000000000000000000000000000000000000002");

        let test_cases = [
            TransferToDestructedTestCase {
                locally: true,
                amount: U256::ZERO,
                expect_state_after_selfdestructed: (
                    State::touch_new(),
                    State::touch_new_destructed(),
                ),
                expect_state_after_committed: (State::loaded(), State::loaded_new()),
            },
            TransferToDestructedTestCase {
                locally: true,
                amount: U256::from(234),
                expect_state_after_selfdestructed: (
                    State::touch_new(),
                    State::touch_new_destructed(),
                ),
                expect_state_after_committed: (State::loaded(), State::loaded_new()),
            },
            TransferToDestructedTestCase {
                locally: false,
                amount: U256::ZERO,
                expect_state_after_selfdestructed: (
                    State::touch_new(),
                    State::destructed_new_before(),
                ),
                expect_state_after_committed: (State::loaded(), State::loaded_new()),
            },
            TransferToDestructedTestCase {
                locally: false,
                amount: U256::from(234),
                expect_state_after_selfdestructed: (
                    State::touch_new(),
                    State::destructed_new_before(),
                ),
                expect_state_after_committed: (State::loaded(), State::loaded_new()),
            },
        ];

        for (index, tc) in test_cases.iter().enumerate() {
            let desc = format!(
                "index={index}, locally={}, amount={}",
                tc.locally, tc.amount
            );
            let mut env = HostTestEnv::new(InMemoryDB::default());

            // Prepare a destruct locally account TARGET, don't expect it will fail here.
            env.set_account_balance(sender, tc.amount);
            env.simulate_create_account(sender, TARGET, tc.amount);
            assert_account_matches!(env, sender, State::touch_new(), U256::ZERO, "({desc})");
            let res = env.simulate_arc_selfdestruct(TARGET, ACCOUNT);
            assert_eq!(res.result, InstructionResult::SelfDestruct, "({desc})");
            assert_account_matches!(
                env,
                TARGET,
                State::touch_new_destructed(),
                U256::ZERO,
                "({desc})"
            );
            assert_account_matches!(env, ACCOUNT, State::touch_new(), tc.amount, "({desc})");

            // If the test case requires a new transaction context, commit the current transaction.
            if !tc.locally {
                env.commit_tx();
            }

            // Destruct ACCOUNT, transfer balance to local destructed account `TARGET`
            let res = env.simulate_arc_selfdestruct(ACCOUNT, TARGET);

            let (account_amount, target_amount) = if tc.expect_revert() {
                assert_eq!(
                    res.result,
                    InstructionResult::Revert,
                    "({desc}) not reverted",
                );
                assert_eq!(res.gas.total_gas_spent(), STATIC_GAS_COST, "({desc})");
                assert_eq!(res.gas.refunded(), 0i64, "({desc})");
                (tc.amount, U256::ZERO)
            } else {
                assert_eq!(
                    res.result,
                    InstructionResult::SelfDestruct,
                    "({desc}) not selfdestructed",
                );
                (U256::ZERO, tc.amount)
            };
            assert_account_matches!(
                env,
                ACCOUNT,
                tc.expect_state_after_selfdestructed.0,
                account_amount,
                "({desc}) ACCOUNT state mismatch after selfdestruct",
            );
            assert_account_matches!(
                env,
                TARGET,
                tc.expect_state_after_selfdestructed.1,
                target_amount,
                "({desc}) TARGET state mismatch after selfdestruct",
            );

            // Commit the block changes to the database.
            env.commit_block();
            let (account_amount, target_amount) = if tc.expect_revert() {
                (tc.amount, U256::ZERO)
            } else {
                (U256::ZERO, U256::ZERO)
            };
            assert_account_matches!(
                env,
                ACCOUNT,
                tc.expect_state_after_committed.0,
                account_amount,
                "({desc}) ACCOUNT state mismatch after commit block",
            );
            assert_account_matches!(
                env,
                TARGET,
                tc.expect_state_after_committed.1,
                target_amount,
                "({desc}) TARGET state mismatch after commit block",
            );
        }
    }

    // B8: Verify Arc's SELFDESTRUCT restrictions make REVM's delayed burn log unreachable.
    //
    // REVM emits Selfdestruct(address,uint256) logs at end-of-transaction for self-destructed
    // accounts that still have balance (e.g., account A selfdestructs, then receives ETH from
    // account B's selfdestruct in the same transaction). Arc does not implement delayed burn
    // logs because its restrictions prevent the scenarios that would trigger them:
    //   1. SELFDESTRUCT to self is always rejected (revert)
    //   2. SELFDESTRUCT to Address::ZERO is rejected
    //   3. SELFDESTRUCT to an already-destructed target is rejected
    // These tests verify each restriction holds under baseline EIP-7708 log mode.

    /// SELFDESTRUCT to self is rejected, so a self-destructed account cannot
    /// re-receive its own balance (the primary delayed burn scenario).
    #[test]
    fn selfdestruct_to_self_rejected() {
        let amount = U256::from(100);
        let mut env = HostTestEnv::new(EmptyDB::new());
        env.set_account_balance(ACCOUNT, amount);

        let res = env.simulate_arc_selfdestruct_full(ACCOUNT, ACCOUNT, None);
        assert_eq!(
            res.result,
            InstructionResult::Revert,
            "SELFDESTRUCT to self should revert"
        );

        // Balance unchanged — no state modification
        assert_account_matches!(env, ACCOUNT, State::touch_new(), amount);

        // No logs emitted
        let logs = env.host.journal_mut().take_logs();
        assert!(logs.is_empty(), "SELFDESTRUCT to self should emit no logs");
    }

    /// SELFDESTRUCT to an already-destructed target is rejected, preventing
    /// cross-selfdestruct balance accumulation that would require delayed burn logs.
    #[test]
    fn selfdestruct_to_destructed_target_rejected() {
        let amount_a = U256::from(100);
        let amount_b = U256::from(200);
        let mut env = HostTestEnv::new(EmptyDB::new());

        // Create two accounts and selfdestruct ACCOUNT → TARGET first.
        env.simulate_create_account(ACCOUNT, ACCOUNT, U256::ZERO);
        env.set_account_balance(ACCOUNT, amount_a);
        env.simulate_create_account(TARGET, TARGET, U256::ZERO);
        env.set_account_balance(TARGET, amount_b);

        // First: ACCOUNT selfdestructs to TARGET (succeeds)
        let res1 = env.simulate_arc_selfdestruct_full(ACCOUNT, TARGET, None);
        assert_eq!(
            res1.result,
            InstructionResult::SelfDestruct,
            "First selfdestruct should succeed"
        );

        // TARGET now has balance: amount_b + amount_a
        // TARGET selfdestructs back to ACCOUNT — but ACCOUNT is now destructed.
        // Arc's check_selfdestruct_accounts rejects this.
        let res2 = env.simulate_arc_selfdestruct_full(TARGET, ACCOUNT, None);
        assert_eq!(
            res2.result,
            InstructionResult::Revert,
            "SELFDESTRUCT to already-destructed target should revert"
        );
    }

    /// SELFDESTRUCT to Address::ZERO with zero balance succeeds but produces no log.
    /// (The zero-address check only triggers when balance is non-zero.)
    #[test]
    fn selfdestruct_to_zero_address_zero_balance_succeeds() {
        let mut env = HostTestEnv::new(EmptyDB::new());
        // ACCOUNT has zero balance
        env.set_account_balance(ACCOUNT, U256::ZERO);

        let res = env.simulate_arc_selfdestruct_full(ACCOUNT, Address::ZERO, None);
        // Zero balance → the non-zero branch is skipped entirely → proceeds to host.selfdestruct
        assert_eq!(
            res.result,
            InstructionResult::SelfDestruct,
            "SELFDESTRUCT to zero address with zero balance should succeed"
        );

        let logs = env.host.journal_mut().take_logs();
        assert!(
            logs.is_empty(),
            "SELFDESTRUCT with zero balance should emit no logs"
        );
    }
}
