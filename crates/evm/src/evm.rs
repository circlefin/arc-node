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

extern crate alloc;

use crate::assembler::ArcBlockAssembler;
use crate::executor::ArcBlockExecutor;
use crate::frame_result::{
    create_blocklisted_frame_result, create_frame_result, create_oog_frame_result,
    BeforeFrameInitResult, BLOCKLISTED_GAS_PENALTY,
};
use crate::log::{create_eip7708_transfer_log, create_native_transfer_log};
use alloc::sync::Arc;
use alloy_evm::eth::EthEvmContext;
use alloy_evm::{
    block::{BlockExecutorFactory, BlockExecutorFor},
    eth::EthBlockExecutionCtx,
    precompiles::PrecompilesMap,
    Evm as AlloyEvmTrait, EvmFactory,
};
use alloy_rpc_types_engine::ExecutionData;
use arc_execution_config::hardforks::{ArcHardfork, ArcHardforkFlags};
use arc_execution_config::native_coin_control::{
    compute_is_blocklisted_storage_slot, is_blocklisted_status,
};
use arc_precompiles::helpers::{
    ERR_SELFDESTRUCTED_BALANCE_INCREASED, ERR_ZERO_ADDRESS, PRECOMPILE_SLOAD_GAS_COST,
};
use arc_precompiles::NATIVE_COIN_CONTROL_ADDRESS;
use core::fmt::Debug;
use reth_ethereum::evm::revm::context::block::BlockEnv;
use reth_ethereum::evm::revm::primitives::U256;
use reth_ethereum::{
    evm::{
        primitives::{Database, EvmEnv, InspectorFor, NextBlockEnvAttributes},
        revm::{
            context::{Context, ContextTr, JournalTr, TxEnv},
            context_interface::result::{EVMError, HaltReason},
            db::State,
            inspector::{Inspector, NoOpInspector},
            interpreter::interpreter::EthInterpreter,
            primitives::hardfork::SpecId,
        },
        EthEvmConfig,
    },
    node::api::ConfigureEvm,
    primitives::{Header, SealedBlock, SealedHeader},
    Receipt, TransactionSigned,
};
use reth_evm::execute::BlockBuilder;
use reth_evm::{ConfigureEngineEvm, EvmEnvFor, ExecutableTxIterator, ExecutionCtxFor};
use reth_primitives_traits::NodePrimitives;
use revm::bytecode::opcode::SELFDESTRUCT;
use revm::context_interface::result::ResultAndState;
use revm::handler::evm::{ContextDbError, FrameInitResult};
use revm::handler::instructions::InstructionProvider;
use revm::handler::{EvmTr, FrameInitOrResult, FrameResult, FrameTr, Handler, ItemOrResult};
use revm::inspector::{InspectorEvmTr, InspectorHandler, JournalExt};
use revm::Database as RevmDatabase;
use revm::ExecuteEvm;
use revm::{
    context::{
        result::{ExecResultAndState, ExecutionResult, InvalidTransaction},
        ContextSetters, Evm as RevmEvm,
    },
    handler::{instructions::EthInstructions, EthFrame, PrecompileProvider},
    interpreter::{CallOutcome, Gas, InstructionResult, InterpreterResult},
    state::EvmState,
    InspectEvm, SystemCallEvm,
};
use revm_context_interface::FrameStack;
use revm_context_interface::Transaction;
use revm_interpreter::interpreter_action::FrameInit;
use revm_interpreter::{CallScheme, CreateScheme, FrameInput, Instruction};
use revm_primitives::{Address, Bytes};
use std::collections::{HashMap, HashSet};

use crate::handler::ArcEvmHandler;
use crate::opcode::{arc_network_selfdestruct, arc_network_selfdestruct_zero4};
use crate::subcall::{SubcallContinuation, SubcallRegistry};
use arc_execution_config::chainspec::{ArcChainSpec, BlockGasLimitProvider};
use arc_execution_config::protocol_config::{
    expected_gas_limit, retrieve_fee_params, retrieve_reward_beneficiary,
};
use arc_precompiles::call_from::{CallFromPrecompile, CALL_FROM_ADDRESS};
use arc_precompiles::precompile_provider::ArcPrecompileProvider;
use arc_precompiles::subcall::SubcallPrecompile;
use revm::interpreter::interpreter_action::CallInputs;

/// Flat gas cost charged for rejected subcall dispatches (unauthorized caller, wrong scheme,
/// static context, value attached, init_subcall errors). Charged by `init_subcall_revert` calls.
/// Prevents zero-cost probing of subcall precompile addresses.
const SUBCALL_DISPATCH_COST: u64 = 100;

/// Construct a revert `FrameResult` for a subcall precompile rejection.
fn init_subcall_revert(message: &str, call_inputs: &CallInputs) -> FrameResult {
    let revert_bytes = arc_precompiles::helpers::revert_message_to_bytes(message);
    let mut gas = Gas::new(call_inputs.gas_limit);
    // Charge a flat dispatch cost. If the caller doesn't have enough gas, consume all of it.
    if !gas.record_cost(SUBCALL_DISPATCH_COST) {
        gas.spend_all();
    }
    let result = InterpreterResult::new(InstructionResult::Revert, revert_bytes, gas);
    FrameResult::Call(CallOutcome {
        result,
        memory_offset: call_inputs.return_memory_offset.clone(),
        was_precompile_called: true,
        precompile_call_logs: Default::default(),
    })
}

#[derive(Debug)]
pub struct ArcEvm<CTX, INSP, I, P, F = EthFrame<EthInterpreter>> {
    /// Inner EVM type.
    pub inner: RevmEvm<CTX, INSP, I, P, F>,
    pub inspect: bool,
    /// Feature flags for Arc hardforks active at the current block.
    pub hardfork_flags: ArcHardforkFlags,
    /// Registry of subcall-capable precompiles.
    subcall_registry: Arc<SubcallRegistry>,
    /// Active subcall continuations, keyed by the precompile call's depth.
    subcall_continuations: HashMap<usize, SubcallContinuation>,
}

/// ArcEvm implementation, wrapping an inner revm EVM instance to apply handler
/// 1. Hook frame_init to add the NativeCoinTransferred event log.
/// 2. Add the blocklist check for each frame.
/// 3. Check static context to the precompiles.
impl<CTX: ContextTr, INSP, P> ArcEvm<CTX, INSP, EthInstructions<EthInterpreter, CTX>, P> {
    /// Create a new Arc EVM.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: CTX,
        inspector: INSP,
        precompiles: P,
        instruction: EthInstructions<EthInterpreter, CTX>,
        inspect: bool,
        hardfork_flags: ArcHardforkFlags,
        subcall_registry: Arc<SubcallRegistry>,
    ) -> Self {
        Self {
            inner: RevmEvm {
                ctx,
                inspector,
                instruction,
                precompiles,
                frame_stack: FrameStack::new(),
            },
            inspect,
            hardfork_flags,
            subcall_registry,
            subcall_continuations: HashMap::new(),
        }
    }
}

/// Extracts transfer parameters (from, to, amount) from a Call frame input.
/// Returns None if the call scheme doesn't involve a value transfer.
fn extract_call_transfer_params(
    inputs: &revm_interpreter::CallInputs,
) -> Option<(Address, Address, U256)> {
    match inputs.scheme {
        CallScheme::DelegateCall | CallScheme::StaticCall | CallScheme::CallCode => None,
        CallScheme::Call => Some((
            inputs.transfer_from(),
            inputs.transfer_to(),
            inputs.transfer_value().unwrap_or(U256::ZERO),
        )),
    }
}

/// Deducts a gas cost from a frame's gas limit. Returns `Some(oog_result)` if the
/// frame has insufficient gas, `None` on success.
fn deduct_gas_from_frame(frame_input: &mut FrameInit, cost: u64) -> Option<FrameResult> {
    match &mut frame_input.frame_input {
        FrameInput::Call(inputs) => {
            if inputs.gas_limit < cost {
                return Some(create_oog_frame_result(frame_input));
            }
            inputs.gas_limit -= cost;
        }
        FrameInput::Create(inputs) => {
            if inputs.gas_limit() < cost {
                return Some(create_oog_frame_result(frame_input));
            }
            inputs.set_gas_limit(inputs.gas_limit() - cost);
        }
        FrameInput::Empty => {}
    }
    None
}

/// Defensive revert for when a subcall interception fires on a non-Call frame.
/// This should never happen (the caller in `frame_init` checks for Call), but
/// avoids panicking in production.
fn non_call_frame_revert<F, E>(frame_input: &FrameInit) -> Result<ItemOrResult<F, FrameResult>, E> {
    Ok(ItemOrResult::Result(create_frame_result(
        frame_input,
        "internal error: subcall interception on non-Call frame",
        0,
    )))
}

/// Resolves any `SharedBuffer` call input to concrete `Bytes`.
///
/// # Warning
///
/// Resolve the buffer before calling any child frames that could overwrite it.
///
/// Precompiles receive `CallInputs` but cannot dereference `SharedBuffer` references
/// because they lack access to the EVM context. This must be called before dispatching
/// to a subcall precompile's `init_subcall` method.
fn resolve_shared_buffer(ctx: &impl ContextTr, frame_input: &mut FrameInit) {
    if let FrameInput::Call(ref mut inputs) = frame_input.frame_input {
        if let revm::interpreter::interpreter_action::CallInput::SharedBuffer(_) = &inputs.input {
            let resolved = inputs.input.bytes(ctx);
            inputs.input = revm::interpreter::interpreter_action::CallInput::Bytes(resolved);
        }
    }
}

/// Returns whether a transfer log should be emitted for the given frame init result.
///
/// A log is kept when the call/create completed successfully.
/// CREATE results that succeed but have no address (nonce overflow) are excluded because
/// value was not actually transferred.
fn should_emit_transfer_log_for_result(result: &FrameResult) -> bool {
    match result {
        FrameResult::Create(create_outcome) => {
            create_outcome.instruction_result().is_ok() && create_outcome.address.is_some()
        }
        FrameResult::Call(call_outcome) => call_outcome.instruction_result().is_ok(),
    }
}

/// Returns whether a transfer log should be emitted for the given frame init outcome.
///
/// For `Item` (pending execution), always emit — REVM's internal frame checkpoint will
/// revert it if the frame later fails. For `Result` (synchronous completion), delegate
/// to [`should_emit_transfer_log_for_result`].
fn should_emit_transfer_log<T>(frame_res: &ItemOrResult<T, FrameResult>) -> bool {
    match frame_res {
        ItemOrResult::Item(_) => true,
        ItemOrResult::Result(result) => should_emit_transfer_log_for_result(result),
    }
}

/// Returns `true` if `frame_input` targets a precompile address.
///
/// Only CALL frames can target precompiles; CREATE never does.
fn is_precompile_call<CTX: ContextTr>(
    frame_input: &FrameInit,
    precompiles: &impl PrecompileProvider<CTX, Output = InterpreterResult>,
) -> bool {
    match &frame_input.frame_input {
        FrameInput::Call(inputs) => precompiles.contains(&inputs.bytecode_address),
        _ => false,
    }
}

/// Outcome of [`ArcEvm::checked_frame_init`].
///
/// Returning owned data (rather than a `FrameInitResult` reference) resolves borrow-checker
/// conflicts: the caller can reborrow individual fields of `self` after the method returns.
enum FrameInitOutcome {
    /// Frame was pushed onto the stack; the execution loop will drive it.
    Pushed,
    /// Frame completed immediately (e.g. empty bytecode, allowlist rejection).
    Immediate(FrameResult),
}

/// Mirrors `Evm::frame_init` from revm-handler, but takes individual field references instead
/// of `&mut self`. This enables borrow splitting at the call site: the returned reference borrows
/// only `frame_stack`, leaving `ctx` and other fields accessible.
///
/// See [`revm::handler::EvmTr::frame_init`] for the original.
fn init_frame<'fs, CTX, P>(
    frame_stack: &'fs mut FrameStack<EthFrame<EthInterpreter>>,
    ctx: &mut CTX,
    precompiles: &mut P,
    frame_input: FrameInit,
) -> Result<FrameInitResult<'fs, EthFrame<EthInterpreter>>, ContextDbError<CTX>>
where
    CTX: ContextTr,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    let is_first_init = frame_stack.index().is_none();
    let new_frame = if is_first_init {
        frame_stack.start_init()
    } else {
        frame_stack.get_next()
    };

    let res = EthFrame::init_with_context(new_frame, ctx, precompiles, frame_input)?;

    Ok(res.map_frame(|token| {
        if is_first_init {
            unsafe { frame_stack.end_init(token) };
        } else {
            unsafe { frame_stack.push(token) };
        }
        frame_stack.get()
    }))
}

/// ArcEvm implementation for customized operations.
impl<CTX: ContextTr, INSP, I, P> ArcEvm<CTX, INSP, I, P> {
    /// Checks if an address is blocklisted by reading from the native coin control precompile storage.
    ///
    /// Returns `(is_blocklisted, is_cold)` where `is_cold` indicates whether the storage slot
    /// was a cold access (EIP-2929). Callers use `is_cold` to compute the correct SLOAD gas cost:
    /// cold access costs 2100 gas, warm access costs 100 gas.
    ///
    /// Note: This calls `journal.sload()` directly, bypassing the interpreter, so revm does not
    /// automatically meter gas for these reads. Gas is accounted for externally:
    /// - Depth 0: `validate_initial_tx_gas` includes the SLOAD cost in intrinsic gas (always cold).
    /// - Depth > 0: `frame_init` deducts the warm/cold-aware SLOAD cost from the child frame's gas limit.
    fn is_address_blocklisted(
        &mut self,
        address: Address,
    ) -> Result<(bool, bool), ContextDbError<CTX>> {
        let storage_slot = compute_is_blocklisted_storage_slot(address).into();

        // Read blocklist status: non-zero value means blocklisted
        let state_load = self
            .inner
            .ctx
            .journal_mut()
            .sload(NATIVE_COIN_CONTROL_ADDRESS, storage_slot)?;

        Ok((is_blocklisted_status(state_load.data), state_load.is_cold))
    }

    /// Extracts transfer parameters (from, to, amount) from a Create frame input.
    /// Returns None if value is zero or scheme is Custom.
    fn extract_create_transfer_params(
        &mut self,
        inputs: &revm_interpreter::CreateInputs,
        depth: usize,
    ) -> Result<Option<(Address, Address, U256)>, ContextDbError<CTX>> {
        if inputs.value().is_zero() {
            return Ok(None);
        }

        match inputs.scheme() {
            CreateScheme::Create => {
                let nonce = if depth == 0 {
                    // First frame: use transaction nonce directly
                    self.inner.ctx.tx().nonce()
                } else {
                    // Nested frame: look up nonce in journal
                    self.inner
                        .ctx
                        .journal_mut()
                        .load_account(inputs.caller())?
                        .info
                        .nonce
                };
                Ok(Some((
                    inputs.caller(),
                    inputs.created_address(nonce),
                    inputs.value(),
                )))
            }
            CreateScheme::Create2 { salt: _ } => {
                // Nonce doesn't matter for CREATE2
                Ok(Some((
                    inputs.caller(),
                    inputs.created_address(0),
                    inputs.value(),
                )))
            }
            CreateScheme::Custom { address: _ } => Ok(None),
        }
    }

    /// Checks blocklist status for transfer participants and returns appropriate result.
    ///
    /// Computes the total SLOAD gas cost for the blocklist checks performed. For Zero6+,
    /// uses EIP-2929 warm/cold pricing via `sload_cost`. For pre-Zero6, uses the fixed
    /// `PRECOMPILE_SLOAD_GAS_COST` (2100 gas) per SLOAD.
    fn check_blocklist_and_create_log(
        &mut self,
        from: Address,
        to: Address,
        amount: U256,
        frame_input: &FrameInit,
    ) -> Result<BeforeFrameInitResult, ContextDbError<CTX>> {
        // Zero5: reject CALL/CREATE value transfers involving the zero address.
        // This prevents accidental burn/mint semantics at the EVM execution layer.
        //
        // Note: this check only applies to CALL/CREATE frame value transfers — it does NOT
        // affect the NativeCoinAuthority precompile, which legitimately uses Address::ZERO in
        // ERC-20 Transfer events for mint (from=0x0) and burn (to=0x0). The precompile operates
        // via direct journal balance mutations within its own frame, never triggering frame_init.
        if self.hardfork_flags.is_active(ArcHardfork::Zero5)
            && (from == Address::ZERO || to == Address::ZERO)
        {
            return Ok(BeforeFrameInitResult::Reverted(create_frame_result(
                frame_input,
                ERR_ZERO_ADDRESS,
                BLOCKLISTED_GAS_PENALTY,
            )));
        }

        let (from_blocklisted, from_is_cold) = self.is_address_blocklisted(from)?;

        // Compute gas cost for the `from` sload
        let from_sload_cost = if self.hardfork_flags.is_active(ArcHardfork::Zero6) {
            if from_is_cold {
                revm_interpreter::gas::COLD_SLOAD_COST
            } else {
                revm_interpreter::gas::WARM_STORAGE_READ_COST
            }
        } else {
            PRECOMPILE_SLOAD_GAS_COST
        };

        if from_blocklisted {
            // Short-circuit: only the `from` sload was performed
            return Ok(BeforeFrameInitResult::Reverted(
                create_blocklisted_frame_result(frame_input),
            ));
        }

        let (to_blocklisted, to_is_cold) = self.is_address_blocklisted(to)?;

        // Compute gas cost for the `to` sload
        let to_sload_cost = if self.hardfork_flags.is_active(ArcHardfork::Zero6) {
            if to_is_cold {
                revm_interpreter::gas::COLD_SLOAD_COST
            } else {
                revm_interpreter::gas::WARM_STORAGE_READ_COST
            }
        } else {
            PRECOMPILE_SLOAD_GAS_COST
        };

        let total_sload_cost = from_sload_cost + to_sload_cost;

        if to_blocklisted {
            return Ok(BeforeFrameInitResult::Reverted(
                create_blocklisted_frame_result(frame_input),
            ));
        }
        if self.hardfork_flags.is_active(ArcHardfork::Zero5) {
            // Note: this selfdestructed-target check uses an unmetered `load_account(to)`.
            // In the common value-transfer path REVM will touch the same target again during
            // execution, so this usually has no practical gas impact even if the first load is cold.
            let target_account = self.inner.journal_mut().load_account(to)?;
            if target_account.is_selfdestructed() {
                return Ok(BeforeFrameInitResult::Reverted(create_frame_result(
                    frame_input,
                    ERR_SELFDESTRUCTED_BALANCE_INCREASED,
                    BLOCKLISTED_GAS_PENALTY,
                )));
            }
        }

        if self.hardfork_flags.is_active(ArcHardfork::Zero5) {
            if from == to {
                // EIP-7708: self-transfers do not emit a log
                Ok(BeforeFrameInitResult::Checked(total_sload_cost))
            } else {
                Ok(BeforeFrameInitResult::Log(
                    create_eip7708_transfer_log(from, to, amount),
                    total_sload_cost,
                ))
            }
        } else {
            Ok(BeforeFrameInitResult::Log(
                create_native_transfer_log(from, to, amount),
                total_sload_cost,
            ))
        }
    }

    pub(crate) fn before_frame_init(
        &mut self,
        frame_input: &FrameInit,
    ) -> Result<BeforeFrameInitResult, ContextDbError<CTX>> {
        // Extract transfer parameters based on frame type
        let transfer_params = match &frame_input.frame_input {
            FrameInput::Empty => None,
            FrameInput::Create(inputs) => {
                self.extract_create_transfer_params(inputs, frame_input.depth)?
            }
            FrameInput::Call(inputs) => extract_call_transfer_params(inputs),
        };

        // Process transfer if present and non-zero
        match transfer_params {
            Some((from, to, amount)) if !amount.is_zero() => {
                self.check_blocklist_and_create_log(from, to, amount, frame_input)
            }
            _ => Ok(BeforeFrameInitResult::None),
        }
    }
}

// Implement EvmTr for ArcEvm
// ref: op-revm v12.0.1 implementation https://github.com/bluealloy/revm/blob/v97/crates/op-revm/src/evm.rs#L95
impl<CTX, INSP, I, P> EvmTr for ArcEvm<CTX, INSP, I, P>
where
    CTX: ContextTr,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    type Context = CTX;
    type Instructions = I;
    type Precompiles = P;
    type Frame = EthFrame<EthInterpreter>;

    #[inline]
    fn all(
        &self,
    ) -> (
        &Self::Context,
        &Self::Instructions,
        &Self::Precompiles,
        &FrameStack<Self::Frame>,
    ) {
        self.inner.all()
    }

    #[inline]
    fn all_mut(
        &mut self,
    ) -> (
        &mut Self::Context,
        &mut Self::Instructions,
        &mut Self::Precompiles,
        &mut FrameStack<Self::Frame>,
    ) {
        self.inner.all_mut()
    }

    /// Initializes the frame for the given frame input. Frame is pushed to the frame stack.
    #[inline]
    fn frame_init(
        &mut self,
        frame_input: <EthFrame as FrameTr>::FrameInit,
    ) -> Result<FrameInitResult<'_, Self::Frame>, ContextDbError<CTX>> {
        // Subcall precompiles must be checked first: they reject value transfers, so
        // the parent's `before_frame_init` is a no-op (no transfer to check). Running
        // `checked_frame_init` for the parent would incorrectly push a frame for the
        // precompile address onto the stack.
        if let FrameInput::Call(ref call_inputs) = frame_input.frame_input {
            if let Some((precompile, allowed_callers)) =
                self.subcall_registry.get(&call_inputs.target_address)
            {
                if !allowed_callers.is_allowed(&call_inputs.caller) {
                    return Ok(ItemOrResult::Result(init_subcall_revert(
                        "unauthorized caller",
                        call_inputs,
                    )));
                }

                // Clone precompile to release the immutable borrow on self.subcall_registry
                // before taking &mut self for before_frame_init.
                let precompile = precompile.clone();

                // Defense-in-depth: run before_frame_init on the parent frame even though
                // subcall precompiles currently reject value transfers (making this a no-op).
                // If a future subcall precompile allows value, the log needs to be handled here.
                match self.before_frame_init(&frame_input)? {
                    BeforeFrameInitResult::Reverted(res) => {
                        return Ok(ItemOrResult::Result(res));
                    }
                    BeforeFrameInitResult::Log(..)
                    | BeforeFrameInitResult::Checked(_)
                    | BeforeFrameInitResult::None => {}
                }

                return self.init_subcall(frame_input, precompile);
            }
        }

        // Standard path: blocklist checks, SLOAD gas, revm frame init, transfer log.
        match self.checked_frame_init(frame_input)? {
            FrameInitOutcome::Pushed => Ok(ItemOrResult::Item(self.inner.frame_stack.get())),
            FrameInitOutcome::Immediate(result) => Ok(ItemOrResult::Result(result)),
        }
    }

    /// Run the frame from the top of the stack. Returns the frame init or result.
    #[inline]
    fn frame_run(&mut self) -> Result<FrameInitOrResult<EthFrame>, ContextDbError<CTX>> {
        self.inner.frame_run()
    }

    /// Returns the result of the frame to the caller. Frame is popped from the frame stack.
    ///
    /// Overrides the default revm behavior to intercept subcall continuations:
    /// when a child frame completes and a continuation exists at `depth - 1`,
    /// we finalize the subcall via `complete_subcall` instead of returning the raw child result.
    #[inline]
    fn frame_return_result(
        &mut self,
        result: <Self::Frame as FrameTr>::FrameResult,
    ) -> Result<Option<<Self::Frame as FrameTr>::FrameResult>, ContextDbError<Self::Context>> {
        let frame_was_finished = self.inner.frame_stack.get().is_finished();

        // Capture the finished frame's depth before popping — needed for subcall continuation
        // lookup when the pop leaves the stack empty (direct EOA -> precompile calls).
        let finished_depth = self.inner.frame_stack.get().depth;

        // Pop the finished frame (revm default behavior)
        if frame_was_finished {
            self.inner.frame_stack.pop();
        }

        let stack_empty = self.inner.frame_stack.index().is_none();

        // Check for a subcall continuation, but ONLY when a frame actually finished execution.
        //
        // When `frame_was_finished` is true, this result came from a child frame that ran to
        // completion. The continuation was stored at the precompile's depth (finished_depth - 1).
        //
        // When `frame_was_finished` is false, this is an immediate result from `frame_init`
        // (e.g., init_subcall already ran complete_subcall for a CallTooDeep).
        // The stack top is the still-running parent frame, and `finished_depth` is the parent's
        // depth — not the child's. Looking up `finished_depth - 1` would match the grandparent's
        // continuation, corrupting state.
        if frame_was_finished {
            let continuation_key = finished_depth.checked_sub(1);
            if let Some(key) = continuation_key {
                if let Some(continuation) = self.subcall_continuations.remove(&key) {
                    let final_result = Self::complete_subcall(result, continuation)?;
                    if stack_empty {
                        // Direct EOA -> precompile: no parent frame to propagate to.
                        return Ok(Some(final_result));
                    }
                    // Propagate to the parent frame
                    self.inner
                        .frame_stack
                        .get()
                        .return_result::<_, ContextDbError<CTX>>(
                            &mut self.inner.ctx,
                            final_result,
                        )?;
                    return Ok(None);
                }
            }
        }

        // If stack is empty, this is the top-level result — return it
        if stack_empty {
            return Ok(Some(result));
        }

        // Propagate to the parent frame. This covers:
        // - Normal (non-subcall) frame returns
        // - Immediate results from frame_init (complete_subcall already ran)
        self.inner
            .frame_stack
            .get()
            .return_result::<_, ContextDbError<CTX>>(&mut self.inner.ctx, result)?;
        Ok(None)
    }
}

/// Subcall interception methods and shared frame-stack helpers.
///
/// These are separated from the `EvmTr` impl because they are private helper methods,
/// not trait methods. They share the same trait bounds as the `EvmTr` impl.
impl<CTX, INSP, I, P> ArcEvm<CTX, INSP, I, P>
where
    CTX: ContextTr,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
{
    /// Runs `before_frame_init` (blocklist checks, transfer log), deducts SLOAD gas for
    /// nested frames with Zero6 active, then initializes the frame via revm's standard
    /// machinery and emits the transfer log on success.
    ///
    /// For Zero5+ precompile CALLs, the EIP-7708 Transfer log is pushed before frame init
    /// (wrapped in a journal checkpoint) so it precedes precompile-emitted logs. For all
    /// other cases, the log is pushed after frame init only on success.
    ///
    /// Returns an owned [`FrameInitOutcome`] so the caller can reborrow `self` fields
    /// after the call without borrow-checker conflicts.
    fn checked_frame_init(
        &mut self,
        mut frame_input: FrameInit,
    ) -> Result<FrameInitOutcome, ContextDbError<CTX>> {
        let (maybe_log, sload_gas) = match self.before_frame_init(&frame_input)? {
            BeforeFrameInitResult::Reverted(res) => {
                return Ok(FrameInitOutcome::Immediate(res));
            }
            BeforeFrameInitResult::Log(log, gas) => (Some(log), gas),
            BeforeFrameInitResult::Checked(gas) => (None, gas),
            BeforeFrameInitResult::None => (None, 0),
        };

        // Deduct warm/cold-aware SLOAD gas for nested frames (depth > 0) with Zero6 active.
        // Depth 0 is covered by `validate_initial_tx_gas` which charges fixed cold SLOAD costs.
        if frame_input.depth > 0
            && sload_gas > 0
            && self.hardfork_flags.is_active(ArcHardfork::Zero6)
        {
            if let Some(oog) = deduct_gas_from_frame(&mut frame_input, sload_gas) {
                return Ok(FrameInitOutcome::Immediate(oog));
            }
        }

        // Log emission strategy for the EIP-7708 Transfer log:
        //
        // **Zero5+ precompile CALL**: Push the Transfer log BEFORE init_frame so it
        // precedes any logs the precompile emits (EIP-7708 requires the native Transfer
        // log to appear before precompile-emitted logs). The precompile runs synchronously
        // inside init_with_context and returns a Result, so a journal checkpoint can
        // correctly commit/revert based on the outcome.
        //
        // **All other cases** (non-precompile CALLs/CREATEs at any hardfork, and pre-Zero5
        // precompile CALLs): Execute first, then push the log only if the result indicates
        // success. For non-precompile frames that return Item (pending execution), REVM's
        // internal frame checkpoint already covers this log — if the frame later reverts,
        // logs.truncate(log_i) removes it automatically.
        let is_precompile = is_precompile_call(&frame_input, &self.inner.precompiles);

        let frame_res = if is_precompile && self.hardfork_flags.is_active(ArcHardfork::Zero5) {
            // Zero5+ precompile path: push the Transfer log BEFORE init_frame so it
            // precedes any logs the precompile emits, wrapped in a journal checkpoint so
            // the log is reverted if the precompile fails.
            let log_checkpoint = if let Some(log) = maybe_log {
                let cp = self.inner.ctx.journal_mut().checkpoint();
                self.inner.ctx.journal_mut().log(log);
                Some(cp)
            } else {
                None
            };

            let res = init_frame(
                &mut self.inner.frame_stack,
                &mut self.inner.ctx,
                &mut self.inner.precompiles,
                frame_input,
            )?;

            if let Some(cp) = log_checkpoint {
                if should_emit_transfer_log(&res) {
                    self.inner.ctx.journal_mut().checkpoint_commit();
                } else {
                    self.inner.ctx.journal_mut().checkpoint_revert(cp);
                }
            }

            res
        } else {
            // Common path: execute first, then push the log only if successful.
            let res = init_frame(
                &mut self.inner.frame_stack,
                &mut self.inner.ctx,
                &mut self.inner.precompiles,
                frame_input,
            )?;

            if let Some(log) = maybe_log {
                if should_emit_transfer_log(&res) {
                    self.inner.ctx.journal_mut().log(log);
                }
            }

            res
        };

        match frame_res {
            ItemOrResult::Item(_) => Ok(FrameInitOutcome::Pushed),
            ItemOrResult::Result(result) => Ok(FrameInitOutcome::Immediate(result)),
        }
    }

    /// Intercept a call to a subcall-capable precompile and initialize the child frame.
    ///
    /// Decodes the precompile input via [`SubcallPrecompile::init_subcall`], stores a
    /// continuation, and initializes the child frame via [`checked_frame_init`] so that it
    /// goes through `before_frame_init` hooks (blocklist checks, transfer log, SLOAD gas
    /// deduction).
    fn init_subcall(
        &mut self,
        mut frame_input: <EthFrame as FrameTr>::FrameInit,
        precompile: Arc<dyn SubcallPrecompile>,
    ) -> Result<FrameInitResult<'_, EthFrame<EthInterpreter>>, ContextDbError<CTX>> {
        let call_inputs = match &frame_input.frame_input {
            FrameInput::Call(inputs) => inputs.as_ref(),
            _ => return non_call_frame_revert(&frame_input),
        };

        // Reject non-CALL schemes (DELEGATECALL, STATICCALL, CALLCODE).
        // Subcall precompiles only support the CALL scheme — other schemes have
        // incompatible semantics (e.g. DELEGATECALL runs code in the caller's
        // context, STATICCALL prohibits state changes).
        if call_inputs.scheme != CallScheme::Call {
            return Ok(ItemOrResult::Result(init_subcall_revert(
                "subcall precompiles only support CALL scheme",
                call_inputs,
            )));
        }

        // Reject static context — even when the scheme is CALL, `is_static` can be true
        // if this call is nested inside a STATICCALL frame higher in the call stack.
        if call_inputs.is_static {
            return Ok(ItemOrResult::Result(init_subcall_revert(
                "subcall precompiles cannot be invoked in static context",
                call_inputs,
            )));
        }

        // Reject calls with value attached. The subcall framework intercepts frame_init,
        // bypassing revm's init_with_context for the precompile call itself, so the value
        // transfer from caller → precompile address is never executed. Forwarding value to
        // the child would require explicit transfer logic that is not yet implemented.
        if call_inputs.transfers_value() {
            return Ok(ItemOrResult::Result(init_subcall_revert(
                "subcall precompiles do not support value transfers",
                call_inputs,
            )));
        }

        // Resolve SharedBuffer → Bytes before handing inputs to the precompile, which
        // doesn't have access to the EVM context needed to dereference shared memory.
        resolve_shared_buffer(&self.inner.ctx, &mut frame_input);

        // Re-extract call_inputs after the mutable borrow in resolve_shared_buffer.
        let call_inputs = match &frame_input.frame_input {
            FrameInput::Call(inputs) => inputs.as_ref(),
            _ => return non_call_frame_revert(&frame_input),
        };

        let init_result = match precompile.init_subcall(call_inputs) {
            Ok(result) => result,
            Err(err) => {
                return Ok(ItemOrResult::Result(init_subcall_revert(
                    &err.to_string(),
                    call_inputs,
                )));
            }
        };

        let return_memory_offset = call_inputs.return_memory_offset.clone();

        // Store continuation keyed by the precompile call's depth.
        //
        // No journal checkpoint is taken here. The child frame's own checkpoint
        // (created by `make_call_frame`) handles commit/revert based on the child's
        // success or failure. This avoids an extra `journal.depth` increment that
        // would create a depth gap visible to tracing inspectors, causing
        // `push_trace` to panic with "Disconnected trace".
        //
        // Note: if `complete_subcall` returns `success: false` or `Err` when the
        // child succeeded, the child's committed state will NOT be reverted.
        // See the `SubcallPrecompile` trait docs.
        let depth = frame_input.depth;
        self.subcall_continuations.insert(
            depth,
            SubcallContinuation {
                precompile,
                gas_limit: call_inputs.gas_limit,
                init_subcall_gas_overhead: init_result.gas_overhead,
                return_memory_offset,
                continuation_data: init_result.continuation_data,
            },
        );

        // Construct child frame input
        let child_inputs = &init_result.child_inputs;

        // Pre-load the child's caller and target accounts into the journal. The normal EVM
        // execution path has these already loaded (caller is the executing frame, target was
        // loaded by the CALL opcode handler), but we're constructing a synthetic child frame
        // with a potentially-unseen spoofed sender and arbitrary target.
        //
        // Both must be present in the journal state map before `make_call_frame` calls
        // `transfer_loaded`, which panics on missing accounts. `load_account` (without code)
        // is sufficient here — `make_call_frame` will call `load_account_with_code` for the
        // target's bytecode separately (since `known_bytecode` is None).
        //
        // Side effect: these `load_account` calls create `AccountWarmed` journal entries
        // outside the child frame's checkpoint scope. If the child reverts, the child's
        // journal entries are rolled back but these pre-loads persist — the addresses stay
        // warm for the rest of the transaction. For CallFrom this is bounded by the
        // allowlist and the addresses are typically already warm. Future subcall precompiles
        // with `AllowedCallers::Unrestricted` targeting user-supplied addresses should
        // account for this.
        self.inner
            .ctx
            .journal_mut()
            .load_account(child_inputs.caller)?;
        self.inner
            .ctx
            .journal_mut()
            .load_account(child_inputs.target_address)?;

        let child_frame_input = FrameInit {
            depth: depth + 1,
            memory: frame_input.memory,
            frame_input: FrameInput::Call(init_result.child_inputs),
        };

        // Initialize the child frame through checked_frame_init so that
        // before_frame_init hooks (blocklist, transfer log, SLOAD gas) run on the
        // child. This returns owned FrameInitOutcome, releasing &mut self.
        match self.checked_frame_init(child_frame_input)? {
            // Child frame needs execution — return a reference so the execution loop
            // drives it. complete_subcall will run in frame_return_result.
            FrameInitOutcome::Pushed => Ok(ItemOrResult::Item(self.inner.frame_stack.get())),
            // Child completed immediately (e.g. rejected by allowlist, empty bytecode).
            // complete_subcall must run now since frame_return_result won't see this child.
            FrameInitOutcome::Immediate(child_result) => {
                let Some(continuation) = self.subcall_continuations.remove(&depth) else {
                    // Defensive: the continuation was inserted a few lines above. If somehow
                    // missing, revert — do not panic in production.
                    return Ok(ItemOrResult::Result(init_subcall_revert(
                        "internal error: missing subcall continuation",
                        call_inputs,
                    )));
                };
                let final_result = Self::complete_subcall(child_result, continuation)?;
                Ok(ItemOrResult::Result(final_result))
            }
        }
    }

    /// The child frame has completed. Finalize the precompile result via
    /// [`SubcallPrecompile::complete_subcall`].
    ///
    /// Computes the final `FrameResult` from the child outcome and continuation state.
    /// The caller is responsible for propagating or returning this result.
    fn complete_subcall(
        child_result: FrameResult,
        continuation: SubcallContinuation,
    ) -> Result<FrameResult, ContextDbError<CTX>> {
        let child_gas = child_result.gas();

        // Classify the child frame's execution result.
        //
        // `child_succeeded`: the child returned normally (Stop, Return, SelfDestruct).
        //   Used to gate SSTORE refund forwarding — reverted children's refunds are invalid.
        //
        // `child_halted`: the child hit an error (OOG, StackUnderflow, etc.) — anything
        //   that is NOT ok and NOT a revert.
        //   In normal EVM CALL semantics, a halted child consumes the entire gas allocation
        //   including the retained 1/64th (revm's `is_ok_or_revert()` check gates
        //   `erase_cost`). We mirror that here: when the child halted, the full gas_limit
        //   is reported as spent.
        let (child_succeeded, child_halted) = match &child_result {
            FrameResult::Call(outcome) => {
                let r = outcome.result.result;
                (r.is_ok(), !r.is_ok_or_revert())
            }
            _ => (false, true),
        };

        let completion_result = continuation
            .precompile
            .complete_subcall(continuation.continuation_data, &child_result);

        // Total gas consumed by the precompile.
        //
        // Normal case: init_subcall overhead + child execution cost. The remainder
        // (gas_limit - gas_used) includes the retained 1/64th implicitly, since it
        // was never forwarded to the child.
        //
        // Child halted (OOG, StackUnderflow, etc.): the entire gas_limit is consumed,
        // matching EVM CALL semantics where a halted child burns the full allocation
        // including the retained 1/64th.
        let gas_used = if child_halted {
            continuation.gas_limit
        } else {
            continuation
                .init_subcall_gas_overhead
                .saturating_add(child_gas.spent())
        };

        // No journal checkpoint to commit/revert here. The child frame's own checkpoint
        // (taken by `make_call_frame`, resolved in `process_next_action`) already
        // committed or reverted based on the child's success/failure. We rely on
        // `complete_subcall` not rejecting a successful child — see
        // `SubcallPrecompile` trait docs.
        match completion_result {
            Ok(result) if result.success => {
                let mut gas = Gas::new(continuation.gas_limit);
                if !gas.record_cost(gas_used) {
                    gas.spend_all();
                }
                // Only forward SSTORE refunds when the child frame itself succeeded.
                // A reverted child's refunds are invalid — they correspond to state
                // changes that were rolled back.
                if child_succeeded {
                    gas.record_refund(child_gas.refunded());
                }

                Ok(FrameResult::Call(CallOutcome {
                    result: InterpreterResult::new(InstructionResult::Return, result.output, gas),
                    memory_offset: continuation.return_memory_offset,
                    was_precompile_called: true,
                    precompile_call_logs: Default::default(),
                }))
            }
            Ok(result) => {
                // Precompile signaled failure — no refund. Note: if the child succeeded,
                // its committed state is NOT reverted (see SubcallPrecompile trait docs).

                let mut gas = Gas::new(continuation.gas_limit);
                if !gas.record_cost(gas_used) {
                    gas.spend_all();
                }
                // No record_refund — precompile signaled failure

                Ok(FrameResult::Call(CallOutcome {
                    result: InterpreterResult::new(InstructionResult::Revert, result.output, gas),
                    memory_offset: continuation.return_memory_offset,
                    was_precompile_called: true,
                    precompile_call_logs: Default::default(),
                }))
            }
            Err(_) => {
                // complete_subcall error — all gas consumed
                Ok(FrameResult::Call(CallOutcome {
                    result: InterpreterResult::new(
                        InstructionResult::Revert,
                        Bytes::new(),
                        Gas::new_spent(continuation.gas_limit),
                    ),
                    memory_offset: continuation.return_memory_offset,
                    was_precompile_called: true,
                    precompile_call_logs: Default::default(),
                }))
            }
        }
    }
}

// Implement InspectorEvmTr for ArcEvm
// ref: op-revm v12.0.1 implementation https://github.com/bluealloy/revm/blob/v97/crates/op-revm/src/evm.rs#L59
impl<CTX, INSP, I, P> InspectorEvmTr for ArcEvm<CTX, INSP, I, P>
where
    CTX: ContextTr<Journal: JournalExt> + ContextSetters,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
    INSP: Inspector<CTX, I::InterpreterTypes>,
{
    type Inspector = INSP;

    #[inline]
    fn all_inspector(
        &self,
    ) -> (
        &Self::Context,
        &Self::Instructions,
        &Self::Precompiles,
        &FrameStack<Self::Frame>,
        &Self::Inspector,
    ) {
        self.inner.all_inspector()
    }

    #[inline]
    fn all_mut_inspector(
        &mut self,
    ) -> (
        &mut Self::Context,
        &mut Self::Instructions,
        &mut Self::Precompiles,
        &mut FrameStack<Self::Frame>,
        &mut Self::Inspector,
    ) {
        self.inner.all_mut_inspector()
    }

    /// Override `inspect_frame_init` to make subcall precompiles transparent in traces.
    ///
    /// For subcall precompiles (e.g. CallFrom): uses [`SubcallPrecompile::trace_child_call`]
    /// to obtain the child's `CallInputs`, then passes them to [`ArcEvm::inspect_frame_init_impl`]
    /// so the trace node shows the logical child call
    /// (spoofed_sender → target) instead of the precompile address.
    ///
    /// For non-subcall calls: delegates to the default `inspect_frame_init_impl(frame_init, None)`
    /// (equivalent to upstream revm's `inspect_frame_init`).
    fn inspect_frame_init(
        &mut self,
        mut frame_init: <Self::Frame as FrameTr>::FrameInit,
    ) -> Result<FrameInitResult<'_, Self::Frame>, ContextDbError<Self::Context>> {
        // Check if this targets a subcall precompile.
        let is_subcall = matches!(
            &frame_init.frame_input,
            FrameInput::Call(c) if self.subcall_registry.get(&c.target_address).is_some()
        );

        if !is_subcall {
            // Pass `None`, so inspect_frame_init_impl behaves like upstream revm.
            // We cannot call self.inner.inspect_frame_init because it calls InnerEvm::frame_init
            // instead of ArcEvm::frame_init.
            return self.inspect_frame_init_impl(frame_init, None);
        }

        // Resolve SharedBuffer so trace_child_call can read the calldata bytes.
        resolve_shared_buffer(&self.inner.ctx, &mut frame_init);

        let FrameInput::Call(call_inputs) = &frame_init.frame_input else {
            debug_assert!(false, "is_subcall matched but frame_input is not Call");
            return self.inspect_frame_init_impl(frame_init, None);
        };

        // Fall back to the original input if trace_child_call returns None
        // (e.g. malformed calldata that will fail during actual execution).
        let trace_frame_input = self
            .subcall_registry
            .get(&call_inputs.target_address)
            .and_then(|(precompile, _)| precompile.trace_child_call(call_inputs))
            .map(|inputs| FrameInput::Call(Box::new(inputs)))
            .unwrap_or_else(|| frame_init.frame_input.clone());

        self.inspect_frame_init_impl(frame_init, Some(trace_frame_input))
    }
}

/// Mirrors the default [`InspectorEvmTr::inspect_frame_init`] from revm-inspector v15.0.0
/// (`revm::inspector::traits`), but routes through `ArcEvm::frame_init` (not the inner
/// `InnerEvm::frame_init`) so Arc-specific logic (blocklist checks, EIP-7708 logs, subcall
/// routing) is always applied.
///
/// When `trace_override` is `Some`, `frame_start`/`frame_end` use the provided input for
/// the inspector trace identity (e.g. showing the logical child call instead of the
/// precompile address). Inspector `call()` mutations are isolated from execution.
///
/// When `trace_override` is `None`, `frame_start` receives `&mut frame_init.frame_input`
/// directly, matching upstream semantics where inspector `call()` mutations flow through
/// to execution.
///
/// Keep in sync with: <https://github.com/bluealloy/revm/blob/v103/crates/inspector/src/traits.rs#L98-L137>
/// (revm crate v34.0.0 — verify this function if upgrading revm)
impl<CTX, INSP, I, P> ArcEvm<CTX, INSP, I, P>
where
    CTX: ContextTr<Journal: JournalExt> + ContextSetters,
    I: InstructionProvider<Context = CTX, InterpreterTypes = EthInterpreter>,
    P: PrecompileProvider<CTX, Output = InterpreterResult>,
    INSP: Inspector<CTX, I::InterpreterTypes>,
{
    /// Resolve the trace identity for `frame_start`/`frame_end`.
    ///
    /// When `trace_override` is `Some`, `frame_start` uses a separate identity (subcall
    /// precompile transparency — inspector mutations isolated from execution).
    /// When `None`, `frame_start` mutates `frame_init.frame_input` directly so inspector
    /// `call()` mutations flow through to execution (upstream semantics).
    ///
    /// Returns `Ok(trace_input)` to continue execution, or `Err(result)` for early exit
    /// when `frame_start` produces a result (e.g. inspector-driven revert).
    fn frame_start_with_trace(
        &mut self,
        frame_init: &mut FrameInit,
        trace_override: Option<FrameInput>,
    ) -> Result<FrameInput, Box<FrameResult>> {
        use revm::inspector::handler::{frame_end, frame_start};

        let (ctx, inspector) = self.ctx_inspector();
        match trace_override {
            Some(mut trace_input) => {
                if let Some(mut output) = frame_start(ctx, inspector, &mut trace_input) {
                    frame_end(ctx, inspector, &trace_input, &mut output);
                    return Err(Box::new(output));
                }
                Ok(trace_input)
            }
            // This branch mirrors upstream revm's inspect_frame_init.
            None => {
                if let Some(mut output) = frame_start(ctx, inspector, &mut frame_init.frame_input) {
                    frame_end(ctx, inspector, &frame_init.frame_input, &mut output);
                    return Err(Box::new(output));
                }
                // Clone after frame_start may have mutated it — frame_init is consumed
                // by move below, but frame_end still needs the trace input.
                Ok(frame_init.frame_input.clone())
            }
        }
    }

    fn inspect_frame_init_impl(
        &mut self,
        mut frame_init: FrameInit,
        trace_override: Option<FrameInput>,
    ) -> Result<FrameInitResult<'_, EthFrame<EthInterpreter>>, ContextDbError<CTX>> {
        use revm::inspector::handler::frame_end;
        use revm::inspector::InspectorFrame;

        let trace_input_for_end = match self.frame_start_with_trace(&mut frame_init, trace_override)
        {
            Ok(input) => input,
            Err(output) => return Ok(ItemOrResult::Result(*output)),
        };

        let (ctx, _) = self.ctx_inspector();
        let logs_i = ctx.journal().logs().len();
        if let ItemOrResult::Result(mut output) = self.frame_init(frame_init)? {
            let (ctx, inspector) = self.ctx_inspector();
            // for precompiles send logs to inspector.
            if let FrameResult::Call(CallOutcome {
                was_precompile_called,
                precompile_call_logs,
                ..
            }) = &mut output
            {
                if *was_precompile_called {
                    let logs = ctx.journal_mut().logs()[logs_i..].to_vec();
                    for log in logs.iter().chain(precompile_call_logs.iter()).cloned() {
                        inspector.log(ctx, log);
                    }
                }
            }
            frame_end(ctx, inspector, &trace_input_for_end, &mut output);
            return Ok(ItemOrResult::Result(output));
        }

        // if it is new frame, initialize the interpreter.
        let (ctx, inspector, frame) = self.ctx_inspector_frame();
        if let Some(frame) = frame.eth_frame() {
            let interp = &mut frame.interpreter;
            inspector.initialize_interp(interp, ctx);
        };
        Ok(ItemOrResult::Item(frame))
    }
}

impl<DB, INSP, PRECOMPILES> ExecuteEvm
    for ArcEvm<
        EthEvmContext<DB>,
        INSP,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILES,
    >
where
    DB: Database,
    INSP: Inspector<EthEvmContext<DB>, EthInterpreter>,
    PRECOMPILES: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    type ExecutionResult = ExecutionResult<HaltReason>;
    type State = EvmState;
    type Error =
        EVMError<<<EthEvmContext<DB> as ContextTr>::Db as RevmDatabase>::Error, InvalidTransaction>;
    type Tx = <EthEvmContext<DB> as ContextTr>::Tx;
    type Block = <EthEvmContext<DB> as ContextTr>::Block;

    fn set_block(&mut self, block: Self::Block) {
        self.inner.set_block(block);
    }

    fn transact_one(&mut self, tx: Self::Tx) -> Result<Self::ExecutionResult, Self::Error> {
        self.inner.ctx.set_tx(tx);
        let result = ArcEvmHandler::<_, _>::new(self.hardfork_flags).run(self);
        debug_assert!(
            self.subcall_continuations.is_empty(),
            "stale subcall continuations after transaction"
        );
        self.subcall_continuations.clear();
        result
    }

    fn finalize(&mut self) -> Self::State {
        self.inner.journal_mut().finalize()
    }

    fn replay(
        &mut self,
    ) -> Result<ExecResultAndState<Self::ExecutionResult, Self::State>, Self::Error> {
        let execution_result =
            ArcEvmHandler::<_, Self::Error>::new(self.hardfork_flags).run(self)?;
        debug_assert!(
            self.subcall_continuations.is_empty(),
            "stale subcall continuations after replay"
        );
        self.subcall_continuations.clear();
        Ok(ExecResultAndState::new(execution_result, self.finalize()))
    }
}

impl<DB, INSP, PRECOMPILES> InspectEvm
    for ArcEvm<
        EthEvmContext<DB>,
        INSP,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILES,
    >
where
    DB: Database,
    INSP: Inspector<EthEvmContext<DB>, EthInterpreter>,
    PRECOMPILES: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    type Inspector = INSP;

    fn set_inspector(&mut self, inspector: Self::Inspector) {
        self.inner.set_inspector(inspector);
    }

    fn inspect_one_tx(&mut self, tx: Self::Tx) -> Result<Self::ExecutionResult, Self::Error> {
        self.inner.ctx.set_tx(tx);
        let result = ArcEvmHandler::<_, _>::new(self.hardfork_flags).inspect_run(self);
        debug_assert!(
            self.subcall_continuations.is_empty(),
            "stale subcall continuations after inspect"
        );
        self.subcall_continuations.clear();
        result
    }
}

/// implement AlloyEvmTrait for ArcEvm
impl<DB, INSP, PRECOMPILE> AlloyEvmTrait
    for ArcEvm<
        EthEvmContext<DB>,
        INSP,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
    >
where
    DB: Database,
    INSP: Inspector<EthEvmContext<DB>, EthInterpreter>,
    PRECOMPILE: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<DB::Error>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PRECOMPILE;
    type Inspector = INSP;

    fn block(&self) -> &BlockEnv {
        &self.inner.ctx.block
    }

    fn chain_id(&self) -> u64 {
        self.inner.ctx.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        if self.inspect {
            InspectEvm::inspect_tx(self, tx)
        } else {
            ExecuteEvm::transact(self, tx)
        }
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.inner.system_call_with_caller(caller, contract, data)
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Context {
            block: block_env,
            cfg: cfg_env,
            journaled_state,
            ..
        } = self.inner.ctx;

        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    /// Provides immutable references to the database, inspector and precompiles.
    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        (
            &self.inner.ctx.journaled_state.database,
            &self.inner.inspector,
            &self.inner.precompiles,
        )
    }

    /// Provides mutable references to the database, inspector and precompiles.
    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        (
            &mut self.inner.ctx.journaled_state.database,
            &mut self.inner.inspector,
            &mut self.inner.precompiles,
        )
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ArcEvmFactory {
    chain_spec: Arc<ArcChainSpec>,
}

impl ArcEvmFactory {
    pub fn new(chain_spec: Arc<ArcChainSpec>) -> Self {
        Self { chain_spec }
    }

    fn get_hardfork_flags(&self, block_env: &BlockEnv) -> ArcHardforkFlags {
        // The block height in header is u64, convert should succeed.
        self.chain_spec.get_hardfork_flags(
            block_env
                .number
                .try_into()
                .expect("Failed to convert block number to u64"),
        )
    }

    /// Builds the subcall registry for the given hardfork flags.
    ///
    /// Registers subcall-capable precompiles gated on their activation hardfork.
    /// When the hardfork is not active, the registry is empty and subcall addresses
    /// are handled as normal (non-subcall) precompile calls.
    fn build_subcall_registry(&self, hardfork_flags: ArcHardforkFlags) -> Arc<SubcallRegistry> {
        use arc_execution_config::call_from::{MEMO_ADDRESS, MULTICALL3_FROM_ADDRESS};

        use crate::subcall::AllowedCallers;

        let mut registry = SubcallRegistry::new();

        if hardfork_flags.is_active(ArcHardfork::Zero6) {
            registry.register(
                CALL_FROM_ADDRESS,
                Arc::new(CallFromPrecompile),
                AllowedCallers::Only(HashSet::from([MEMO_ADDRESS, MULTICALL3_FROM_ADDRESS])),
            );
        }

        Arc::new(registry)
    }
}

impl EvmFactory for ArcEvmFactory {
    type Evm<DB: Database, I: Inspector<EthEvmContext<DB>>> = ArcEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        Self::Precompiles,
    >;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Context<DB: Database> = EthEvmContext<DB>;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: Database>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let spec = input.cfg_env.spec;
        let hardfork_flags = self.get_hardfork_flags(&input.block_env);

        let ctx = Self::Context::new(db, spec)
            .with_cfg(input.cfg_env)
            .with_block(input.block_env);
        let mut instruction = EthInstructions::default();
        let precompiles = ArcPrecompileProvider::create_precompiles_map(spec, hardfork_flags);
        let inspector = NoOpInspector {};
        let subcall_registry = self.build_subcall_registry(hardfork_flags);

        if hardfork_flags.is_active(ArcHardfork::Zero5) {
            instruction.insert_instruction(
                SELFDESTRUCT,
                Instruction::new(arc_network_selfdestruct, 5000),
            );
        } else {
            instruction.insert_instruction(
                SELFDESTRUCT,
                Instruction::new(arc_network_selfdestruct_zero4, 5000),
            );
        }

        ArcEvm::new(
            ctx,
            inspector,
            precompiles,
            instruction,
            false,
            hardfork_flags,
            subcall_registry,
        )
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>, EthInterpreter>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let spec = input.cfg_env.spec;
        let hardfork_flags = self.get_hardfork_flags(&input.block_env);

        let ctx = Self::Context::new(db, spec)
            .with_cfg(input.cfg_env)
            .with_block(input.block_env);
        let mut instruction = EthInstructions::default();
        let precompiles = ArcPrecompileProvider::create_precompiles_map(spec, hardfork_flags);
        let subcall_registry = self.build_subcall_registry(hardfork_flags);

        if hardfork_flags.is_active(ArcHardfork::Zero5) {
            instruction.insert_instruction(
                SELFDESTRUCT,
                Instruction::new(arc_network_selfdestruct, 5000),
            );
        } else {
            instruction.insert_instruction(
                SELFDESTRUCT,
                Instruction::new(arc_network_selfdestruct_zero4, 5000),
            );
        }

        ArcEvm::new(
            ctx,
            inspector,
            precompiles,
            instruction,
            true,
            hardfork_flags,
            subcall_registry,
        )
    }
}

/// Custom EVM configuration for Arc
#[derive(Debug, Clone)]
pub struct ArcEvmConfig {
    pub(crate) inner: EthEvmConfig<ArcChainSpec, ArcEvmFactory>,
    pub(crate) evm_factory_instance: ArcEvmFactory,
    pub(crate) block_assembler: ArcBlockAssembler<ArcChainSpec>,
}

impl ArcEvmConfig {
    /// Create a new Arc EVM configuration
    pub fn new(inner: EthEvmConfig<ArcChainSpec, ArcEvmFactory>) -> Self {
        let chain_spec = inner.chain_spec().clone();
        let evm_factory_instance = inner.executor_factory.evm_factory().clone();
        Self {
            inner,
            evm_factory_instance,
            block_assembler: ArcBlockAssembler::new(chain_spec.clone()),
        }
    }
}

impl BlockExecutorFactory for ArcEvmConfig {
    type EvmFactory = ArcEvmFactory;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a>;
    type Transaction = TransactionSigned;
    type Receipt = Receipt;

    fn evm_factory(&self) -> &Self::EvmFactory {
        &self.evm_factory_instance
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <Self::EvmFactory as EvmFactory>::Evm<&'a mut State<DB>, I>,
        ctx: EthBlockExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>
    where
        DB: Database + 'a,
        I: InspectorFor<Self, &'a mut State<DB>> + 'a,
    {
        ArcBlockExecutor::new(
            evm,
            ctx,
            self.inner.chain_spec(),
            self.inner.executor_factory.receipt_builder(),
        )
    }
}

impl ConfigureEvm for ArcEvmConfig {
    type Primitives = <EthEvmConfig as ConfigureEvm>::Primitives;
    type Error = <EthEvmConfig as ConfigureEvm>::Error;
    type NextBlockEnvCtx = <EthEvmConfig as ConfigureEvm>::NextBlockEnvCtx;
    type BlockExecutorFactory = Self;
    type BlockAssembler = ArcBlockAssembler<ArcChainSpec>;

    fn block_executor_factory(&self) -> &Self::BlockExecutorFactory {
        self
    }

    fn block_assembler(&self) -> &Self::BlockAssembler {
        &self.block_assembler
    }

    fn evm_env(&self, header: &Header) -> Result<EvmEnv<SpecId>, Self::Error> {
        self.inner.evm_env(header)
    }

    fn builder_for_next_block<'a, DB: Database + 'a>(
        &'a self,
        db: &'a mut State<DB>,
        parent: &'a SealedHeader<<Self::Primitives as NodePrimitives>::BlockHeader>,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<
        impl BlockBuilder<
            Primitives = Self::Primitives,
            Executor: BlockExecutorFor<'a, Self::BlockExecutorFactory, DB>,
        >,
        Self::Error,
    > {
        // Query the ProtocolConfig contract for the reward beneficiary using system call
        let mut attributes = attributes.clone();

        // Create EVM environment for the system call
        let mut system_evm = self.inner.evm_with_env(
            &mut *db,
            self.inner
                .next_evm_env(parent, &attributes)
                .inspect_err(|err| {
                    tracing::error!(error = ?err, "Failed to create EVM environment");
                })?,
        );

        // Override the suggested fee recipient with the reward beneficiary from the ProtocolConfig contract.
        attributes.suggested_fee_recipient = retrieve_reward_beneficiary(&mut system_evm)
            .unwrap_or_else(|err| {
                tracing::warn!(error = ?err, "Failed to get reward beneficiary from ProtocolConfig");
                // fallback - the coinbase address from the parent block
                parent.beneficiary
            });

        // Override the gas limit with the gas limit from the ProtocolConfig contract.
        // ADR-0003: use chainspec bounds; fall back to chainspec default when ProtocolConfig
        // is unavailable or returns an out-of-bounds value.
        let chain_spec = self.inner.chain_spec().as_ref();
        let fee_params = retrieve_fee_params(&mut system_evm).inspect_err(|err| {
                tracing::warn!(error = ?err, "Failed to get fee params from ProtocolConfig, using default gas limit");
            }).ok();
        let next_block_height = parent.number + 1;

        let gas_limit_config = chain_spec.block_gas_limit_config(next_block_height);
        attributes.gas_limit = expected_gas_limit(fee_params.as_ref(), &gas_limit_config);

        let evm_env = self.next_evm_env(parent, &attributes)?;
        let evm = self.evm_with_env(db, evm_env);
        let ctx = self.context_for_next_block(parent, attributes)?;

        Ok(self.create_block_builder(evm, parent, ctx))
    }

    fn next_evm_env(
        &self,
        parent: &Header,
        attributes: &NextBlockEnvAttributes,
    ) -> Result<EvmEnv, Self::Error> {
        self.inner.next_evm_env(parent, attributes)
    }

    fn context_for_block<'a>(
        &self,
        block: &'a SealedBlock<reth_ethereum::Block>,
    ) -> Result<EthBlockExecutionCtx<'a>, Self::Error> {
        self.inner.context_for_block(block)
    }

    fn context_for_next_block(
        &self,
        parent: &SealedHeader,
        attributes: Self::NextBlockEnvCtx,
    ) -> Result<EthBlockExecutionCtx<'_>, Self::Error> {
        let mut ctx = self.inner.context_for_next_block(parent, attributes)?;
        // Clearing extra_data as the executor will supply it after execution.
        ctx.extra_data = Default::default();
        Ok(ctx)
    }
}

impl ConfigureEngineEvm<ExecutionData> for ArcEvmConfig {
    fn evm_env_for_payload(&self, payload: &ExecutionData) -> Result<EvmEnvFor<Self>, Self::Error> {
        self.inner.evm_env_for_payload(payload)
    }

    fn context_for_payload<'a>(
        &self,
        payload: &'a ExecutionData,
    ) -> Result<ExecutionCtxFor<'a, Self>, Self::Error> {
        self.inner.context_for_payload(payload)
    }

    fn tx_iterator_for_payload(
        &self,
        payload: &ExecutionData,
    ) -> Result<impl ExecutableTxIterator<Self>, Self::Error> {
        self.inner.tx_iterator_for_payload(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_result::BeforeFrameInitResult;
    use crate::frame_result::BLOCKLISTED_GAS_PENALTY;
    use crate::log::NativeCoinTransferred;
    use alloy_consensus::Block;
    use alloy_primitives::{address, Bytes, B256, U256};
    use alloy_rpc_types_engine::ExecutionData;
    use alloy_sol_types::SolEvent;
    use arc_execution_config::chainspec::{DEVNET, LOCAL_DEV, TESTNET};
    use arc_precompiles::precompile_provider::ArcPrecompileProvider;
    use arc_precompiles::{
        native_coin_control, NATIVE_COIN_AUTHORITY_ADDRESS, NATIVE_COIN_CONTROL_ADDRESS,
    };
    use reth_chainspec::EthChainSpec;
    use reth_ethereum::evm::revm::{
        context::CfgEnv,
        db::{CacheDB, EmptyDB},
        primitives::keccak256,
    };
    use reth_ethereum::evm::revm_spec_by_timestamp_and_block_number;
    use reth_ethereum_primitives::TransactionSigned;
    use reth_evm::{eth::EthEvmContext, precompiles::PrecompilesMap};
    use reth_node_api::ConfigureEvm;
    use revm::context::ContextTr;
    use revm::interpreter::{
        interpreter_action::{
            CallInput, CallInputs, CallScheme, CallValue, CreateInputs, FrameInit, FrameInput,
        },
        InstructionResult, SharedMemory,
    };
    use revm::{
        bytecode::{opcode, Bytecode},
        context::Context,
        database::InMemoryDB,
        handler::instructions::EthInstructions,
        inspector::NoOpInspector,
    };
    use revm_context_interface::journaled_state::account::JournaledAccountTr;
    use revm_interpreter::interpreter::EthInterpreter;
    use revm_interpreter::CreateScheme;
    use revm_primitives::hardfork::SpecId;

    struct TestCase {
        name: &'static str,
        frame_input: FrameInput,
        expected_log: Option<NativeCoinTransferred>,
    }

    const ADDRESS_A: Address = address!("1000000000000000000000000000000000000001");
    const ADDRESS_B: Address = address!("2000000000000000000000000000000000000002");

    fn create_arc_evm(
        chain_spec: Arc<ArcChainSpec>,
        db: InMemoryDB,
    ) -> ArcEvm<
        EthEvmContext<InMemoryDB>,
        NoOpInspector,
        EthInstructions<EthInterpreter, EthEvmContext<InMemoryDB>>,
        PrecompilesMap,
    > {
        let spec = revm_spec_by_timestamp_and_block_number(&chain_spec, 0, 0);
        let hardfork_flags = chain_spec.get_hardfork_flags(0);
        let cfg_env = CfgEnv::new()
            .with_chain_id(chain_spec.chain_id())
            .with_spec_and_mainnet_gas_params(spec);
        let block_env = BlockEnv::default();

        let ctx = EthEvmContext::new(db, spec)
            .with_cfg(cfg_env)
            .with_block(block_env);
        let mut instruction = EthInstructions::default();
        let precompiles = ArcPrecompileProvider::create_precompiles_map(spec, hardfork_flags);
        let inspector = NoOpInspector {};

        if hardfork_flags.is_active(ArcHardfork::Zero5) {
            instruction.insert_instruction(
                SELFDESTRUCT,
                revm_interpreter::Instruction::new(arc_network_selfdestruct, 5000),
            );
        } else {
            instruction.insert_instruction(
                SELFDESTRUCT,
                revm_interpreter::Instruction::new(arc_network_selfdestruct_zero4, 5000),
            );
        }

        let factory = ArcEvmFactory::new(chain_spec);
        let subcall_registry = factory.build_subcall_registry(hardfork_flags);

        ArcEvm::new(
            ctx,
            inspector,
            precompiles,
            instruction,
            false,
            hardfork_flags,
            subcall_registry,
        )
    }

    fn create_db(accounts: &[(Address, u64)]) -> InMemoryDB {
        let mut db = InMemoryDB::default();
        for (address, balance) in accounts {
            db.insert_account_info(
                *address,
                revm::state::AccountInfo {
                    balance: U256::from(*balance),
                    nonce: 0,
                    code_hash: alloy_primitives::KECCAK256_EMPTY,
                    code: None,
                    account_id: None,
                },
            );
        }
        db
    }

    fn call_input(
        scheme: CallScheme,
        value: U256,
        caller: Address,
        target: Address,
    ) -> Box<CallInputs> {
        Box::new(CallInputs {
            scheme,
            target_address: target,
            bytecode_address: address!("2000000000000000000000000000000000000002"),
            known_bytecode: None,
            value: CallValue::Transfer(value),
            input: CallInput::Bytes(Bytes::new()),
            gas_limit: 100_000,
            is_static: false,
            caller,
            return_memory_offset: 0..1,
        })
    }

    fn create_input(scheme: CreateScheme, value: U256, caller: Address) -> Box<CreateInputs> {
        Box::new(CreateInputs::new(
            caller,
            scheme,
            value,
            Bytes::new(),
            100_000,
        ))
    }

    /// Builds bytecode that executes `CALL(gas, target, value, 0, 0, 0, 0)`.
    ///
    /// Equivalent assembly (stack grows left-to-right, CALL pops 7 args):
    /// ```text
    ///   PUSH1  0       ; retLength
    ///   PUSH1  0       ; retOffset
    ///   PUSH1  0       ; argsLength
    ///   PUSH1  0       ; argsOffset
    ///   PUSH32 <value> ; value to transfer
    ///   PUSH20 <target>; target address
    ///   GAS            ; forward all remaining gas
    ///   CALL           ; CALL(gas, target, value, argsOffset, argsLength, retOffset, retLength)
    ///   POP            ; discard success flag
    ///   STOP
    /// ```
    #[allow(clippy::vec_init_then_push)]
    fn call_with_value_bytecode(target: Address, value: U256) -> Bytecode {
        let mut bytecode = Vec::new();

        // retLength, retOffset, argsLength, argsOffset — all zero
        bytecode.push(opcode::PUSH1);
        bytecode.push(0);
        bytecode.push(opcode::PUSH1);
        bytecode.push(0);
        bytecode.push(opcode::PUSH1);
        bytecode.push(0);
        bytecode.push(opcode::PUSH1);
        bytecode.push(0);

        // value
        bytecode.push(opcode::PUSH32);
        bytecode.extend_from_slice(&value.to_be_bytes::<32>());

        // target address
        bytecode.push(opcode::PUSH20);
        bytecode.extend_from_slice(target.as_slice());

        // gas (all remaining), CALL, discard result, stop
        bytecode.push(opcode::GAS);
        bytecode.push(opcode::CALL);
        bytecode.push(opcode::POP);
        bytecode.push(opcode::STOP);

        Bytecode::new_legacy(bytecode.into())
    }

    /// Builds bytecode that copies `init_code` into memory then executes
    /// `CREATE(value, 0, init_code.len())`.
    ///
    /// Equivalent assembly:
    /// ```text
    ///   ;; Copy init_code into memory byte-by-byte
    ///   PUSH1  <byte>  ; for each byte of init_code...
    ///   PUSH1  <index> ;   memory offset
    ///   MSTORE8        ;   memory[index] = byte
    ///   ...
    ///   ;; Execute CREATE
    ///   PUSH1  <len>   ; init_code length
    ///   PUSH1  0       ; memory offset
    ///   PUSH32 <value> ; value to endow the new contract
    ///   CREATE         ; CREATE(value, offset, length)
    ///   POP            ; discard new contract address
    ///   STOP
    /// ```
    #[allow(clippy::vec_init_then_push)]
    fn create_with_value_bytecode(init_code: &[u8], value: U256) -> Bytecode {
        let mut bytecode = Vec::new();

        // Copy init_code into memory byte-by-byte: memory[i] = init_code[i]
        for (i, byte) in init_code.iter().enumerate() {
            bytecode.push(opcode::PUSH1);
            bytecode.push(*byte);
            bytecode.push(opcode::PUSH1);
            bytecode.push(i as u8);
            bytecode.push(opcode::MSTORE8);
        }

        // length, offset
        bytecode.push(opcode::PUSH1);
        bytecode.push(init_code.len() as u8);
        bytecode.push(opcode::PUSH1);
        bytecode.push(0);

        // value
        bytecode.push(opcode::PUSH32);
        bytecode.extend_from_slice(&value.to_be_bytes::<32>());

        // CREATE, discard result, stop
        bytecode.push(opcode::CREATE);
        bytecode.push(opcode::POP);
        bytecode.push(opcode::STOP);

        Bytecode::new_legacy(bytecode.into())
    }

    fn create_test_evm(
        db: InMemoryDB,
        hardfork_flags: ArcHardforkFlags,
    ) -> ArcEvm<
        EthEvmContext<InMemoryDB>,
        NoOpInspector,
        EthInstructions<EthInterpreter, EthEvmContext<InMemoryDB>>,
        PrecompilesMap,
    > {
        let spec = SpecId::PRAGUE;
        let precompiles = ArcPrecompileProvider::create_precompiles_map(spec, hardfork_flags);
        create_test_evm_with_precompiles(db, hardfork_flags, precompiles)
    }

    fn create_test_evm_with_precompiles(
        db: InMemoryDB,
        hardfork_flags: ArcHardforkFlags,
        precompiles: PrecompilesMap,
    ) -> ArcEvm<
        EthEvmContext<InMemoryDB>,
        NoOpInspector,
        EthInstructions<EthInterpreter, EthEvmContext<InMemoryDB>>,
        PrecompilesMap,
    > {
        let spec = SpecId::PRAGUE;
        let ctx = Context::new(db, spec);
        let instruction = EthInstructions::default();

        let subcall_registry = Arc::new(SubcallRegistry::default());
        ArcEvm::new(
            ctx,
            NoOpInspector {},
            precompiles,
            instruction,
            false,
            hardfork_flags,
            subcall_registry,
        )
    }

    #[test]
    fn test_builder_for_next_block_fallback_behavior() {
        use alloy_primitives::B256;

        // Create test setup
        let chain_spec = LOCAL_DEV.clone();
        let inner_config = EthEvmConfig::new_with_evm_factory(
            chain_spec.clone(),
            ArcEvmFactory::new(chain_spec.clone()),
        );
        let evm_config = ArcEvmConfig::new(inner_config);
        let mut db = State::builder().build();

        // Create a minimal sealed parent header with a known beneficiary
        let fallback_beneficiary = Address::repeat_byte(0x99);
        let parent_header = Header {
            number: 1,
            gas_limit: 30_000_000, // Standard gas limit
            gas_used: 21_000,
            base_fee_per_gas: Some(1_000_000_000), // 1 gwei
            timestamp: 1000,
            beneficiary: fallback_beneficiary,
            ..Default::default()
        };
        let sealed_parent = SealedHeader::new(parent_header, B256::ZERO);

        // Create next block attributes
        let attributes = NextBlockEnvAttributes {
            timestamp: 1001,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO, // Different from parent beneficiary
            gas_limit: 30_000_000,
            parent_beacon_block_root: None,
            withdrawals: None,
            extra_data: Default::default(),
        };

        // Test builder_for_next_block - should succeed and use fallback beneficiary
        let result = evm_config.builder_for_next_block(&mut db, &sealed_parent, attributes);
        assert!(
            result.is_ok(),
            "builder_for_next_block should succeed with fallback"
        );

        // The function should have set suggested_fee_recipient to parent.beneficiary
        // We can't directly inspect the builder's internal state, but we know it succeeded
        // and the fallback logic was triggered since ProtocolConfig contract doesn't exist
    }

    /// Under Zero5, Arc self-emits EIP-7708 Transfer logs for CALL/CREATE value transfers.
    #[test]
    fn test_transact_one_eip7708_log_under_zero5() {
        use alloy_primitives::B256;
        use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
        use revm::handler::SYSTEM_ADDRESS;
        use revm_primitives::TxKind;

        let chain_spec = localdev_with_hardforks(&[
            (ArcHardfork::Zero3, 0),
            (ArcHardfork::Zero4, 0),
            (ArcHardfork::Zero5, 0),
        ]);
        let sender = Address::repeat_byte(0x01);
        let receiver = Address::repeat_byte(0x02);
        let amount = U256::from(100);
        let tx = TxEnv {
            caller: sender,
            kind: TxKind::Call(receiver),
            value: amount,
            gas_limit: 26_000, // Must exceed (21000 + 2*SLOAD) = 25200 for Zero6 blocklist gas
            gas_price: 0,
            chain_id: Some(chain_spec.chain_id()),
            ..Default::default()
        };
        let db = create_db(&[(sender, 1000)]);
        let mut evm = create_arc_evm(chain_spec.clone(), db);

        let result = evm.transact_one(tx).expect("transact_one should succeed");

        assert!(
            matches!(result, ExecutionResult::Success { .. }),
            "transact_one execution should succeed"
        );
        let logs = match &result {
            ExecutionResult::Success { logs, .. } => logs,
            _ => panic!("Expected Success result"),
        };
        assert_eq!(logs.len(), 1, "Zero5: expect 1 EIP-7708 Transfer log");
        assert_eq!(
            logs[0].address, SYSTEM_ADDRESS,
            "Log should be from EIP-7708 system address"
        );
        // Verify full log content: 3 topics (event sig, from, to) + amount data
        assert_eq!(
            logs[0].topics().len(),
            3,
            "EIP-7708 Transfer log should have 3 topics"
        );
        assert_eq!(
            logs[0].topics()[1],
            B256::left_padding_from(sender.as_slice()),
            "topic[1] should be sender address"
        );
        assert_eq!(
            logs[0].topics()[2],
            B256::left_padding_from(receiver.as_slice()),
            "topic[2] should be receiver address"
        );
        assert_eq!(
            logs[0].data.data.as_ref(),
            &amount.to_be_bytes::<32>(),
            "log data should encode the transfer amount"
        );
    }

    /// Under Zero5, replay emits EIP-7708 Transfer logs.
    #[test]
    fn test_replay_eip7708_log_under_zero5() {
        use alloy_primitives::B256;
        use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
        use revm::handler::SYSTEM_ADDRESS;
        use revm_primitives::TxKind;

        let chain_spec = localdev_with_hardforks(&[
            (ArcHardfork::Zero3, 0),
            (ArcHardfork::Zero4, 0),
            (ArcHardfork::Zero5, 0),
        ]);
        let sender = Address::repeat_byte(0x01);
        let receiver = Address::repeat_byte(0x02);
        let amount = U256::from(100);
        let tx = TxEnv {
            caller: sender,
            kind: TxKind::Call(receiver),
            value: amount,
            gas_limit: 26_000, // Must exceed (21000 + 2*SLOAD) = 25200 for Zero6 blocklist gas
            gas_price: 0,
            chain_id: Some(chain_spec.chain_id()),
            ..Default::default()
        };
        let db = create_db(&[(sender, 1000)]);
        let mut evm = create_arc_evm(chain_spec.clone(), db);

        evm.inner.ctx.set_tx(tx);
        let replay_result = evm.replay().expect("replay should succeed");

        assert!(
            matches!(replay_result.result, ExecutionResult::Success { .. }),
            "replay execution should succeed"
        );
        let replay_logs = match &replay_result.result {
            ExecutionResult::Success { logs, .. } => logs,
            _ => panic!("Expected Success result"),
        };
        assert_eq!(
            replay_logs.len(),
            1,
            "Zero5: expect 1 EIP-7708 Transfer log in replay"
        );
        assert_eq!(
            replay_logs[0].address, SYSTEM_ADDRESS,
            "Log should be from EIP-7708 system address"
        );
        // Verify full log content
        assert_eq!(
            replay_logs[0].topics()[1],
            B256::left_padding_from(sender.as_slice()),
            "topic[1] should be sender address"
        );
        assert_eq!(
            replay_logs[0].topics()[2],
            B256::left_padding_from(receiver.as_slice()),
            "topic[2] should be receiver address"
        );
        assert_eq!(
            replay_logs[0].data.data.as_ref(),
            &amount.to_be_bytes::<32>(),
            "log data should encode the transfer amount"
        );
    }

    /// Zero-value CALL under Zero5 emits no log (transfer amount check short-circuits).
    #[test]
    fn test_zero5_zero_value_call_no_log() {
        let db = CacheDB::new(EmptyDB::default());
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5]);
        let mut evm = create_test_evm(db, flags);

        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::ZERO,
                ADDRESS_A,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        assert!(
            matches!(result, BeforeFrameInitResult::None),
            "Zero-value call under Zero5 should return None (no log, no blocklist checks)"
        );
    }

    #[test]
    fn test_capture_transfer_events() {
        let test_cases = vec![
            TestCase {
                name: "call with value",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::Call,
                    U256::from(1),
                    ADDRESS_A,
                    ADDRESS_B,
                )),
                expected_log: Some(NativeCoinTransferred {
                    from: ADDRESS_A,
                    to: ADDRESS_B,
                    amount: U256::from(1),
                }),
            },
            TestCase {
                name: "call with no value",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::Call,
                    U256::ZERO,
                    ADDRESS_A,
                    ADDRESS_B,
                )),
                expected_log: None,
            },
            TestCase {
                name: "create with value",
                frame_input: FrameInput::Create(create_input(
                    CreateScheme::Create,
                    U256::from(1),
                    ADDRESS_A,
                )),
                expected_log: Some(NativeCoinTransferred {
                    from: ADDRESS_A,
                    to: ADDRESS_A.create(0),
                    amount: U256::from(1),
                }),
            },
            TestCase {
                name: "create with no value",
                frame_input: FrameInput::Create(create_input(
                    CreateScheme::Create,
                    U256::ZERO,
                    ADDRESS_A,
                )),
                expected_log: None,
            },
            TestCase {
                name: "create2 with value",
                frame_input: FrameInput::Create(create_input(
                    CreateScheme::Create2 {
                        salt: U256::from(123),
                    },
                    U256::from(1),
                    ADDRESS_A,
                )),
                expected_log: Some(NativeCoinTransferred {
                    from: ADDRESS_A,
                    to: ADDRESS_A.create2(U256::from(123).to_be_bytes(), keccak256(Bytes::new())),
                    amount: U256::from(1),
                }),
            },
            TestCase {
                name: "create2 with no value",
                frame_input: FrameInput::Create(create_input(
                    CreateScheme::Create2 {
                        salt: U256::from(123),
                    },
                    U256::ZERO,
                    ADDRESS_A,
                )),
                expected_log: None,
            },
        ];

        // Test pre-Zero5 hardforks only — Zero5 enables EIP-7708 which emits different logs.
        for hardfork in [ArcHardfork::Zero3, ArcHardfork::Zero4] {
            for test in &test_cases {
                let frame = FrameInit {
                    frame_input: test.frame_input.clone(),
                    memory: SharedMemory::default(),
                    depth: 0,
                };

                let db = CacheDB::new(EmptyDB::default());
                let mut evm = create_test_evm(db, ArcHardforkFlags::with(&[hardfork]));

                // Load native coin control account
                evm.ctx_mut()
                    .journal_mut()
                    .load_account(NATIVE_COIN_CONTROL_ADDRESS)
                    .unwrap();

                let transfer_result = evm.before_frame_init(&frame).unwrap();

                // No early return should occur for basic tests
                assert!(
                    !matches!(transfer_result, BeforeFrameInitResult::Reverted(_)),
                    "{} (hardfork: {:?}): unexpected blocklist violation",
                    test.name,
                    hardfork
                );

                let log_opt = match transfer_result {
                    BeforeFrameInitResult::Log(log, _gas) => Some(log),
                    _ => None,
                };

                assert_eq!(
                    log_opt.is_some(),
                    test.expected_log.is_some(),
                    "{} (hardfork: {:?}): unexpected log result",
                    test.name,
                    hardfork
                );

                if let Some(log) = log_opt {
                    assert_eq!(
                        log.address, NATIVE_COIN_AUTHORITY_ADDRESS,
                        "{} (hardfork: {:?}): wrong log address",
                        test.name, hardfork
                    );

                    let log_data = NativeCoinTransferred::decode_log(&log);
                    assert!(
                        log_data.is_ok(),
                        "{} (hardfork: {:?}): failed to decode log",
                        test.name,
                        hardfork
                    );
                    assert_eq!(
                        &log_data.unwrap().data,
                        test.expected_log.as_ref().unwrap(),
                        "Native send event mismatch"
                    );
                }
            }
        }
    }

    struct BlocklistTestCase {
        name: &'static str,
        frame_input: FrameInput,
        sender_blocklisted: bool,
        recipient_blocklisted: bool,
        expected_reverted: bool,
        expect_context_db_error: bool,
    }

    fn run_blocklist_test_case(
        test_case: &BlocklistTestCase,
        hardfork: ArcHardfork,
        sender: Address,
        recipient: Address,
    ) {
        println!(
            "Running blocklist test case: {} (hardfork: {:?})",
            test_case.name, hardfork
        );

        let frame = FrameInit {
            frame_input: test_case.frame_input.clone(),
            memory: SharedMemory::default(),
            depth: 0,
        };

        let db = CacheDB::new(EmptyDB::default());
        let mut evm = create_test_evm(db, ArcHardforkFlags::with(&[hardfork]));

        // Set transaction nonce for CREATE address calculation
        evm.ctx_mut().tx.nonce = 0;

        if test_case.expect_context_db_error {
            // Intentionally do NOT load the account into the journal
            // This will cause is_address_blocklisted to fail (sload panics when account is not loaded)
            let transfer_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                evm.before_frame_init(&frame)
            }));

            assert!(
                transfer_result.is_err(),
                "{} (hardfork: {:?}): expected panic when account is not loaded but got {:?}",
                test_case.name,
                hardfork,
                transfer_result
            );
            return;
        }

        // Load native coin control account
        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        // Set sender blocklist status
        if test_case.sender_blocklisted {
            let storage_slot = native_coin_control::compute_is_blocklisted_storage_slot(sender);
            evm.ctx_mut()
                .journal_mut()
                .sstore(
                    NATIVE_COIN_CONTROL_ADDRESS,
                    storage_slot.into(),
                    U256::from(1), // Non-zero means blocklisted
                )
                .unwrap();
        }

        // Set recipient blocklist status
        if test_case.recipient_blocklisted {
            let storage_slot = native_coin_control::compute_is_blocklisted_storage_slot(recipient);
            evm.ctx_mut()
                .journal_mut()
                .sstore(
                    NATIVE_COIN_CONTROL_ADDRESS,
                    storage_slot.into(),
                    U256::from(1), // Non-zero means blocklisted
                )
                .unwrap();
        }

        let transfer_result = evm.before_frame_init(&frame).unwrap();

        if test_case.expected_reverted {
            assert!(
                matches!(transfer_result, BeforeFrameInitResult::Reverted(_)),
                "{} (hardfork: {:?}): expected blocklist violation but got {:?}",
                test_case.name,
                hardfork,
                transfer_result
            );
            if let BeforeFrameInitResult::Reverted(reverted) = transfer_result {
                assert_eq!(
                    reverted.gas().spent(),
                    BLOCKLISTED_GAS_PENALTY,
                    "{} (hardfork: {:?}): expect reverted gas spent BLOCKLISTED_GAS_PENALTY",
                    test_case.name,
                    hardfork
                );
            }
        } else {
            assert!(
                !matches!(transfer_result, BeforeFrameInitResult::Reverted(_)),
                "{} (hardfork: {:?}): unexpected blocklist violation",
                test_case.name,
                hardfork
            );
        }
    }

    #[test]
    fn test_capture_transfer_events_with_blocklist() {
        let sender = address!("A000000000000000000000000000000000000001");
        let recipient = address!("B000000000000000000000000000000000000002");

        let test_cases = vec![
            BlocklistTestCase {
                name: "call_with_value_sender_blocklisted",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::Call,
                    U256::from(100),
                    sender,
                    recipient,
                )),
                sender_blocklisted: true,
                recipient_blocklisted: false,
                expected_reverted: true,
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "call_with_value_recipient_blocklisted",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::Call,
                    U256::from(100),
                    sender,
                    recipient,
                )),
                sender_blocklisted: false,
                recipient_blocklisted: true,
                expected_reverted: true,
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "call_with_value_both_blocklisted",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::Call,
                    U256::from(100),
                    sender,
                    recipient,
                )),
                sender_blocklisted: true,
                recipient_blocklisted: true,
                expected_reverted: true,
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "call_with_value_neither_blocklisted",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::Call,
                    U256::from(100),
                    sender,
                    recipient,
                )),
                sender_blocklisted: false,
                recipient_blocklisted: false,
                expected_reverted: false,
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "call_zero_value_sender_blocklisted_bypassed",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::Call,
                    U256::ZERO,
                    sender,
                    recipient,
                )),
                sender_blocklisted: true,
                recipient_blocklisted: false,
                expected_reverted: false, // Zero value bypasses blocklist
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "call_zero_value_recipient_blocklisted_bypassed",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::Call,
                    U256::ZERO,
                    sender,
                    recipient,
                )),
                sender_blocklisted: false,
                recipient_blocklisted: true,
                expected_reverted: false, // Zero value bypasses blocklist
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "delegatecall_sender_blocklisted_bypassed",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::DelegateCall,
                    U256::from(100),
                    sender,
                    recipient,
                )),
                sender_blocklisted: true,
                recipient_blocklisted: false,
                expected_reverted: false, // DelegateCall doesn't transfer value
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "staticcall_recipient_blocklisted_bypassed",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::StaticCall,
                    U256::from(100),
                    sender,
                    recipient,
                )),
                sender_blocklisted: false,
                recipient_blocklisted: true,
                expected_reverted: false, // StaticCall doesn't transfer value
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "callcode_sender_blocklisted_bypassed",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::CallCode,
                    U256::from(100),
                    sender,
                    recipient,
                )),
                sender_blocklisted: true,
                recipient_blocklisted: false,
                expected_reverted: false, // CallCode doesn't transfer value
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "create_with_value_sender_blocklisted",
                frame_input: FrameInput::Create(create_input(
                    CreateScheme::Create,
                    U256::from(100),
                    sender,
                )),
                sender_blocklisted: true,
                recipient_blocklisted: false,
                expected_reverted: true,
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "create2_with_value_sender_blocklisted",
                frame_input: FrameInput::Create(create_input(
                    CreateScheme::Create2 {
                        salt: U256::from(456),
                    },
                    U256::from(100),
                    sender,
                )),
                sender_blocklisted: true,
                recipient_blocklisted: false,
                expected_reverted: true,
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "create_zero_value_sender_blocklisted_bypassed",
                frame_input: FrameInput::Create(create_input(
                    CreateScheme::Create,
                    U256::ZERO,
                    sender,
                )),
                sender_blocklisted: true,
                recipient_blocklisted: false,
                expected_reverted: false, // Zero value bypasses blocklist
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "create2_zero_value_sender_blocklisted_bypassed",
                frame_input: FrameInput::Create(create_input(
                    CreateScheme::Create2 {
                        salt: U256::from(789),
                    },
                    U256::ZERO,
                    sender,
                )),
                sender_blocklisted: true,
                recipient_blocklisted: false,
                expected_reverted: false, // Zero value bypasses blocklist
                expect_context_db_error: false,
            },
            BlocklistTestCase {
                name: "call_with_value_context_db_error",
                frame_input: FrameInput::Call(call_input(
                    CallScheme::Call,
                    U256::from(100),
                    sender,
                    recipient,
                )),
                sender_blocklisted: false,
                recipient_blocklisted: false,
                expected_reverted: false, // Not used - this test expects ContextDbError
                expect_context_db_error: true,
            },
        ];

        for hardfork in [ArcHardfork::Zero4, ArcHardfork::Zero5, ArcHardfork::Zero6] {
            for test_case in &test_cases {
                run_blocklist_test_case(test_case, hardfork, sender, recipient);
            }
        }
    }

    #[test]
    fn test_nested_frame_gas_deduction_zero6() {
        use arc_precompiles::helpers::PRECOMPILE_SLOAD_GAS_COST;
        use revm::handler::EvmTr;

        // Test that nested frames (depth > 0) with Zero6 get OOG when gas is insufficient
        let db = CacheDB::new(EmptyDB::default());
        let mut evm = create_test_evm(db, ArcHardforkFlags::with(&[ArcHardfork::Zero6]));

        // Load native coin control account (required for blocklist checks)
        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        // Create a nested CALL frame (depth=1) with value and insufficient gas
        // With value transfer: needs 2 cold SLOADs = 2 * 2100 = 4200 gas
        let insufficient_gas = PRECOMPILE_SLOAD_GAS_COST; // Only 2100, need 4200
        let frame_with_value = FrameInit {
            frame_input: FrameInput::Call(Box::new(CallInputs {
                scheme: CallScheme::Call,
                target_address: ADDRESS_B,
                bytecode_address: ADDRESS_B,
                known_bytecode: None,
                value: CallValue::Transfer(U256::from(100)),
                input: CallInput::Bytes(Bytes::new()),
                gas_limit: insufficient_gas,
                is_static: false,
                caller: ADDRESS_A,
                return_memory_offset: 0..0,
            })),
            memory: SharedMemory::default(),
            depth: 1, // Nested frame
        };

        let result = evm.frame_init(frame_with_value);

        // Should return OOG because gas_limit (2100) < required (4200)
        match result {
            Ok(ItemOrResult::Result(FrameResult::Call(outcome))) => {
                assert_eq!(
                    outcome.result.result,
                    InstructionResult::OutOfGas,
                    "Expected OutOfGas for nested call with insufficient gas"
                );
            }
            other => panic!(
                "Expected Ok(Result(Call(OutOfGas))), got {:?}",
                other.map(|r| format!("{:?}", r))
            ),
        }
    }

    #[test]
    fn test_blocklist_sload_gas_cold_access() {
        // Both addresses are fresh (cold), so each sload costs 2100 gas
        use arc_precompiles::helpers::PRECOMPILE_SLOAD_GAS_COST;

        let db = CacheDB::new(EmptyDB::default());
        let mut evm = create_test_evm(db, ArcHardforkFlags::with(&[ArcHardfork::Zero6]));

        // Load native coin control account
        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                ADDRESS_A,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        match result {
            BeforeFrameInitResult::Log(_log, gas) => {
                assert_eq!(
                    gas,
                    2 * PRECOMPILE_SLOAD_GAS_COST,
                    "Two cold SLOADs should cost 2 * 2100 = 4200"
                );
            }
            other => panic!("Expected Log result, got {:?}", other),
        }
    }

    #[test]
    fn test_blocklist_sload_gas_warm_access() {
        // Pre-warm the blocklist slots, then call before_frame_init — should get warm pricing
        let db = CacheDB::new(EmptyDB::default());
        let mut evm = create_test_evm(db, ArcHardforkFlags::with(&[ArcHardfork::Zero6]));

        // Load native coin control account
        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        // Pre-warm the blocklist slots by reading them first
        let slot_a = compute_is_blocklisted_storage_slot(ADDRESS_A).into();
        let slot_b = compute_is_blocklisted_storage_slot(ADDRESS_B).into();
        evm.inner
            .ctx
            .journal_mut()
            .sload(NATIVE_COIN_CONTROL_ADDRESS, slot_a)
            .unwrap();
        evm.inner
            .ctx
            .journal_mut()
            .sload(NATIVE_COIN_CONTROL_ADDRESS, slot_b)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                ADDRESS_A,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        match result {
            BeforeFrameInitResult::Log(_log, gas) => {
                // Warm SLOAD costs 100 gas each (EIP-2929)
                assert_eq!(gas, 200, "Two warm SLOADs should cost 2 * 100 = 200");
            }
            other => panic!("Expected Log result, got {:?}", other),
        }
    }

    #[test]
    fn test_blocklist_sload_gas_zero_value_no_charge() {
        // Zero-value calls should not perform any SLOADs (no blocklist check needed)
        let db = CacheDB::new(EmptyDB::default());
        let mut evm = create_test_evm(db, ArcHardforkFlags::with(&[ArcHardfork::Zero6]));

        // Load native coin control account
        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        // Zero-value CALL
        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::ZERO,
                ADDRESS_A,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        assert!(
            matches!(result, BeforeFrameInitResult::None),
            "Zero-value call should return None (no SLOADs, no gas cost)"
        );

        // DelegateCall (no value transfer regardless)
        let frame_delegate = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::DelegateCall,
                U256::from(100),
                ADDRESS_A,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame_delegate).unwrap();
        assert!(
            matches!(result, BeforeFrameInitResult::None),
            "DelegateCall should return None (no SLOADs, no gas cost)"
        );
    }

    #[test]
    fn test_blocklist_sload_gas_pre_zero6_always_fixed() {
        // Pre-Zero6: always uses fixed 2100 per SLOAD regardless of warm/cold
        use arc_precompiles::helpers::PRECOMPILE_SLOAD_GAS_COST;

        let db = CacheDB::new(EmptyDB::default());
        let mut evm = create_test_evm(db, ArcHardforkFlags::with(&[ArcHardfork::Zero5]));

        // Load native coin control account
        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        // Pre-warm the slots
        let slot_a = compute_is_blocklisted_storage_slot(ADDRESS_A).into();
        let slot_b = compute_is_blocklisted_storage_slot(ADDRESS_B).into();
        evm.inner
            .ctx
            .journal_mut()
            .sload(NATIVE_COIN_CONTROL_ADDRESS, slot_a)
            .unwrap();
        evm.inner
            .ctx
            .journal_mut()
            .sload(NATIVE_COIN_CONTROL_ADDRESS, slot_b)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                ADDRESS_A,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        match result {
            BeforeFrameInitResult::Log(_log, gas) => {
                // Pre-Zero6: always 2100 per SLOAD even when warm
                assert_eq!(
                    gas,
                    2 * PRECOMPILE_SLOAD_GAS_COST,
                    "Pre-Zero6 should always charge fixed 2100 per SLOAD"
                );
            }
            other => panic!("Expected Log result, got {:?}", other),
        }
    }

    /// Guards against the static_gas regression introduced during the revm v29→v32 migration.
    /// revm v32 moved SELFDESTRUCT's 5000 base gas (EIP-150) from the instruction handler to
    /// the instruction table's static_gas field. Since Arc overrides the instruction entry via
    /// insert_instruction, the static_gas must be set explicitly or it silently drops to 0.
    #[test]
    fn selfdestruct_static_gas_is_5000() {
        let db = InMemoryDB::default();
        let evm = create_arc_evm(LOCAL_DEV.clone(), db);
        let static_gas =
            evm.inner.instruction.instruction_table[SELFDESTRUCT as usize].static_gas();
        assert_eq!(
            static_gas, 5000,
            "SELFDESTRUCT static gas must be 5000 (EIP-150)"
        );
    }

    /// Verifies that `create_arc_evm` dispatches the pre-Zero5 SELFDESTRUCT variant when
    /// only Zero3+Zero4 are active.  Pre-Zero5 allows SELFDESTRUCT to the zero address
    /// (unlike Zero5) and emits `NativeCoinTransferred` (unlike EIP-7708).
    #[test]
    fn create_arc_evm_dispatches_selfdestruct_zero4() {
        use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
        use revm_primitives::TxKind;

        let chain_spec =
            localdev_with_hardforks(&[(ArcHardfork::Zero3, 0), (ArcHardfork::Zero4, 0)]);
        let sender = Address::repeat_byte(0x11);
        let contract = Address::repeat_byte(0xBB);

        // Bytecode: PUSH20 0x00..00 SELFDESTRUCT  (target = zero address)
        let mut code = vec![opcode::PUSH20];
        code.extend_from_slice(Address::ZERO.as_slice());
        code.push(SELFDESTRUCT);
        let runtime = Bytecode::new_raw(Bytes::from(code.clone()));

        let mut db = create_db(&[(sender, 1000)]);
        db.insert_account_info(
            contract,
            revm::state::AccountInfo {
                balance: U256::from(500),
                nonce: 1,
                code_hash: keccak256(&code),
                code: Some(runtime),
                account_id: None,
            },
        );

        let mut evm = create_arc_evm(chain_spec.clone(), db);
        let tx = TxEnv {
            caller: sender,
            kind: TxKind::Call(contract),
            value: U256::ZERO,
            gas_limit: 100_000,
            gas_price: 0,
            chain_id: Some(chain_spec.chain_id()),
            ..Default::default()
        };

        let result = evm.transact_one(tx).expect("transaction should execute");
        // Zero4 allows SELFDESTRUCT to the zero address — not a revert.
        assert!(
            result.is_success(),
            "Zero4 SELFDESTRUCT to zero address should succeed, got {:?}",
            result
        );
        // Zero4 emits the custom NativeCoinTransferred log, not EIP-7708.
        let logs = result.logs();
        assert!(
            !logs.is_empty(),
            "Zero4 SELFDESTRUCT with nonzero balance should emit a transfer log"
        );
        assert_ne!(
            logs[0].address,
            revm::handler::SYSTEM_ADDRESS,
            "Zero4 should emit NativeCoinTransferred, not an EIP-7708 Transfer log"
        );
    }

    /// Transaction-level regression test: a Zero5 SELFDESTRUCT with nonzero balance
    /// must produce exactly one EIP-7708 Transfer log in the final receipt.
    #[test]
    fn test_zero5_selfdestruct_emits_eip7708_log_in_receipt() {
        use alloy_primitives::b256;
        use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
        use revm::handler::SYSTEM_ADDRESS;
        use revm_primitives::TxKind;

        let chain_spec = localdev_with_hardforks(&[
            (ArcHardfork::Zero3, 0),
            (ArcHardfork::Zero4, 0),
            (ArcHardfork::Zero5, 0),
        ]);

        let sender = Address::repeat_byte(0x11);
        let contract = Address::repeat_byte(0xBB);
        let beneficiary = Address::repeat_byte(0xCC);
        let contract_balance = U256::from(500);

        // Bytecode: PUSH20 <beneficiary> SELFDESTRUCT
        let mut code = vec![opcode::PUSH20];
        code.extend_from_slice(beneficiary.as_slice());
        code.push(SELFDESTRUCT);
        let runtime = Bytecode::new_raw(Bytes::from(code.clone()));

        let mut db = create_db(&[(sender, 1000)]);
        db.insert_account_info(
            contract,
            revm::state::AccountInfo {
                balance: contract_balance,
                nonce: 1,
                code_hash: keccak256(&code),
                code: Some(runtime),
                account_id: None,
            },
        );

        let mut evm = create_arc_evm(chain_spec.clone(), db);
        let tx = TxEnv {
            caller: sender,
            kind: TxKind::Call(contract),
            value: U256::ZERO,
            gas_limit: 100_000,
            gas_price: 0,
            chain_id: Some(chain_spec.chain_id()),
            ..Default::default()
        };

        let result = evm.transact_one(tx).expect("transaction should execute");
        assert!(
            result.is_success(),
            "Zero5 SELFDESTRUCT to non-zero beneficiary should succeed, got {:?}",
            result
        );

        let logs = result.logs();
        assert_eq!(
            logs.len(),
            1,
            "Zero5 SELFDESTRUCT should emit exactly one EIP-7708 Transfer log, got {}",
            logs.len()
        );

        let log = &logs[0];
        assert_eq!(
            log.address, SYSTEM_ADDRESS,
            "Log should come from the EIP-7708 system address"
        );

        let topics = log.data.topics();
        assert_eq!(topics.len(), 3, "Transfer log should have 3 topics");
        // topic0: Transfer(address,address,uint256) selector
        assert_eq!(
            topics[0],
            b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"),
        );
        // topic1: from = contract
        assert_eq!(topics[1], B256::left_padding_from(contract.as_slice()));
        // topic2: to = beneficiary
        assert_eq!(topics[2], B256::left_padding_from(beneficiary.as_slice()));
        // data: amount = contract balance
        assert_eq!(
            log.data.data.as_ref(),
            &contract_balance.to_be_bytes::<32>()
        );
    }

    #[test]
    fn transfer_to_selfdestruct_account() {
        let db = InMemoryDB::default();
        let mut evm = create_test_evm(
            db,
            ArcHardforkFlags::with(&[ArcHardfork::Zero4, ArcHardfork::Zero5]),
        );

        // Load native coin control account
        let spec_id = evm.ctx().cfg.spec;
        let journal = evm.ctx_mut().journal_mut();
        journal.load_account(NATIVE_COIN_CONTROL_ADDRESS).unwrap();

        // 1. Prepare ADDRESS_B as a destructed account.
        journal
            .load_account_mut_optional_code(ADDRESS_A, false)
            .expect("load ADDRESS_A")
            .set_balance(U256::from(100));
        journal.load_account(ADDRESS_B).expect("load ADDRESS_B");
        journal
            .create_account_checkpoint(ADDRESS_A, ADDRESS_B, U256::from(100), spec_id)
            .unwrap();
        journal
            .selfdestruct(ADDRESS_B, ADDRESS_A, false)
            .expect("selfdestruct");

        // 2. Prepare frame to transfer balance from ADDRESS_A to ADDRESS_B
        let frame = FrameInit {
            frame_input: FrameInput::Call(Box::new(CallInputs {
                scheme: CallScheme::Call,
                target_address: ADDRESS_B,
                bytecode_address: ADDRESS_A,
                known_bytecode: None,
                value: CallValue::Transfer(U256::from(100)),
                input: CallInput::Bytes(Bytes::new()),
                gas_limit: 100_000,
                caller: ADDRESS_A,
                is_static: false,
                return_memory_offset: 0..0,
            })),
            memory: SharedMemory::default(),
            depth: 0,
        };

        // 3. Should revert on transferring to destructed account
        let result = evm.before_frame_init(&frame);
        assert!(
            matches!(result, Ok(BeforeFrameInitResult::Reverted(_))),
            "expect revert on transferring to destructed account"
        );
    }

    #[test]
    fn test_chain_spec_resolves_to_expected_spec_id() {
        let cases: &[(&str, &Arc<ArcChainSpec>, SpecId)] = &[
            ("localdev", &LOCAL_DEV, SpecId::OSAKA),
            ("devnet", &DEVNET, SpecId::PRAGUE),
            ("testnet", &TESTNET, SpecId::PRAGUE),
        ];

        for (label, chain_spec, expected) in cases {
            let spec = revm_spec_by_timestamp_and_block_number(chain_spec, 0, 0);
            assert_eq!(spec, *expected, "{label} should resolve to {expected:?}");
        }
    }

    // These two tests exercise the extra_data contract that the executor's base fee
    // validation depends on, namely, applying strict validation against extra_data contents.
    //
    // Reth provides a tx_count_hint, which can distinguish between various paths (payload execution vs. building vs. an eth_call type context)
    // though simply validating against the presence of the extra_data is more explicit.
    // To do this though, we need to clear the extra_data vs. blindly copying it over from a parent
    // when building context for the next block.
    #[test]
    fn context_for_payload_preserves_extra_data() {
        let chain_spec = LOCAL_DEV.clone();
        let evm_config = ArcEvmConfig::new(EthEvmConfig::new_with_evm_factory(
            chain_spec.clone(),
            ArcEvmFactory::new(chain_spec.clone()),
        ));

        let block: Block<TransactionSigned> = Block {
            header: Header {
                extra_data: arc_execution_config::gas_fee::encode_base_fee_to_bytes(12345),
                ..Default::default()
            },
            ..Default::default()
        };
        let payload = ExecutionData::from_block_unchecked(B256::ZERO, &block);

        let ctx = evm_config
            .context_for_payload(&payload)
            .expect("context_for_payload must succeed");

        assert!(
            !ctx.extra_data.is_empty(),
            "context_for_payload must preserve extra_data from the payload — \
             the executor relies on it being non-empty to trigger base fee validation"
        );
    }

    #[test]
    fn context_for_next_block_clears_extra_data() {
        let chain_spec = LOCAL_DEV.clone();
        let evm_config = ArcEvmConfig::new(EthEvmConfig::new_with_evm_factory(
            chain_spec.clone(),
            ArcEvmFactory::new(chain_spec.clone()),
        ));

        // Use a parent with non-empty extra_data to confirm we clear it.
        let parent = SealedHeader::new(
            Header {
                extra_data: arc_execution_config::gas_fee::encode_base_fee_to_bytes(12345),
                ..Default::default()
            },
            B256::ZERO,
        );
        let attributes = NextBlockEnvAttributes {
            timestamp: 1,
            prev_randao: B256::ZERO,
            suggested_fee_recipient: Address::ZERO,
            gas_limit: 30_000_000,
            parent_beacon_block_root: None,
            withdrawals: None,
            extra_data: Default::default(),
        };

        let ctx = evm_config
            .context_for_next_block(&parent, attributes)
            .expect("context_for_next_block must succeed");

        assert!(
            ctx.extra_data.is_empty(),
            "context_for_next_block must clear extra_data so the executor's \
             base fee validation gate does not fire during block building or simulation"
        );
    }

    // ========================================================================
    // Subcall integration tests
    // ========================================================================

    mod build_subcall_registry {
        use super::*;
        use arc_execution_config::call_from::{MEMO_ADDRESS, MULTICALL3_FROM_ADDRESS};
        use arc_precompiles::call_from::CALL_FROM_ADDRESS;

        fn factory() -> ArcEvmFactory {
            ArcEvmFactory::new(LOCAL_DEV.clone())
        }

        #[test]
        fn registry_populated_when_zero6_active() {
            let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero6]);
            let registry = factory().build_subcall_registry(flags);

            assert!(
                registry.get(&CALL_FROM_ADDRESS).is_some(),
                "CallFrom should be registered when Zero6 is active"
            );
        }

        #[test]
        fn registry_empty_when_zero6_not_active() {
            let flags = ArcHardforkFlags::with(&[
                ArcHardfork::Zero3,
                ArcHardfork::Zero4,
                ArcHardfork::Zero5,
            ]);
            let registry = factory().build_subcall_registry(flags);

            assert!(
                registry.get(&CALL_FROM_ADDRESS).is_none(),
                "CallFrom should not be registered when Zero6 is inactive"
            );
        }

        #[test]
        fn registry_empty_with_no_hardforks() {
            let flags = ArcHardforkFlags::default();
            let registry = factory().build_subcall_registry(flags);

            assert!(
                registry.get(&CALL_FROM_ADDRESS).is_none(),
                "CallFrom should not be registered with no hardforks active"
            );
        }

        #[test]
        fn allowed_callers_include_memo_and_multicall3_from() {
            let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero6]);
            let registry = factory().build_subcall_registry(flags);

            let (_precompile, allowed_callers) = registry
                .get(&CALL_FROM_ADDRESS)
                .expect("should be registered");

            assert!(
                allowed_callers.is_allowed(&MEMO_ADDRESS),
                "Memo should be an allowed caller"
            );
            assert!(
                allowed_callers.is_allowed(&MULTICALL3_FROM_ADDRESS),
                "Multicall3From should be an allowed caller"
            );
        }

        #[test]
        fn arbitrary_address_not_allowed() {
            let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero6]);
            let registry = factory().build_subcall_registry(flags);

            let (_precompile, allowed_callers) = registry
                .get(&CALL_FROM_ADDRESS)
                .expect("should be registered");

            let rando = address!("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
            assert!(
                !allowed_callers.is_allowed(&rando),
                "arbitrary address should not be an allowed caller"
            );
        }
    }

    mod subcall_tests {
        use super::*;
        use crate::subcall_test::SUBCALL_TEST_ADDRESS;
        use alloy_primitives::{address, keccak256, Bytes, U256};
        use alloy_sol_types::SolType;
        use arc_execution_config::chainspec::LOCAL_DEV;
        use revm::context_interface::result::ExecutionResult;
        use revm::database::InMemoryDB;
        use revm::state::{AccountInfo, Bytecode};
        use revm_primitives::TxKind;

        // ----- EVM bytecode helpers -----

        use revm::bytecode::opcode::*;

        /// Appends the return-returndata epilogue: copy all returndata to memory[0..] and RETURN.
        fn append_return_returndata(code: &mut Vec<u8>) {
            #[rustfmt::skip]
            code.extend_from_slice(&[
                // RETURNDATACOPY(destOffset=0, srcOffset=0, size=RETURNDATASIZE)
                RETURNDATASIZE,
                PUSH1, 0x00,      // srcOffset=0
                PUSH1, 0x00,      // destOffset=0
                RETURNDATACOPY,
                // RETURN(offset=0, size=RETURNDATASIZE)
                RETURNDATASIZE,
                PUSH1, 0x00,      // offset=0
                RETURN,
            ]);
        }

        /// Returns EVM bytecode that reads calldata[0..32], multiplies by 2, and returns it.
        fn echo_double_bytecode() -> Bytes {
            #[rustfmt::skip]
            let code = vec![
                PUSH1, 0x00,      // offset 0
                CALLDATALOAD,     // stack: calldata[0..32]
                PUSH1, 0x02,      // multiplier
                MUL,              // stack: value * 2
                PUSH1, 0x00,      // memory offset
                MSTORE,           // memory[0..32] = result
                PUSH1, 0x20,      // return size = 32
                PUSH1, 0x00,      // return offset = 0
                RETURN,
            ];
            Bytes::from(code)
        }

        /// Returns EVM bytecode that always reverts with empty data.
        fn reverting_bytecode() -> Bytes {
            #[rustfmt::skip]
            let code = vec![
                PUSH1, 0x00,
                PUSH1, 0x00,
                REVERT,
            ];
            Bytes::from(code)
        }

        /// Returns EVM bytecode that returns msg.sender as a 32-byte word.
        fn return_caller_bytecode() -> Bytes {
            #[rustfmt::skip]
            let code = vec![
                CALLER,           // stack: msg.sender
                PUSH1, 0x00,      // memory offset
                MSTORE,           // memory[0..32] = msg.sender
                PUSH1, 0x20,      // return size = 32
                PUSH1, 0x00,      // return offset = 0
                RETURN,
            ];
            Bytes::from(code)
        }

        /// Returns EVM bytecode that forwards all calldata to `target` via CALL, then
        /// returns the returndata unchanged.
        fn wrapper_call_bytecode(target: Address) -> Bytes {
            #[rustfmt::skip]
            let mut code = vec![
                // CALLDATACOPY(destOffset=0, srcOffset=0, size=CALLDATASIZE)
                CALLDATASIZE,
                PUSH1, 0x00,      // srcOffset=0
                PUSH1, 0x00,      // destOffset=0
                CALLDATACOPY,
                // CALL(gas, target, value=0, argsOff=0, argsLen=CALLDATASIZE, retOff=0, retLen=0)
                PUSH1, 0x00,      // retLen=0
                PUSH1, 0x00,      // retOffset=0
                CALLDATASIZE,     // argsLen
                PUSH1, 0x00,      // argsOffset=0
                PUSH1, 0x00,      // value=0
                PUSH20,           // target address follows
            ];
            code.extend_from_slice(target.as_slice());
            code.extend_from_slice(&[GAS, CALL, POP]);
            append_return_returndata(&mut code);
            Bytes::from(code)
        }

        /// Returns EVM bytecode that forwards all calldata to `target` via STATICCALL,
        /// then returns the returndata unchanged.
        fn static_call_wrapper_bytecode(target: Address) -> Bytes {
            #[rustfmt::skip]
            let mut code = vec![
                // CALLDATACOPY(destOffset=0, srcOffset=0, size=CALLDATASIZE)
                CALLDATASIZE,
                PUSH1, 0x00,      // srcOffset=0
                PUSH1, 0x00,      // destOffset=0
                CALLDATACOPY,
                // STATICCALL(gas, target, argsOff=0, argsLen=CALLDATASIZE, retOff=0, retLen=0)
                PUSH1, 0x00,      // retLen=0
                PUSH1, 0x00,      // retOffset=0
                CALLDATASIZE,     // argsLen
                PUSH1, 0x00,      // argsOffset=0
                PUSH20,           // target address follows
            ];
            code.extend_from_slice(target.as_slice());
            code.extend_from_slice(&[GAS, STATICCALL, POP]);
            append_return_returndata(&mut code);
            Bytes::from(code)
        }

        // ----- Encoding helpers -----

        /// ABI-encodes `(address target, bytes calldata, bytes memo)` for the subcall test
        /// precompile.
        fn encode_subcall_test_input(target: Address, calldata: &[u8]) -> Bytes {
            type Input = (
                alloy_sol_types::sol_data::Address,
                alloy_sol_types::sol_data::Bytes,
            );
            Bytes::from(Input::abi_encode(&(target, calldata.to_vec())))
        }

        /// Decodes an `Error(string)` revert reason from raw bytes.
        /// Returns the contained message string, or panics if the format doesn't match.
        fn decode_revert_reason(data: &[u8]) -> String {
            const REVERT_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];
            assert!(
                data.len() >= 4 && data[..4] == REVERT_SELECTOR,
                "expected Error(string) revert selector, got {data:?}"
            );
            <alloy_sol_types::sol_data::String as SolType>::abi_decode(&data[4..])
                .expect("should decode revert reason string")
        }

        // ----- Setup helpers -----

        /// Creates an ArcEvm backed by an in-memory DB with accounts and deployed contracts.
        ///
        /// `accounts`: list of `(address, balance)` pairs for EOAs.
        /// `contracts`: list of `(address, bytecode)` pairs for contract accounts.
        /// Creates an ArcEvm with both SubcallTestPrecompile (unrestricted) and
        /// CallFromPrecompile (with the given allowlist) registered.
        fn setup_test_evm(
            accounts: &[(Address, U256)],
            contracts: &[(Address, Bytes)],
            call_from_allowlist: &[Address],
        ) -> ArcEvm<
            EthEvmContext<InMemoryDB>,
            revm::inspector::NoOpInspector,
            EthInstructions<EthInterpreter, EthEvmContext<InMemoryDB>>,
            PrecompilesMap,
        > {
            use crate::subcall::AllowedCallers;
            use crate::subcall_test::{SubcallTestPrecompile, SUBCALL_TEST_ADDRESS};

            let chain_spec = LOCAL_DEV.clone();
            let mut db = InMemoryDB::default();

            for (addr, balance) in accounts {
                db.insert_account_info(
                    *addr,
                    AccountInfo {
                        balance: *balance,
                        nonce: 0,
                        code_hash: alloy_primitives::KECCAK256_EMPTY,
                        code: None,
                        account_id: None,
                    },
                );
            }

            for (addr, code) in contracts {
                db.insert_account_info(
                    *addr,
                    AccountInfo {
                        balance: U256::ZERO,
                        nonce: 1,
                        code_hash: keccak256(code),
                        code: Some(Bytecode::new_raw(code.clone())),
                        account_id: None,
                    },
                );
            }

            let spec =
                reth_ethereum::evm::revm_spec_by_timestamp_and_block_number(&chain_spec, 0, 0);
            let hardfork_flags = chain_spec.get_hardfork_flags(0);
            let mut cfg_env = revm::context::CfgEnv::new()
                .with_chain_id(chain_spec.chain_id())
                .with_spec_and_mainnet_gas_params(spec);
            // Disable EIP-7825 tx gas limit cap so tests can use arbitrary gas limits.
            cfg_env.tx_gas_limit_cap = Some(u64::MAX);
            let ctx = EthEvmContext::new(db, spec)
                .with_cfg(cfg_env)
                .with_block(BlockEnv::default());
            let precompiles = ArcPrecompileProvider::create_precompiles_map(spec, hardfork_flags);
            let mut instruction = EthInstructions::default();
            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                instruction.insert_instruction(
                    SELFDESTRUCT,
                    revm_interpreter::Instruction::new(arc_network_selfdestruct, 5000),
                );
            }

            let mut registry = SubcallRegistry::new();
            registry.register(
                SUBCALL_TEST_ADDRESS,
                Arc::new(SubcallTestPrecompile),
                AllowedCallers::Unrestricted,
            );
            let allowed_callers =
                AllowedCallers::Only(HashSet::from_iter(call_from_allowlist.iter().copied()));
            registry.register(
                CALL_FROM_ADDRESS,
                Arc::new(CallFromPrecompile),
                allowed_callers,
            );

            ArcEvm::new(
                ctx,
                revm::inspector::NoOpInspector {},
                precompiles,
                instruction,
                false,
                hardfork_flags,
                Arc::new(registry),
            )
        }

        // ----- Test addresses -----

        const EOA: Address = address!("e000000000000000000000000000000000000001");
        const WRAPPER: Address = address!("c000000000000000000000000000000000000001");
        const ECHO_CONTRACT: Address = address!("c000000000000000000000000000000000000002");
        const REVERT_CONTRACT: Address = address!("c000000000000000000000000000000000000003");
        const CALLER_CONTRACT: Address = address!("c000000000000000000000000000000000000004");
        const WRAPPER_INNER: Address = address!("c000000000000000000000000000000000000005");

        // ----- Integration tests -----

        /// EOA → wrapper → subcall_test_precompile → echo_double(42)
        /// Asserts: success, output contains echo_double(42) = 84.
        #[test]
        fn test_subcall_happy_path() {
            let wrapper_code = wrapper_call_bytecode(SUBCALL_TEST_ADDRESS);
            let echo_code = echo_double_bytecode();

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(WRAPPER, wrapper_code), (ECHO_CONTRACT, echo_code)],
                &[],
            );

            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let subcall_input = encode_subcall_test_input(ECHO_CONTRACT, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: subcall_input,
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    let data = output.data();
                    assert!(!data.is_empty(), "should have non-empty output");

                    // The test precompile ABI-encodes the child output as `bytes`.
                    // Decode the outer `bytes` wrapper to get the raw child return data.
                    let child_output =
                        <alloy_sol_types::sol_data::Bytes as SolType>::abi_decode(data)
                            .expect("should decode bytes wrapper");
                    // echo_double(42) → 84
                    let expected = U256::from(84).to_be_bytes::<32>();
                    assert_eq!(child_output.as_ref(), &expected, "result mismatch");
                }
                other => panic!("expected Success, got {other:?}"),
            };
        }

        /// EOA → wrapper → subcall_test_precompile → reverting contract
        /// The wrapper catches the revert, so the top-level tx succeeds.
        /// The wrapper's returndata is the precompile's revert output (empty).
        #[test]
        fn test_subcall_child_reverts() {
            let wrapper_code = wrapper_call_bytecode(SUBCALL_TEST_ADDRESS);
            let revert_code = reverting_bytecode();

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(WRAPPER, wrapper_code), (REVERT_CONTRACT, revert_code)],
                &[],
            );

            let subcall_input = encode_subcall_test_input(REVERT_CONTRACT, &[]);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: subcall_input,
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                // The wrapper forwards returndata from the precompile. When the child
                // reverts, the test precompile signals failure (success=false) with
                // empty output, so the wrapper sees a REVERT with empty data.
                ExecutionResult::Success { output, .. } => {
                    assert!(
                        output.data().is_empty(),
                        "wrapper should return empty data on child revert"
                    );
                }
                other => panic!("expected Success (wrapper catches revert), got {other:?}"),
            };
        }

        /// EOA → wrapper → subcall_test_precompile → return_caller contract
        /// Asserts: the child sees msg.sender as the wrapper address (not the precompile).
        #[test]
        fn test_subcall_caller_is_wrapper_not_precompile() {
            let wrapper_code = wrapper_call_bytecode(SUBCALL_TEST_ADDRESS);
            let caller_code = return_caller_bytecode();

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(WRAPPER, wrapper_code), (CALLER_CONTRACT, caller_code)],
                &[],
            );

            let subcall_input = encode_subcall_test_input(CALLER_CONTRACT, &[]);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: subcall_input,
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    // The test precompile ABI-encodes the child output as `bytes`.
                    // The child (return_caller) returns msg.sender left-padded to 32 bytes.
                    // The subcall test precompile passes `caller = inputs.caller` to the
                    // child, so msg.sender should be the WRAPPER.
                    let child_output =
                        <alloy_sol_types::sol_data::Bytes as SolType>::abi_decode(output.data())
                            .expect("should decode bytes wrapper");
                    let returned_address = Address::from_slice(&child_output[12..32]);
                    assert_eq!(
                        returned_address, WRAPPER,
                        "child should see msg.sender = wrapper, not the precompile address"
                    );
                }
                other => panic!("expected Success, got {other:?}"),
            };
        }

        /// EOA → subcall_test_precompile directly (no wrapper)
        /// Asserts: success, output contains echo_double(21) = 42.
        #[test]
        fn test_subcall_direct_eoa_call() {
            let echo_code = echo_double_bytecode();

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(ECHO_CONTRACT, echo_code)],
                &[],
            );

            let inner_calldata = U256::from(21).to_be_bytes::<32>().to_vec();
            let subcall_input = encode_subcall_test_input(ECHO_CONTRACT, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(SUBCALL_TEST_ADDRESS),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: subcall_input,
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    let child_output =
                        <alloy_sol_types::sol_data::Bytes as SolType>::abi_decode(output.data())
                            .expect("should decode bytes wrapper");
                    // echo_double(21) → 42
                    let expected = U256::from(42).to_be_bytes::<32>();
                    assert_eq!(child_output.as_ref(), &expected);
                }
                other => panic!("expected Success, got {other:?}"),
            };
        }

        /// EOA → wrapper_outer → subcall_test → wrapper_inner → subcall_test → echo_double(42)
        /// Asserts: the inner echo_double(42) = 84 result propagates through both subcall
        /// layers, verifying correct frame-stack ordering for nested subcalls.
        #[test]
        fn test_subcall_reentrant() {
            let echo_code = echo_double_bytecode();
            // wrapper_inner calls subcall_test_precompile
            let wrapper_inner_code = wrapper_call_bytecode(SUBCALL_TEST_ADDRESS);
            // wrapper_outer also calls subcall_test_precompile
            let wrapper_outer_code = wrapper_call_bytecode(SUBCALL_TEST_ADDRESS);

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[
                    (WRAPPER, wrapper_outer_code),
                    (WRAPPER_INNER, wrapper_inner_code),
                    (ECHO_CONTRACT, echo_code),
                ],
                &[],
            );

            // Inner subcall: wrapper_inner → subcall_test → echo_double(42)
            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let inner_subcall_input = encode_subcall_test_input(ECHO_CONTRACT, &inner_calldata);

            // Outer subcall: wrapper_outer → subcall_test → wrapper_inner(inner_subcall_input)
            let outer_subcall_input =
                encode_subcall_test_input(WRAPPER_INNER, inner_subcall_input.as_ref());

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER),
                value: U256::ZERO,
                gas_limit: 5_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: outer_subcall_input,
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    // The return chain is:
                    //   echo_double(42) → raw 84 (32 bytes)
                    //   inner subcall_test → ABI-encode as `bytes` (wrapper around raw 84)
                    //   wrapper_inner → forwards inner subcall_test output as-is
                    //   outer subcall_test → ABI-encode as `bytes` (wrapper around inner output)
                    //   wrapper_outer → forwards outer subcall_test output as-is
                    //
                    // Peel the outer `bytes` wrapper:
                    let inner_output =
                        <alloy_sol_types::sol_data::Bytes as SolType>::abi_decode(output.data())
                            .expect("should decode outer bytes wrapper");
                    // Peel the inner `bytes` wrapper:
                    let echo_result =
                        <alloy_sol_types::sol_data::Bytes as SolType>::abi_decode(&inner_output)
                            .expect("should decode inner bytes wrapper");
                    // echo_double(42) → 84
                    let expected = U256::from(84).to_be_bytes::<32>();
                    assert_eq!(
                        echo_result.as_ref(),
                        &expected,
                        "inner echo_double(42) should propagate through nested subcalls as 84"
                    );
                }
                other => panic!("expected Success, got {other:?}"),
            };
        }

        /// AllowedCallers enforcement: if the caller is not in the allowed set, the call
        /// should revert.
        #[test]
        fn test_subcall_unauthorized_caller_rejected() {
            use crate::subcall::AllowedCallers;
            use std::collections::HashSet;

            let wrapper_code = wrapper_call_bytecode(SUBCALL_TEST_ADDRESS);
            let echo_code = echo_double_bytecode();

            // Build a custom EVM where the test precompile only allows a specific address
            let authorized_caller = address!("a000000000000000000000000000000000000099");
            let chain_spec = LOCAL_DEV.clone();
            let mut db = InMemoryDB::default();

            // Insert accounts
            db.insert_account_info(
                EOA,
                AccountInfo {
                    balance: U256::from(1_000_000u64),
                    nonce: 0,
                    code_hash: alloy_primitives::KECCAK256_EMPTY,
                    code: None,
                    account_id: None,
                },
            );
            for (addr, code) in [(WRAPPER, &wrapper_code), (ECHO_CONTRACT, &echo_code)] {
                db.insert_account_info(
                    addr,
                    AccountInfo {
                        balance: U256::ZERO,
                        nonce: 1,
                        code_hash: keccak256(code),
                        code: Some(Bytecode::new_raw(code.clone())),
                        account_id: None,
                    },
                );
            }

            let spec =
                reth_ethereum::evm::revm_spec_by_timestamp_and_block_number(&chain_spec, 0, 0);
            let hardfork_flags = chain_spec.get_hardfork_flags(0);
            let mut cfg_env = revm::context::CfgEnv::new()
                .with_chain_id(chain_spec.chain_id())
                .with_spec_and_mainnet_gas_params(spec);
            // Disable EIP-7825 tx gas limit cap so tests can use arbitrary gas limits.
            cfg_env.tx_gas_limit_cap = Some(u64::MAX);
            let ctx = EthEvmContext::new(db, spec)
                .with_cfg(cfg_env)
                .with_block(BlockEnv::default());
            let precompiles = ArcPrecompileProvider::create_precompiles_map(spec, hardfork_flags);
            let mut instruction = EthInstructions::default();
            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                instruction.insert_instruction(
                    SELFDESTRUCT,
                    revm_interpreter::Instruction::new(arc_network_selfdestruct, 5000),
                );
            }

            // Custom registry: restrict subcall_test to `authorized_caller` only
            let mut registry = SubcallRegistry::new();
            registry.register(
                SUBCALL_TEST_ADDRESS,
                Arc::new(crate::subcall_test::SubcallTestPrecompile),
                AllowedCallers::Only(HashSet::from([authorized_caller])),
            );

            let mut evm = ArcEvm::new(
                ctx,
                revm::inspector::NoOpInspector {},
                precompiles,
                instruction,
                false,
                hardfork_flags,
                Arc::new(registry),
            );

            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let subcall_input = encode_subcall_test_input(ECHO_CONTRACT, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: subcall_input,
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                // The wrapper catches the precompile's revert, so the tx succeeds.
                // The returndata is the revert output (Error(string) ABI-encoded).
                ExecutionResult::Success { output, .. } => {
                    let reason = decode_revert_reason(output.data());
                    assert_eq!(reason, "unauthorized caller");
                }
                other => panic!("expected Success (wrapper catches revert), got {other:?}"),
            };
        }

        /// Subcall precompiles must reject static-context invocations.
        #[test]
        fn test_subcall_static_context_rejected() {
            let static_wrapper_code = static_call_wrapper_bytecode(SUBCALL_TEST_ADDRESS);
            let echo_code = echo_double_bytecode();

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(WRAPPER, static_wrapper_code), (ECHO_CONTRACT, echo_code)],
                &[],
            );

            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let subcall_input = encode_subcall_test_input(ECHO_CONTRACT, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: subcall_input,
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                // The wrapper catches the precompile's revert, so the tx succeeds.
                // The returndata is the revert output (Error(string) ABI-encoded).
                // STATICCALL sets scheme=StaticCall, so the scheme check fires before
                // the is_static check.
                ExecutionResult::Success { output, .. } => {
                    let reason = decode_revert_reason(output.data());
                    assert_eq!(reason, "subcall precompiles only support CALL scheme");
                }
                other => panic!("expected Success (wrapper catches revert), got {other:?}"),
            };
        }

        // ================================================================
        // CallFrom precompile tests
        // ================================================================

        /// ABI-encodes the raw parameters for `callFrom(address sender, address target, bytes data)`.
        /// Uses the sol!-generated type to ensure encoding matches `abi_decode`.
        fn encode_call_from_input(sender: Address, target: Address, data: &[u8]) -> Bytes {
            use alloy_sol_types::SolCall;
            use arc_precompiles::call_from::ICallFrom;
            let call = ICallFrom::callFromCall {
                sender,
                target,
                data: data.to_vec().into(),
            };
            // abi_encode includes the 4-byte function selector, matching standard
            // Solidity call syntax and what abi_decode expects.
            Bytes::from(call.abi_encode())
        }

        /// Decodes ABI-encoded `(bool success, bytes returnData)` from CallFrom output.
        fn decode_call_from_output(data: &[u8]) -> (bool, Vec<u8>) {
            use alloy_sol_types::SolCall;
            use arc_precompiles::call_from::ICallFrom;
            let ret = ICallFrom::callFromCall::abi_decode_returns(data)
                .expect("failed to ABI-decode callFrom return");
            (ret.success, ret.returnData.to_vec())
        }

        /// Returns EVM bytecode that forwards all calldata to `target` via CALL,
        /// forwarding the received CALLVALUE, then returns the returndata unchanged.
        fn wrapper_call_with_value_bytecode(target: Address) -> Bytes {
            #[rustfmt::skip]
            let mut code = vec![
                // CALLDATACOPY(destOffset=0, srcOffset=0, size=CALLDATASIZE)
                CALLDATASIZE,
                PUSH1, 0x00,      // srcOffset=0
                PUSH1, 0x00,      // destOffset=0
                CALLDATACOPY,
                // CALL(gas, target, value=CALLVALUE, argsOff=0, argsLen=CALLDATASIZE, retOff=0, retLen=0)
                PUSH1, 0x00,      // retLen=0
                PUSH1, 0x00,      // retOffset=0
                CALLDATASIZE,     // argsLen
                PUSH1, 0x00,      // argsOffset=0
                CALLVALUE,        // value
                PUSH20,           // target address follows
            ];
            code.extend_from_slice(target.as_slice());
            code.extend_from_slice(&[GAS, CALL, POP]);
            append_return_returndata(&mut code);
            Bytes::from(code)
        }

        /// Returns EVM bytecode that forwards all calldata to `target` via DELEGATECALL,
        /// then returns the returndata unchanged.
        fn delegatecall_wrapper_bytecode(target: Address) -> Bytes {
            #[rustfmt::skip]
            let mut code = vec![
                // CALLDATACOPY(destOffset=0, srcOffset=0, size=CALLDATASIZE)
                CALLDATASIZE,
                PUSH1, 0x00,      // srcOffset=0
                PUSH1, 0x00,      // destOffset=0
                CALLDATACOPY,
                // DELEGATECALL(gas, target, argsOff=0, argsLen=CALLDATASIZE, retOff=0, retLen=0)
                PUSH1, 0x00,      // retLen=0
                PUSH1, 0x00,      // retOffset=0
                CALLDATASIZE,     // argsLen
                PUSH1, 0x00,      // argsOffset=0
                PUSH20,           // target address follows
            ];
            code.extend_from_slice(target.as_slice());
            code.extend_from_slice(&[GAS, DELEGATECALL, POP]);
            append_return_returndata(&mut code);
            Bytes::from(code)
        }

        /// Contract A (allowlisted) calls CallFrom(sender=EOA, target=B, data).
        /// B returns successfully → A receives B's return data.
        #[test]
        fn test_call_from_happy_path() {
            let contract_a_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            let contract_b_code = echo_double_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const CONTRACT_B: Address = ECHO_CONTRACT;

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_A, contract_a_code), (CONTRACT_B, contract_b_code)],
                &[CONTRACT_A],
            );

            // A calls CallFrom(sender=EOA, target=B, data=abi(42))
            // The wrapper forwards its own msg.sender (EOA) as the sender parameter.
            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let call_from_input = encode_call_from_input(EOA, CONTRACT_B, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    let (success, return_data) = decode_call_from_output(output.data());
                    assert!(success, "callFrom should report child success");
                    let expected = U256::from(84).to_be_bytes::<32>();
                    assert_eq!(
                        return_data, expected,
                        "returnData should be echo_double(42) = 84"
                    );
                }
                other => panic!("expected Success, got {other:?}"),
            }
        }

        /// Verify that the child frame's gas consumption is propagated to the parent.
        ///
        /// Compares gas_used for two transactions through the same wrapper:
        /// 1. callFrom → echo_double (trivial work, ~20 gas in child)
        /// 2. callFrom → gas_burner (infinite loop, consumes all child gas)
        ///
        /// Both targets are pre-deployed contracts (same cold-access cost pattern).
        /// If child gas is properly propagated, the gas_burner transaction must use
        /// substantially more gas. If gas were NOT propagated (the old bug), both
        /// would show nearly identical gas_used.
        #[test]
        fn test_call_from_gas_propagated() {
            let contract_a_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            let contract_b_code = echo_double_bytecode();
            let gas_burner_code = gas_burner_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const CONTRACT_B: Address = ECHO_CONTRACT;
            const GAS_BURNER: Address = address!("c000000000000000000000000000000000000006");

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[
                    (CONTRACT_A, contract_a_code),
                    (CONTRACT_B, contract_b_code),
                    (GAS_BURNER, gas_burner_code),
                ],
                &[CONTRACT_A],
            );

            // Transaction 1: callFrom targeting echo_double (trivial child work)
            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let call_from_input_echo = encode_call_from_input(EOA, CONTRACT_B, &inner_calldata);

            let tx_echo = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 100_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input_echo,
                ..Default::default()
            };

            let result_echo = evm
                .transact_one(tx_echo)
                .expect("transact_one should succeed");
            let gas_used_echo = match &result_echo {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success, got {other:?}"),
            };

            // Transaction 2: callFrom targeting gas_burner (infinite loop, consumes all gas)
            let call_from_input_burner = encode_call_from_input(EOA, GAS_BURNER, &[]);

            let tx_burner = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 100_000,
                gas_price: 0,
                nonce: 1,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input_burner,
                ..Default::default()
            };

            let result_burner = evm
                .transact_one(tx_burner)
                .expect("transact_one should succeed");
            let gas_used_burner = match &result_burner {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success, got {other:?}"),
            };

            // The gas_burner call should use substantially more gas than echo_double.
            // With 100k gas_limit, the burner should consume nearly all of it (~90k+),
            // while echo_double uses only ~26k (base tx + wrapper + calldata + trivial child).
            assert!(
                gas_used_burner > gas_used_echo * 2,
                "callFrom to gas_burner ({gas_used_burner}) should use much more gas \
                 than callFrom to echo_double ({gas_used_echo})"
            );
        }

        /// Contract A (allowlisted) calls CallFrom(sender=EOA, target=B, data).
        /// B reverts → CallFrom propagates the revert, wrapper catches it.
        #[test]
        fn test_call_from_child_reverts() {
            let contract_a_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            let contract_b_code = reverting_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const CONTRACT_B: Address = REVERT_CONTRACT;

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_A, contract_a_code), (CONTRACT_B, contract_b_code)],
                &[CONTRACT_A],
            );

            let call_from_input = encode_call_from_input(EOA, CONTRACT_B, &[]);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            // CallFrom always returns successfully with ABI-encoded (bool, bytes).
            // The child reverted, so success=false and returnData contains the revert output.
            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    let (success, _return_data) = decode_call_from_output(output.data());
                    assert!(!success, "callFrom should report child failure");
                }
                other => panic!("expected Success, got {other:?}"),
            }
        }

        /// EOA directly calls CallFrom precompile. Since the default registry has an empty
        /// allowlist for CallFrom, this should revert.
        #[test]
        fn test_call_from_unauthorized_caller_rejected() {
            let echo_code = echo_double_bytecode();
            const CONTRACT_B: Address = ECHO_CONTRACT;

            // Empty allowlist — no one can call CallFrom
            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_B, echo_code)],
                &[],
            );

            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let call_from_input = encode_call_from_input(EOA, CONTRACT_B, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CALL_FROM_ADDRESS),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            assert!(
                matches!(result, ExecutionResult::Revert { .. }),
                "direct call from non-allowlisted address should revert"
            );
        }

        /// DELEGATECALL to CallFrom should revert — only CALL scheme is supported.
        #[test]
        fn test_call_from_delegatecall_rejected() {
            let contract_a_code = delegatecall_wrapper_bytecode(CALL_FROM_ADDRESS);
            let contract_b_code = echo_double_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const CONTRACT_B: Address = ECHO_CONTRACT;

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_A, contract_a_code), (CONTRACT_B, contract_b_code)],
                &[CONTRACT_A],
            );

            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let call_from_input = encode_call_from_input(EOA, CONTRACT_B, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            // The wrapper uses DELEGATECALL → CallFrom. The scheme check rejects it with a
            // revert. The wrapper catches the revert and returns the revert data.
            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    // The output is revert data from the scheme rejection.
                    // If echo_double had actually run, output would contain value 84.
                    let echo_result = U256::from(84).to_be_bytes::<32>();
                    assert_ne!(
                        output.data().as_ref(),
                        &echo_result,
                        "DELEGATECALL should not have reached the child contract"
                    );
                }
                other => panic!("expected Success (wrapper catches revert), got {other:?}"),
            }
        }

        /// Contract A (allowlisted) calls CallFrom with value attached.
        /// The framework rejects this with a revert. Asserts no value is lost:
        /// EOA balance unchanged (minus nothing — gas_price=0), A and B at zero.
        #[test]
        fn test_call_from_value_rejected() {
            let contract_a_code = wrapper_call_with_value_bytecode(CALL_FROM_ADDRESS);
            let contract_b_code = echo_double_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const CONTRACT_B: Address = ECHO_CONTRACT;
            let transfer_value = U256::from(1_000);
            let eoa_balance = U256::from(100_000);

            let mut evm = setup_test_evm(
                &[(EOA, eoa_balance)],
                &[(CONTRACT_A, contract_a_code), (CONTRACT_B, contract_b_code)],
                &[CONTRACT_A],
            );

            let inner_calldata = U256::from(7).to_be_bytes::<32>().to_vec();
            let call_from_input = encode_call_from_input(EOA, CONTRACT_B, &inner_calldata);

            evm.inner.ctx.set_tx(TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: transfer_value,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            });

            let result_and_state = evm.replay().expect("replay should succeed");

            // The wrapper catches the inner revert via CALL, so the top-level tx succeeds.
            // But the child contract should NOT have been reached.
            match &result_and_state.result {
                ExecutionResult::Success { output, .. } => {
                    let echo_result = U256::from(14).to_be_bytes::<32>();
                    assert_ne!(
                        output.data().as_ref(),
                        &echo_result,
                        "child contract should not have been reached"
                    );
                }
                other => panic!("expected Success (wrapper catches revert), got {other:?}"),
            }

            let state = &result_and_state.state;

            // EOA sent value to CONTRACT_A via the outer CALL, but the inner callFrom
            // reverted. The wrapper's CALL to the precompile returned 0 (failure),
            // but the wrapper itself doesn't revert — it just returns the revert data.
            // The outer EOA → CONTRACT_A transfer still happened.
            let eoa_account = state.get(&EOA).expect("EOA should be in state");
            assert_eq!(
                eoa_account.info.balance,
                eoa_balance - transfer_value,
                "EOA should be debited by the outer transfer to CONTRACT_A"
            );

            // CONTRACT_A received value from EOA but the inner callFrom reverted,
            // so the value stays in CONTRACT_A.
            let a_account = state
                .get(&CONTRACT_A)
                .expect("contract A should be in state");
            assert_eq!(
                a_account.info.balance, transfer_value,
                "contract A should hold the value (inner call reverted)"
            );

            // CONTRACT_B should not have received anything.
            let b_balance = state
                .get(&CONTRACT_B)
                .map(|a| a.info.balance)
                .unwrap_or(U256::ZERO);
            assert_eq!(b_balance, U256::ZERO, "contract B should have zero balance");
        }

        /// Nested callFrom targeting the CallFrom precompile address itself. The child frame
        /// calls CALL_FROM_ADDRESS via `init_with_context` (not our `frame_init`), so the
        /// subcall interception never fires. Since CALL_FROM_ADDRESS has no deployed code,
        /// revm returns an immediate empty success. This verifies the framework handles
        /// the code-less-address case correctly — the child "succeeds" with empty output,
        /// and echo_double is never invoked.
        #[test]
        fn test_call_from_target_codeless_precompile_address() {
            let contract_a_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            let contract_b_code = echo_double_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const CONTRACT_B: Address = ECHO_CONTRACT;

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_A, contract_a_code), (CONTRACT_B, contract_b_code)],
                &[CONTRACT_A],
            );

            let inner_calldata = U256::from(7).to_be_bytes::<32>().to_vec();
            let inner_call_from = encode_call_from_input(EOA, CONTRACT_B, &inner_calldata);

            // Outer callFrom targets CALL_FROM_ADDRESS. The child frame calls it via
            // init_with_context, which sees no code and returns immediate Stop.
            let outer_call_from = encode_call_from_input(EOA, CALL_FROM_ADDRESS, &inner_call_from);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: outer_call_from,
                ..Default::default()
            };

            // CALL_FROM_ADDRESS has no deployed code → init_with_context returns
            // immediate Stop. The outer callFrom returns (true, <empty>).
            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    let (success, return_data) = decode_call_from_output(output.data());
                    assert!(
                        success,
                        "outer callFrom should report child success (code-less address)"
                    );
                    assert!(
                        return_data.is_empty(),
                        "return data should be empty (CALL_FROM_ADDRESS has no deployed code)"
                    );
                }
                other => panic!("expected Success, got {other:?}"),
            }
        }

        /// CallFrom targets another subcall precompile address that has genesis stub
        /// bytecode (`0x01` = ADD opcode). The child frame bypasses the subcall registry
        /// (dispatched via `EthFrame::init_with_context`), executes the stub ADD opcode
        /// which stack-underflows, and reverts. The outer callFrom reports child failure.
        ///
        /// This mirrors production behavior where precompile addresses have `0x01` stub
        /// code in genesis. A subcall precompile's `init_subcall` child target must be a
        /// regular contract or standard precompile — targeting another subcall precompile
        /// address silently reverts via the stub bytecode instead of going through the
        /// subcall registry.
        #[test]
        fn test_call_from_target_subcall_precompile_with_stub_bytecode() {
            let contract_a_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            const CONTRACT_A: Address = WRAPPER;

            // Deploy stub bytecode (0x01 = ADD) at the subcall test precompile address,
            // simulating the genesis allocation for custom precompile addresses.
            let stub_bytecode = Bytes::from(vec![0x01]);

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[
                    (CONTRACT_A, contract_a_code),
                    (SUBCALL_TEST_ADDRESS, stub_bytecode),
                ],
                &[CONTRACT_A],
            );

            // callFrom(sender=EOA, target=SUBCALL_TEST_ADDRESS, data=<arbitrary>)
            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let call_from_input =
                encode_call_from_input(EOA, SUBCALL_TEST_ADDRESS, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            // The child frame targets SUBCALL_TEST_ADDRESS, but it is dispatched via
            // init_with_context (not the subcall registry). It finds the 0x01 stub code,
            // executes ADD with an empty stack, stack-underflows, and reverts.
            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    let (success, _return_data) = decode_call_from_output(output.data());
                    assert!(
                        !success,
                        "child targeting subcall precompile stub bytecode should revert"
                    );
                }
                other => panic!("expected Success (wrapper catches revert), got {other:?}"),
            }
        }

        /// Returns EVM bytecode that increments a counter in storage slot 0, then
        /// forwards all calldata to `target` via CALL (value=0). On return, copies
        /// returndata to memory and returns it.
        fn counting_wrapper_bytecode(target: Address) -> Bytes {
            #[rustfmt::skip]
            let mut code = vec![
                // counter = SLOAD(0) + 1; SSTORE(0, counter)
                PUSH1, 0x00,      // slot 0
                SLOAD,            // load current counter
                PUSH1, 0x01,
                ADD,              // counter + 1
                PUSH1, 0x00,      // slot 0
                SSTORE,           // store updated counter

                // CALLDATACOPY(destOffset=0, srcOffset=0, size=CALLDATASIZE)
                CALLDATASIZE,
                PUSH1, 0x00,      // srcOffset=0
                PUSH1, 0x00,      // destOffset=0
                CALLDATACOPY,

                // CALL(gas, target, value=0, argsOff=0, argsLen=CALLDATASIZE, retOff=0, retLen=0)
                PUSH1, 0x00,      // retLen=0
                PUSH1, 0x00,      // retOffset=0
                CALLDATASIZE,     // argsLen
                PUSH1, 0x00,      // argsOffset=0
                PUSH1, 0x00,      // value=0
                PUSH20,           // target address follows
            ];
            code.extend_from_slice(target.as_slice());
            code.extend_from_slice(&[GAS, CALL, POP]);
            append_return_returndata(&mut code);
            Bytes::from(code)
        }

        /// Indirect recursion: CONTRACT_A → callFrom → CONTRACT_A → callFrom → ... until the
        /// EVM call stack depth limit (1024) is hit. CONTRACT_A increments a storage counter
        /// on each invocation so we can observe the actual recursion depth.
        #[test]
        fn test_call_from_recursive_depth_limit() {
            let contract_a_code = counting_wrapper_bytecode(CALL_FROM_ADDRESS);
            let contract_b_code = echo_double_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const CONTRACT_B: Address = ECHO_CONTRACT;

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_A, contract_a_code), (CONTRACT_B, contract_b_code)],
                &[CONTRACT_A],
            );

            // Build a deeply nested payload: 1000 layers of callFrom(EOA, CONTRACT_A, <next>)
            // wrapping a base case of callFrom(EOA, CONTRACT_B, abi(7)). Each layer causes
            // CONTRACT_A to be invoked and increment its counter. The depth limit (1024)
            // should cut off recursion before we exhaust all 1000 layers.
            let base_calldata = U256::from(7).to_be_bytes::<32>().to_vec();
            let mut payload = encode_call_from_input(EOA, CONTRACT_B, &base_calldata);
            for _ in 0..1000 {
                payload = encode_call_from_input(EOA, CONTRACT_A, &payload);
            }

            // With EIP-150 63/64ths gas forwarding, gas shrinks geometrically at each level.
            // Use u64::MAX so that gas is never the limiting factor — the 1024 depth limit
            // should be what stops recursion.
            evm.inner.ctx.set_tx(TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: u64::MAX,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: payload,
                ..Default::default()
            });

            let result_and_state = evm.replay().expect("replay should succeed");

            // The recursion eventually hits CallTooDeep. Each level's wrapper catches
            // the revert from the level below, so the top-level tx succeeds.
            assert!(
                matches!(result_and_state.result, ExecutionResult::Success { .. }),
                "recursive callFrom should succeed (wrapper catches depth-limit revert)"
            );

            // Read the counter from CONTRACT_A's storage slot 0 to observe recursion depth.
            // Each cycle: CONTRACT_A at depth D does CALL → intercepted at D+1 → child
            // CONTRACT_A created at D+2. So each invocation consumes 2 depth levels.
            let state = &result_and_state.state;
            let a_account = state
                .get(&CONTRACT_A)
                .expect("contract A should be in state");
            let counter = a_account
                .storage
                .get(&U256::ZERO)
                .map(|slot| slot.present_value)
                .unwrap_or(U256::ZERO);

            // Each recursion cycle uses 2 depth levels (one for the intercepted callFrom frame,
            // one for the child CONTRACT_A frame). With a limit of 1024 and initial depth 0,
            // CONTRACT_A runs at depths 0, 2, 4, ..., 1024 — that's 513 invocations.
            assert_eq!(
                counter,
                U256::from(513),
                "expected 513 invocations (depths 0, 2, 4, ..., 1024)"
            );
        }

        /// Returns EVM bytecode that consumes all gas via an infinite loop.
        fn gas_burner_bytecode() -> Bytes {
            #[rustfmt::skip]
            let code = vec![
                // JUMPDEST at offset 0; JUMP back to 0
                JUMPDEST,         // offset 0
                PUSH1, 0x00,      // jump target = 0
                JUMP,
            ];
            Bytes::from(code)
        }

        /// Returns EVM bytecode that makes two CALLs to `target` with different payloads,
        /// discards the first result, and returns the second call's returndata.
        ///
        /// Calldata layout: `[len1: 32 bytes][payload1: len1 bytes][payload2: rest]`
        fn double_call_wrapper_bytecode(target: Address) -> Bytes {
            #[rustfmt::skip]
            let mut code = vec![
                // len1 = calldata[0..32]
                PUSH1, 0x00,
                CALLDATALOAD,     // [len1]

                // Copy payload1 to mem[0x00..]
                DUP1,             // [len1, len1]
                PUSH1, 0x20,      // [0x20, len1, len1]
                PUSH1, 0x00,      // [0x00, 0x20, len1, len1]
                CALLDATACOPY,     // mem[0x00..len1] = calldata[0x20..0x20+len1]; stack: [len1]

                // CALL 1(gas, target, 0, 0, len1, 0, 0) — result discarded
                PUSH1, 0x00,      // retLen
                PUSH1, 0x00,      // retOff
                DUP3,             // argsLen = len1
                PUSH1, 0x00,      // argsOff
                PUSH1, 0x00,      // value
                PUSH20,
            ];
            code.extend_from_slice(target.as_slice());
            #[rustfmt::skip]
            code.extend_from_slice(&[
                GAS,
                CALL,             // [success, len1]
                POP,              // [len1]

                // src2 = 0x20 + len1; len2 = CALLDATASIZE - src2
                PUSH1, 0x20,
                ADD,              // [src2]
                CALLDATASIZE,     // [CDS, src2]
                DUP2,             // [src2, CDS, src2]
                SWAP1,            // [CDS, src2, src2]
                SUB,              // [len2, src2]

                // Copy payload2 to mem[0x00..]
                DUP1,             // [len2, len2, src2]
                DUP3,             // [src2, len2, len2, src2]
                PUSH1, 0x00,      // [0x00, src2, len2, len2, src2]
                CALLDATACOPY,     // mem[0x00..len2] = calldata[src2..]; stack: [len2, src2]
                SWAP1,            // [src2, len2]
                POP,              // [len2]

                // CALL 2(gas, target, 0, 0, len2, 0, 0)
                PUSH1, 0x00,      // retLen
                PUSH1, 0x00,      // retOff
                DUP3,             // argsLen = len2
                PUSH1, 0x00,      // argsOff
                PUSH1, 0x00,      // value
                PUSH20,
            ]);
            code.extend_from_slice(target.as_slice());
            code.extend_from_slice(&[GAS, CALL, POP, POP]); // discard success and len2
            append_return_returndata(&mut code);
            Bytes::from(code)
        }

        /// callFrom targeting an EOA (no bytecode). init_with_context returns an immediate
        /// success (Stop) for empty code. Verifies the subcall framework handles
        /// immediate success results correctly (continuation consumed, no stale state).
        #[test]
        fn test_call_from_target_eoa() {
            let contract_a_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            const CONTRACT_A: Address = WRAPPER;
            const TARGET_EOA: Address = address!("e000000000000000000000000000000000000002");

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000)), (TARGET_EOA, U256::ZERO)],
                &[(CONTRACT_A, contract_a_code)],
                &[CONTRACT_A],
            );

            let call_from_input = encode_call_from_input(EOA, TARGET_EOA, &[0xab; 4]);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            // The child frame targets an EOA (no code) — init_with_context returns
            // immediate success with empty output.
            let result = evm.transact_one(tx).expect("transact_one should succeed");
            assert!(
                matches!(result, ExecutionResult::Success { .. }),
                "callFrom targeting an EOA should succeed, got {result:?}"
            );
        }

        /// callFrom targeting the ecrecover precompile (address 0x01). The child frame
        /// is handled as a standard precompile by revm's init_with_context, returning
        /// an immediate result. Verifies interop between subcall and standard precompiles.
        #[test]
        fn test_call_from_target_standard_precompile() {
            let contract_a_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            const CONTRACT_A: Address = WRAPPER;
            const ECRECOVER: Address = address!("0000000000000000000000000000000000000001");

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_A, contract_a_code)],
                &[CONTRACT_A],
            );

            // Provide invalid ecrecover input — the precompile will return empty/error,
            // but the call itself should not panic or leave stale state.
            let call_from_input = encode_call_from_input(EOA, ECRECOVER, &[0x00; 128]);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            // The wrapper catches any inner failure. The key property: no panics,
            // no stale continuations, transaction completes cleanly.
            let result = evm.transact_one(tx).expect("transact_one should succeed");
            assert!(
                matches!(result, ExecutionResult::Success { .. }),
                "callFrom targeting ecrecover should complete cleanly, got {result:?}"
            );
        }

        /// callFrom child runs out of gas (OOG). The child is an infinite loop that
        /// consumes all forwarded gas. complete_subcall should see the halt and propagate
        /// failure to the wrapper.
        #[test]
        fn test_call_from_child_oog() {
            let contract_a_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            let gas_burner_code = gas_burner_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const GAS_BURNER: Address = address!("c000000000000000000000000000000000000006");

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_A, contract_a_code), (GAS_BURNER, gas_burner_code)],
                &[CONTRACT_A],
            );

            let call_from_input = encode_call_from_input(EOA, GAS_BURNER, &[]);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            // The child burns all gas and halts with OOG. The child's own checkpoint is
            // reverted by process_next_action. The wrapper catches the failure via CALL
            // return value (0), so the top-level tx succeeds.
            let result = evm.transact_one(tx).expect("transact_one should succeed");
            assert!(
                matches!(result, ExecutionResult::Success { .. }),
                "wrapper should catch OOG child failure, got {result:?}"
            );
        }

        /// Child OOG should burn the full subcall gas allocation (including the retained
        /// 1/64th), matching EVM CALL semantics.
        ///
        /// Compares a callFrom-to-gas-burner (child OOGs) against a direct
        /// wrapper-to-gas-burner CALL (standard EVM OOG). Both wrappers are structurally
        /// identical (same opcodes, same calldata size), so the wrapper-level gas overhead
        /// is the same. Any difference in gas_used is due to the subcall precompile's
        /// extra layer (init overhead + second 63/64ths split).
        ///
        /// The key assertion: the callFrom OOG path must consume *at least* as much gas
        /// as the direct CALL OOG path. If the subcall leaked the 1/64th back, the
        /// callFrom path would consume *less* gas than the direct CALL (because the
        /// leaked 1/64th would be returned to the wrapper).
        #[test]
        fn test_call_from_child_oog_burns_full_allocation() {
            // Wrapper A: calls CallFrom precompile → CallFrom calls gas burner (child OOGs)
            let wrapper_via_callfrom = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            // Wrapper B: calls gas burner directly (standard EVM CALL, child OOGs)
            let wrapper_direct = wrapper_call_bytecode(GAS_BURNER);
            let gas_burner_code = gas_burner_bytecode();
            const WRAPPER_VIA_CALLFROM: Address = WRAPPER;
            const WRAPPER_DIRECT: Address = address!("c000000000000000000000000000000000000009");
            const GAS_BURNER: Address = address!("c000000000000000000000000000000000000006");

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[
                    (WRAPPER_VIA_CALLFROM, wrapper_via_callfrom),
                    (WRAPPER_DIRECT, wrapper_direct),
                    (GAS_BURNER, gas_burner_code),
                ],
                &[WRAPPER_VIA_CALLFROM],
            );

            let gas_limit: u64 = 500_000;

            // Tx 1: callFrom → gas burner (child OOGs inside subcall framework)
            let call_from_input = encode_call_from_input(EOA, GAS_BURNER, &[]);
            let tx_callfrom = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER_VIA_CALLFROM),
                value: U256::ZERO,
                gas_limit,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };
            let result_callfrom = evm.transact_one(tx_callfrom).expect("callfrom tx");
            let gas_used_callfrom = match &result_callfrom {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success (wrapper catches OOG), got {other:?}"),
            };

            // Tx 2: direct CALL → gas burner (standard EVM OOG, no subcall layer)
            let tx_direct = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER_DIRECT),
                value: U256::ZERO,
                gas_limit,
                gas_price: 0,
                nonce: 1,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: Bytes::new(),
                ..Default::default()
            };
            let result_direct = evm.transact_one(tx_direct).expect("direct tx");
            let gas_used_direct = match &result_direct {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success, got {other:?}"),
            };

            // Both wrappers forward the same amount of gas G to their CALL target.
            // In the direct path, the child receives G and OOGs — the wrapper's CALL
            // reports G spent. In the callFrom path, the subcall precompile deducts
            // init overhead, then forwards (G - overhead) * 63/64 to the child. The
            // child OOGs and burns that. With correct gas accounting the subcall reports
            // the full G as spent (overhead + child + retained 1/64th), matching the
            // direct path.
            //
            // If the subcall leaked the retained 1/64th back to the wrapper,
            // gas_used_callfrom would be less than gas_used_direct — that's the bug
            // this test catches.
            assert!(
                gas_used_callfrom >= gas_used_direct,
                "callFrom OOG ({gas_used_callfrom}) should consume at least as much gas \
                 as direct CALL OOG ({gas_used_direct})"
            );
        }

        /// Two sequential callFrom calls from the same wrapper in one transaction, each
        /// with different parameters. Verifies that continuations keyed by depth don't
        /// collide — each call stores and consumes its own continuation independently.
        /// The wrapper discards the first result and returns the second call's returndata.
        #[test]
        fn test_call_from_sequential_calls() {
            let contract_a_code = double_call_wrapper_bytecode(CALL_FROM_ADDRESS);
            let contract_b_code = echo_double_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const CONTRACT_B: Address = ECHO_CONTRACT;

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_A, contract_a_code), (CONTRACT_B, contract_b_code)],
                &[CONTRACT_A],
            );

            // Call 1: echo_double(7) = 14 (result discarded by wrapper)
            let payload1 =
                encode_call_from_input(EOA, CONTRACT_B, &U256::from(7).to_be_bytes::<32>());
            // Call 2: echo_double(42) = 84 (result returned by wrapper)
            let payload2 =
                encode_call_from_input(EOA, CONTRACT_B, &U256::from(42).to_be_bytes::<32>());

            // Calldata layout: [len1: 32 bytes][payload1][payload2]
            let len1 = U256::from(payload1.len());
            let mut calldata = len1.to_be_bytes::<32>().to_vec();
            calldata.extend_from_slice(&payload1);
            calldata.extend_from_slice(&payload2);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: Bytes::from(calldata),
                ..Default::default()
            };

            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    // Wrapper returns the second call's returndata (ABI-encoded callFrom output)
                    let (success, return_data) = decode_call_from_output(output.data());
                    assert!(success, "second callFrom should report child success");

                    let value = U256::from_be_slice(&return_data);
                    assert_eq!(value, U256::from(84), "echo_double(42) should return 84");
                }
                other => panic!("expected Success, got {other:?}"),
            }
        }

        /// STATICCALL to CallFrom should revert — static context is not supported.
        #[test]
        fn test_call_from_staticcall_rejected() {
            let contract_a_code = static_call_wrapper_bytecode(CALL_FROM_ADDRESS);
            let contract_b_code = echo_double_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const CONTRACT_B: Address = ECHO_CONTRACT;

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[(CONTRACT_A, contract_a_code), (CONTRACT_B, contract_b_code)],
                &[CONTRACT_A],
            );

            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let call_from_input = encode_call_from_input(EOA, CONTRACT_B, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            // The wrapper uses STATICCALL → CallFrom. The scheme check rejects it with a
            // revert. The wrapper catches the revert and returns the revert data.
            let result = evm.transact_one(tx).expect("transact_one should succeed");
            match &result {
                ExecutionResult::Success { output, .. } => {
                    // The output is revert data from the scheme rejection.
                    // If echo_double had actually run, output would contain value 84.
                    // Instead, the data should NOT contain the echo result, confirming
                    // the subcall never reached the child contract.
                    let echo_result = U256::from(84).to_be_bytes::<32>();
                    assert_ne!(
                        output.data().as_ref(),
                        &echo_result,
                        "STATICCALL should not have reached the child contract"
                    );
                }
                other => panic!("expected Success (wrapper catches revert), got {other:?}"),
            }
        }

        // ----- Tracing / Inspector tests -----

        /// Creates an ArcEvm identical to `setup_test_evm` but with a generic inspector
        /// and the `inspect` flag set to `true` so `inspect_one_tx` uses the inspector path.
        fn setup_test_evm_with_inspector<
            INSP: revm::inspector::Inspector<EthEvmContext<InMemoryDB>, EthInterpreter>,
        >(
            accounts: &[(Address, U256)],
            contracts: &[(Address, Bytes)],
            call_from_allowlist: &[Address],
            inspector: INSP,
        ) -> ArcEvm<
            EthEvmContext<InMemoryDB>,
            INSP,
            EthInstructions<EthInterpreter, EthEvmContext<InMemoryDB>>,
            PrecompilesMap,
        > {
            use crate::subcall::AllowedCallers;
            use crate::subcall_test::{SubcallTestPrecompile, SUBCALL_TEST_ADDRESS};

            let chain_spec = LOCAL_DEV.clone();
            let mut db = InMemoryDB::default();

            for (addr, balance) in accounts {
                db.insert_account_info(
                    *addr,
                    AccountInfo {
                        balance: *balance,
                        nonce: 0,
                        code_hash: alloy_primitives::KECCAK256_EMPTY,
                        code: None,
                        account_id: None,
                    },
                );
            }

            for (addr, code) in contracts {
                db.insert_account_info(
                    *addr,
                    AccountInfo {
                        balance: U256::ZERO,
                        nonce: 1,
                        code_hash: keccak256(code),
                        code: Some(Bytecode::new_raw(code.clone())),
                        account_id: None,
                    },
                );
            }

            let spec =
                reth_ethereum::evm::revm_spec_by_timestamp_and_block_number(&chain_spec, 0, 0);
            let hardfork_flags = chain_spec.get_hardfork_flags(0);
            let mut cfg_env = revm::context::CfgEnv::new()
                .with_chain_id(chain_spec.chain_id())
                .with_spec_and_mainnet_gas_params(spec);
            cfg_env.tx_gas_limit_cap = Some(u64::MAX);
            let ctx = EthEvmContext::new(db, spec)
                .with_cfg(cfg_env)
                .with_block(BlockEnv::default());
            let precompiles = ArcPrecompileProvider::create_precompiles_map(spec, hardfork_flags);
            let mut instruction = EthInstructions::default();
            if hardfork_flags.is_active(ArcHardfork::Zero5) {
                instruction.insert_instruction(
                    SELFDESTRUCT,
                    revm_interpreter::Instruction::new(arc_network_selfdestruct, 5000),
                );
            }

            let mut registry = SubcallRegistry::new();
            registry.register(
                SUBCALL_TEST_ADDRESS,
                Arc::new(SubcallTestPrecompile),
                AllowedCallers::Unrestricted,
            );
            let allowed_callers =
                AllowedCallers::Only(HashSet::from_iter(call_from_allowlist.iter().copied()));
            registry.register(
                CALL_FROM_ADDRESS,
                Arc::new(CallFromPrecompile),
                allowed_callers,
            );

            ArcEvm::new(
                ctx,
                inspector,
                precompiles,
                instruction,
                true, // enable inspect path
                hardfork_flags,
                Arc::new(registry),
            )
        }

        /// Verifies that `debug_traceTransaction` with `callTracer` produces a transparent
        /// trace for subcall precompiles: the CallFrom precompile is invisible, and the trace
        /// shows the logical child call (spoofed_sender → target) with inner frames nested.
        ///
        /// Call chain: EOA → wrapper → CallFrom → forwarder → echo_double(42)
        ///
        /// Expected trace (precompile is transparent):
        ///   CALL: EOA → wrapper
        ///     └─ CALL: EOA → forwarder          (the spoofed child call)
        ///          └─ CALL: forwarder → echo     (forwarder calls echo_double)
        ///
        /// Also serves as a regression test for the original "Disconnected trace" panic
        /// (checkpoint depth gap).
        #[test]
        fn test_call_from_nested_call_with_tracing_inspector() {
            use alloy_rpc_types_trace::geth::call::CallConfig;
            use revm_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

            // forwarder: a contract that forwards calldata to echo_double via CALL
            let forwarder_code = wrapper_call_bytecode(ECHO_CONTRACT);
            let echo_code = echo_double_bytecode();
            let wrapper_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            const CONTRACT_WRAPPER: Address = WRAPPER;
            const CONTRACT_FORWARDER: Address =
                address!("c000000000000000000000000000000000000003");
            const CONTRACT_ECHO: Address = ECHO_CONTRACT;

            let call_config = CallConfig {
                with_log: Some(true),
                only_top_call: Some(false),
            };
            let inspector =
                TracingInspector::new(TracingInspectorConfig::from_geth_call_config(&call_config));

            let mut evm = setup_test_evm_with_inspector(
                &[(EOA, U256::from(1_000_000))],
                &[
                    (CONTRACT_WRAPPER, wrapper_code),
                    (CONTRACT_FORWARDER, forwarder_code),
                    (CONTRACT_ECHO, echo_code),
                ],
                &[CONTRACT_WRAPPER], // wrapper is allowed to call CallFrom
                inspector,
            );

            // Inner calldata: the forwarder will forward this to echo_double
            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            // CallFrom(sender=EOA, target=forwarder, data=inner_calldata)
            let call_from_input = encode_call_from_input(EOA, CONTRACT_FORWARDER, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            // Must use inspect_one_tx (not transact_one) because transact_one uses
            // run_exec_loop which never calls inspector.call/call_end. The inspect
            // path (inspect_run_exec_loop → inspect_frame_init → frame_start) is
            // what triggers the TracingInspector's depth tracking.
            let result = evm
                .inspect_one_tx(tx)
                .expect("inspect_one_tx should succeed");
            let gas_used = match &result {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success, got {other:?}"),
            };

            // Convert trace to geth call format and verify structure
            let frame = evm
                .inner
                .inspector
                .with_transaction_gas_limit(1_000_000)
                .into_geth_builder()
                .geth_call_traces(call_config, gas_used);

            // Top-level frame: EOA → wrapper
            assert_eq!(frame.from, EOA, "top frame: from should be EOA");
            assert_eq!(
                frame.to,
                Some(CONTRACT_WRAPPER),
                "top frame: to should be wrapper"
            );

            // The wrapper's only call should be the transparent child call
            // (spoofed_sender → forwarder), not the precompile call.
            assert_eq!(
                frame.calls.len(),
                1,
                "wrapper should have exactly 1 child call (the transparent subcall)"
            );
            let child = &frame.calls[0];
            assert_eq!(
                child.from, EOA,
                "child frame: from should be the spoofed sender (EOA)"
            );
            assert_eq!(
                child.to,
                Some(CONTRACT_FORWARDER),
                "child frame: to should be the forwarder"
            );

            // The forwarder calls echo_double — this should appear as a nested call.
            assert_eq!(
                child.calls.len(),
                1,
                "forwarder should have exactly 1 nested call (to echo_double)"
            );
            let grandchild = &child.calls[0];
            assert_eq!(
                grandchild.from, CONTRACT_FORWARDER,
                "grandchild frame: from should be forwarder"
            );
            assert_eq!(
                grandchild.to,
                Some(CONTRACT_ECHO),
                "grandchild frame: to should be echo contract"
            );
        }

        /// When CallFrom receives malformed calldata, `trace_child_call` returns `None`.
        /// The trace should fall back to showing the precompile address, not the logical
        /// child call. The execution itself reverts (ABI decode failure in `init_subcall`).
        #[test]
        fn test_call_from_malformed_calldata_trace_shows_precompile() {
            use alloy_rpc_types_trace::geth::call::CallConfig;
            use revm_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

            let wrapper_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);

            let call_config = CallConfig {
                with_log: Some(true),
                only_top_call: Some(false),
            };
            let inspector =
                TracingInspector::new(TracingInspectorConfig::from_geth_call_config(&call_config));

            let mut evm = setup_test_evm_with_inspector(
                &[(EOA, U256::from(1_000_000))],
                &[(WRAPPER, wrapper_code)],
                &[WRAPPER],
                inspector,
            );

            // Send garbage calldata — not a valid callFrom ABI encoding.
            let garbage_data = Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: garbage_data,
                ..Default::default()
            };

            let result = evm
                .inspect_one_tx(tx)
                .expect("inspect_one_tx should succeed");
            let gas_used = match &result {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success, got {other:?}"),
            };

            let frame = evm
                .inner
                .inspector
                .into_geth_builder()
                .geth_call_traces(call_config, gas_used);

            // Top-level: EOA → wrapper
            assert_eq!(frame.from, EOA);
            assert_eq!(frame.to, Some(WRAPPER));

            // The child call should show the precompile address (fallback behavior),
            // since trace_child_call returned None for the malformed input.
            assert_eq!(
                frame.calls.len(),
                1,
                "wrapper should have exactly 1 child call"
            );
            let child = &frame.calls[0];
            assert_eq!(
                child.from, WRAPPER,
                "malformed calldata: trace should show precompile address (fallback)"
            );
            assert_eq!(
                child.to,
                Some(CALL_FROM_ADDRESS),
                "malformed calldata: trace should show precompile address (fallback)"
            );
        }

        /// When CallFrom targets an EOA (no bytecode), the child call completes
        /// immediately without spawning an interpreter frame. This exercises the
        /// `ItemOrResult::Result` branch in `inspect_frame_init_impl`.
        #[test]
        fn test_call_from_eoa_target_trace() {
            use alloy_rpc_types_trace::geth::call::CallConfig;
            use revm_inspectors::tracing::{TracingInspector, TracingInspectorConfig};

            let wrapper_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            const TARGET_EOA: Address = address!("e000000000000000000000000000000000000099");

            let call_config = CallConfig {
                with_log: Some(true),
                only_top_call: Some(false),
            };
            let inspector =
                TracingInspector::new(TracingInspectorConfig::from_geth_call_config(&call_config));

            let mut evm = setup_test_evm_with_inspector(
                &[(EOA, U256::from(1_000_000)), (TARGET_EOA, U256::ZERO)],
                &[(WRAPPER, wrapper_code)],
                &[WRAPPER],
                inspector,
            );

            let inner_calldata = vec![];
            let call_from_input = encode_call_from_input(EOA, TARGET_EOA, &inner_calldata);

            let tx = TxEnv {
                caller: EOA,
                kind: TxKind::Call(WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_input,
                ..Default::default()
            };

            let result = evm
                .inspect_one_tx(tx)
                .expect("inspect_one_tx should succeed");
            let gas_used = match &result {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success, got {other:?}"),
            };

            let frame = evm
                .inner
                .inspector
                .into_geth_builder()
                .geth_call_traces(call_config, gas_used);

            // Top-level: EOA → wrapper
            assert_eq!(frame.from, EOA);
            assert_eq!(frame.to, Some(WRAPPER));

            // The child call should show the logical call (EOA → TARGET_EOA),
            // transparently — even though the EOA has no code and completes immediately.
            assert_eq!(
                frame.calls.len(),
                1,
                "wrapper should have exactly 1 child call"
            );
            let child = &frame.calls[0];
            assert_eq!(
                child.from, EOA,
                "child frame: from should be the spoofed sender"
            );
            assert_eq!(
                child.to,
                Some(TARGET_EOA),
                "child frame: to should be the EOA target"
            );
            // EOA target has no code, so no nested calls.
            assert_eq!(
                child.calls.len(),
                0,
                "EOA target should have no nested calls"
            );
        }

        // ----- Gas accounting tests -----

        /// Verifies CallFromPrecompile::init_subcall applies EIP-150 63/64ths gas forwarding.
        /// Direct unit test — no EVM execution, just the init_subcall calculation.
        #[test]
        fn test_call_from_init_subcall_gas_calculation() {
            use alloy_sol_types::SolCall;
            use arc_precompiles::call_from::{abi_decode_gas, ICallFrom};
            use arc_precompiles::subcall::SubcallPrecompile;

            let precompile = CallFromPrecompile;
            let gas_limit: u64 = 100_000;
            let child_data: Vec<u8> = vec![0x42];

            let calldata = ICallFrom::callFromCall {
                sender: EOA,
                target: ECHO_CONTRACT,
                data: child_data.clone().into(),
            }
            .abi_encode();

            let inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: CALL_FROM_ADDRESS,
                bytecode_address: CALL_FROM_ADDRESS,
                known_bytecode: None,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(Bytes::from(calldata)),
                gas_limit,
                is_static: false,
                caller: WRAPPER,
                return_memory_offset: 0..0,
            };

            let result = precompile
                .init_subcall(&inputs)
                .expect("init_subcall should succeed");

            let overhead = abi_decode_gas(child_data.len());
            let expected_available = gas_limit - overhead;
            let expected_child_gas = expected_available - (expected_available / 64);

            assert_eq!(
                result.child_inputs.gas_limit, expected_child_gas,
                "child gas_limit should be (gas_limit - overhead) * 63/64"
            );
            assert_eq!(result.gas_overhead, overhead);
        }

        /// init_subcall should error when gas_limit is less than the ABI decode overhead.
        #[test]
        fn test_call_from_init_subcall_insufficient_gas() {
            use alloy_sol_types::SolCall;
            use arc_precompiles::call_from::{abi_decode_gas, ICallFrom};
            use arc_precompiles::subcall::SubcallPrecompile;

            let precompile = CallFromPrecompile;
            let child_data: Vec<u8> = vec![0xAB; 64]; // 2 words of data

            let calldata = ICallFrom::callFromCall {
                sender: EOA,
                target: ECHO_CONTRACT,
                data: child_data.clone().into(),
            }
            .abi_encode();

            let inputs = CallInputs {
                scheme: CallScheme::Call,
                target_address: CALL_FROM_ADDRESS,
                bytecode_address: CALL_FROM_ADDRESS,
                known_bytecode: None,
                value: CallValue::Transfer(U256::ZERO),
                input: CallInput::Bytes(Bytes::from(calldata)),
                gas_limit: abi_decode_gas(child_data.len()) - 1, // Not enough
                is_static: false,
                caller: WRAPPER,
                return_memory_offset: 0..0,
            };

            let result = precompile.init_subcall(&inputs);
            assert!(
                result.is_err(),
                "init_subcall should fail with insufficient gas"
            );
        }

        /// Bytecode: SSTORE(slot=0, value=1) then SSTORE(slot=0, value=0) to trigger a refund,
        /// then RETURN empty. The SSTORE 1→0 transition earns a gas refund.
        fn sstore_refund_bytecode() -> Bytes {
            #[rustfmt::skip]
            let code = vec![
                PUSH1, 0x01, PUSH1, 0x00, SSTORE,  // SSTORE(0, 1)
                PUSH1, 0x00, PUSH1, 0x00, SSTORE,  // SSTORE(0, 0) — refund
                PUSH1, 0x00, PUSH1, 0x00, RETURN,   // RETURN(0, 0)
            ];
            Bytes::from(code)
        }

        /// Bytecode: SSTORE(slot=0, value=1) then SSTORE(slot=0, value=0) to trigger a refund,
        /// then REVERT.
        fn sstore_refund_then_revert_bytecode() -> Bytes {
            #[rustfmt::skip]
            let code = vec![
                PUSH1, 0x01, PUSH1, 0x00, SSTORE,  // SSTORE(0, 1)
                PUSH1, 0x00, PUSH1, 0x00, SSTORE,  // SSTORE(0, 0) — refund
                PUSH1, 0x00, PUSH1, 0x00, REVERT,   // REVERT(0, 0)
            ];
            Bytes::from(code)
        }

        /// SSTORE refunds are forwarded on child success but NOT on child revert.
        #[test]
        fn test_call_from_gas_refund_on_success_not_revert() {
            let contract_a_code = wrapper_call_bytecode(CALL_FROM_ADDRESS);
            let refund_success_code = sstore_refund_bytecode();
            let refund_revert_code = sstore_refund_then_revert_bytecode();
            const CONTRACT_A: Address = WRAPPER;
            const REFUND_SUCCESS: Address = address!("c000000000000000000000000000000000000007");
            const REFUND_REVERT: Address = address!("c000000000000000000000000000000000000008");

            let mut evm = setup_test_evm(
                &[(EOA, U256::from(1_000_000))],
                &[
                    (CONTRACT_A, contract_a_code),
                    (REFUND_SUCCESS, refund_success_code),
                    (REFUND_REVERT, refund_revert_code),
                ],
                &[CONTRACT_A],
            );

            // Tx 1: child succeeds with SSTORE refund
            let call_from_success = encode_call_from_input(EOA, REFUND_SUCCESS, &[]);
            let tx_success = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_success,
                ..Default::default()
            };
            let result_success = evm.transact_one(tx_success).expect("success tx");
            let gas_used_success = match &result_success {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success, got {other:?}"),
            };

            // Tx 2: child reverts after earning SSTORE refund (refund should be discarded)
            let call_from_revert = encode_call_from_input(EOA, REFUND_REVERT, &[]);
            let tx_revert = TxEnv {
                caller: EOA,
                kind: TxKind::Call(CONTRACT_A),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                nonce: 1,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: call_from_revert,
                ..Default::default()
            };
            let result_revert = evm.transact_one(tx_revert).expect("revert tx");
            let gas_used_revert = match &result_revert {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success (wrapper catches revert), got {other:?}"),
            };

            // The success path should use LESS gas than the revert path because the
            // SSTORE refund is applied on success but discarded on revert.
            assert!(
                gas_used_success < gas_used_revert,
                "success ({gas_used_success}) should use less gas than revert ({gas_used_revert}) \
                 due to SSTORE refund being forwarded only on success"
            );
        }

        /// complete_subcall error should consume all gas allocated to the subcall.
        #[test]
        fn test_complete_subcall_error_consumes_all_gas() {
            use crate::subcall::AllowedCallers;
            use crate::subcall_test::{
                FailingCompleteSubcallPrecompile, SubcallTestPrecompile,
                FAILING_COMPLETE_SUBCALL_ADDRESS, SUBCALL_TEST_ADDRESS,
            };

            let wrapper_code = wrapper_call_bytecode(FAILING_COMPLETE_SUBCALL_ADDRESS);
            let echo_code = echo_double_bytecode();
            let normal_wrapper_code = wrapper_call_bytecode(SUBCALL_TEST_ADDRESS);
            const FAILING_WRAPPER: Address = WRAPPER;
            const NORMAL_WRAPPER: Address = address!("c000000000000000000000000000000000000010");

            let chain_spec = LOCAL_DEV.clone();
            let mut db = InMemoryDB::default();
            db.insert_account_info(
                EOA,
                AccountInfo {
                    balance: U256::from(1_000_000),
                    nonce: 0,
                    code_hash: alloy_primitives::KECCAK256_EMPTY,
                    code: None,
                    account_id: None,
                },
            );
            for (addr, code) in [
                (FAILING_WRAPPER, wrapper_code),
                (NORMAL_WRAPPER, normal_wrapper_code),
                (ECHO_CONTRACT, echo_code),
            ] {
                db.insert_account_info(
                    addr,
                    AccountInfo {
                        balance: U256::ZERO,
                        nonce: 1,
                        code_hash: keccak256(&code),
                        code: Some(Bytecode::new_raw(code)),
                        account_id: None,
                    },
                );
            }

            let spec =
                reth_ethereum::evm::revm_spec_by_timestamp_and_block_number(&chain_spec, 0, 0);
            let hardfork_flags = chain_spec.get_hardfork_flags(0);
            let mut cfg_env = revm::context::CfgEnv::new()
                .with_chain_id(chain_spec.chain_id())
                .with_spec_and_mainnet_gas_params(spec);
            // Disable EIP-7825 tx gas limit cap so tests can use arbitrary gas limits.
            cfg_env.tx_gas_limit_cap = Some(u64::MAX);
            let ctx = EthEvmContext::new(db, spec)
                .with_cfg(cfg_env)
                .with_block(BlockEnv::default());
            let precompiles = ArcPrecompileProvider::create_precompiles_map(spec, hardfork_flags);
            let instruction = EthInstructions::default();

            let mut registry = SubcallRegistry::new();
            registry.register(
                FAILING_COMPLETE_SUBCALL_ADDRESS,
                Arc::new(FailingCompleteSubcallPrecompile),
                AllowedCallers::Unrestricted,
            );
            registry.register(
                SUBCALL_TEST_ADDRESS,
                Arc::new(SubcallTestPrecompile),
                AllowedCallers::Unrestricted,
            );

            let mut evm = ArcEvm::new(
                ctx,
                revm::inspector::NoOpInspector {},
                precompiles,
                instruction,
                false,
                hardfork_flags,
                Arc::new(registry),
            );

            // Tx 1: call via FailingCompleteSubcallPrecompile — complete_subcall errors, should consume more gas
            let inner_calldata = U256::from(42).to_be_bytes::<32>().to_vec();
            let input = encode_subcall_test_input(ECHO_CONTRACT, &inner_calldata);
            let tx_failing = TxEnv {
                caller: EOA,
                kind: TxKind::Call(FAILING_WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: input.clone(),
                ..Default::default()
            };

            let result_failing = evm.transact_one(tx_failing).expect("tx should succeed");
            let gas_used_failing = match &result_failing {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success (wrapper catches revert), got {other:?}"),
            };

            // Tx 2: call via normal SubcallTestPrecompile — complete_subcall succeeds, uses less gas
            let tx_normal = TxEnv {
                caller: EOA,
                kind: TxKind::Call(NORMAL_WRAPPER),
                value: U256::ZERO,
                gas_limit: 1_000_000,
                gas_price: 0,
                nonce: 1,
                chain_id: Some(LOCAL_DEV.chain_id()),
                data: input,
                ..Default::default()
            };

            let result_normal = evm.transact_one(tx_normal).expect("tx should succeed");
            let gas_used_normal = match &result_normal {
                ExecutionResult::Success { gas_used, .. } => *gas_used,
                other => panic!("expected Success, got {other:?}"),
            };

            // complete_subcall error consumes ALL gas allocated to the subcall, while a successful
            // complete_subcall returns unused gas. So the failing path should use more gas.
            assert!(
                gas_used_failing > gas_used_normal,
                "complete_subcall error ({gas_used_failing}) should consume more gas than \
                 normal path ({gas_used_normal}) because all subcall gas is consumed"
            );
        }
    }
    fn create_test_evm_with_spec(
        db: InMemoryDB,
        hardfork_flags: ArcHardforkFlags,
        spec: SpecId,
    ) -> ArcEvm<
        EthEvmContext<InMemoryDB>,
        NoOpInspector,
        EthInstructions<EthInterpreter, EthEvmContext<InMemoryDB>>,
        PrecompilesMap,
    > {
        let ctx = Context::new(db, spec);
        let instruction = EthInstructions::default();
        let precompiles = ArcPrecompileProvider::create_precompiles_map(spec, hardfork_flags);
        ArcEvm::new(
            ctx,
            NoOpInspector {},
            precompiles,
            instruction,
            false,
            hardfork_flags,
            Arc::new(SubcallRegistry::default()),
        )
    }

    #[test]
    fn test_zero5_emits_eip7708_transfer_log() {
        use revm::handler::SYSTEM_ADDRESS;

        let db = CacheDB::new(EmptyDB::default());
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5]);
        let mut evm = create_test_evm(db, flags);

        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                ADDRESS_A,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        match result {
            BeforeFrameInitResult::Log(log, gas) => {
                assert!(gas > 0, "Should have SLOAD gas cost");
                assert_eq!(
                    log.address, SYSTEM_ADDRESS,
                    "Zero5 should emit EIP-7708 Transfer log from system address"
                );
            }
            other => panic!(
                "Expected Log result with EIP-7708 Transfer under Zero5, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_zero5_self_transfer_no_log() {
        let db = CacheDB::new(EmptyDB::default());
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5]);
        let mut evm = create_test_evm(db, flags);

        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        // Self-transfer: from == to
        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                ADDRESS_A,
                ADDRESS_A,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        match result {
            BeforeFrameInitResult::Checked(gas) => {
                assert!(gas > 0, "Should have SLOAD gas cost");
            }
            other => panic!(
                "Expected Checked result for self-transfer under Zero5, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_pre_zero5_still_emits_custom_log() {
        let db = CacheDB::new(EmptyDB::default());
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero4]);
        let mut evm = create_test_evm(db, flags);

        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                ADDRESS_A,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        match result {
            BeforeFrameInitResult::Log(log, _gas) => {
                assert_eq!(log.address, NATIVE_COIN_AUTHORITY_ADDRESS);
            }
            other => panic!("Expected Log result pre-Zero5, got {:?}", other),
        }
    }

    /// Verifies that AMSTERDAM SpecId enables EIP-7708 (is_enabled_in returns true).
    /// Once REVM is upgraded to a version with EIP-7708 journal support, the journal's
    /// `transfer` method will emit Transfer logs when SpecId >= AMSTERDAM.
    #[test]
    fn test_amsterdam_spec_enables_eip7708() {
        // AMSTERDAM is after PRAGUE in the SpecId ordering
        assert!(
            SpecId::AMSTERDAM.is_enabled_in(SpecId::AMSTERDAM),
            "AMSTERDAM should be enabled in AMSTERDAM"
        );
        assert!(
            !SpecId::PRAGUE.is_enabled_in(SpecId::AMSTERDAM),
            "PRAGUE should NOT be enabled in AMSTERDAM (AMSTERDAM comes after PRAGUE)"
        );

        // Verify that an EVM can be created with AMSTERDAM spec (for future use)
        let db = create_db(&[(ADDRESS_A, 1000)]);
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5]);
        let evm = create_test_evm_with_spec(db, flags, SpecId::AMSTERDAM);
        assert_eq!(
            evm.inner.ctx.cfg.spec,
            SpecId::AMSTERDAM,
            "EVM should be configured with AMSTERDAM spec"
        );
    }

    /// Verifies that Zero5 emits EIP-7708 Transfer logs regardless of SpecId.
    /// Arc self-implements EIP-7708 log emission, so PRAGUE vs AMSTERDAM doesn't matter.
    #[test]
    fn test_zero5_emits_eip7708_regardless_of_spec() {
        use revm::handler::SYSTEM_ADDRESS;

        // Zero5 + PRAGUE: Arc emits EIP-7708 Transfer logs itself
        let db = CacheDB::new(EmptyDB::default());
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5]);
        let mut evm = create_test_evm_with_spec(db, flags, SpecId::PRAGUE);

        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                ADDRESS_A,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        match result {
            BeforeFrameInitResult::Log(log, _gas) => {
                assert_eq!(
                    log.address, SYSTEM_ADDRESS,
                    "Zero5 + PRAGUE: should emit EIP-7708 Transfer log"
                );
            }
            other => panic!(
                "Expected Log result with EIP-7708 Transfer, got {:?}",
                other
            ),
        }
    }

    /// Zero5: CALL with value to Address::ZERO should revert.
    #[test]
    fn test_zero5_call_to_zero_address_reverts() {
        let db = CacheDB::new(EmptyDB::default());
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5]);
        let mut evm = create_test_evm(db, flags);

        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                ADDRESS_A,
                Address::ZERO,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        assert!(
            matches!(result, BeforeFrameInitResult::Reverted(_)),
            "Zero5 should revert CALL with value to zero address, got {:?}",
            result,
        );
    }

    /// Zero5: CALL from Address::ZERO with value should revert.
    #[test]
    fn test_zero5_call_from_zero_address_reverts() {
        let db = CacheDB::new(EmptyDB::default());
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5]);
        let mut evm = create_test_evm(db, flags);

        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                Address::ZERO,
                ADDRESS_B,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        assert!(
            matches!(result, BeforeFrameInitResult::Reverted(_)),
            "Zero5 should revert CALL with value from zero address, got {:?}",
            result,
        );
    }

    /// Pre-Zero5: CALL to Address::ZERO is NOT blocked (backwards compatible).
    #[test]
    fn test_pre_zero5_call_to_zero_address_allowed() {
        let db = CacheDB::new(EmptyDB::default());
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero4]);
        let mut evm = create_test_evm(db, flags);

        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();

        let frame = FrameInit {
            frame_input: FrameInput::Call(call_input(
                CallScheme::Call,
                U256::from(100),
                ADDRESS_A,
                Address::ZERO,
            )),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.before_frame_init(&frame).unwrap();
        assert!(
            matches!(result, BeforeFrameInitResult::Log(_, _)),
            "Pre-Zero5 should allow CALL to zero address, got {:?}",
            result,
        );
    }

    /// Regression test for phantom EIP-7708 logs:
    /// an inner value-transferring CALL that reverts must not leave a Transfer log behind.
    #[test]
    fn test_zero5_reverted_call_with_value_emits_no_eip7708_log() {
        use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
        use revm_primitives::TxKind;

        let chain_spec = localdev_with_hardforks(&[
            (ArcHardfork::Zero3, 0),
            (ArcHardfork::Zero4, 0),
            (ArcHardfork::Zero5, 0),
        ]);
        let sender = Address::repeat_byte(0x11);
        let caller_contract = Address::repeat_byte(0x22);
        let reverting_contract = Address::repeat_byte(0x33);
        let amount = U256::from(100);

        // Runtime bytecode: PUSH1 0x00 PUSH1 0x00 REVERT
        let revert_runtime: Bytes = vec![0x60, 0x00, 0x60, 0x00, 0xfd].into();
        let caller_runtime = call_with_value_bytecode(reverting_contract, amount);

        let mut db = create_db(&[(sender, 1000)]);
        db.insert_account_info(
            caller_contract,
            revm::state::AccountInfo {
                balance: U256::from(1000),
                nonce: 1,
                code_hash: keccak256(caller_runtime.bytecode()),
                code: Some(caller_runtime),
                account_id: None,
            },
        );
        db.insert_account_info(
            reverting_contract,
            revm::state::AccountInfo {
                balance: U256::ZERO,
                nonce: 1,
                code_hash: keccak256(&revert_runtime),
                code: Some(Bytecode::new_raw(revert_runtime)),
                account_id: None,
            },
        );

        let mut evm = create_arc_evm(chain_spec.clone(), db);
        let tx = TxEnv {
            caller: sender,
            kind: TxKind::Call(caller_contract),
            value: U256::ZERO,
            gas_limit: 100_000,
            gas_price: 0,
            chain_id: Some(chain_spec.chain_id()),
            ..Default::default()
        };

        let result = evm.transact_one(tx).expect("nested CALL should execute");
        assert!(
            result.is_success(),
            "Outer transaction should succeed; only the inner CALL should revert, got {:?}",
            result
        );
        assert_eq!(
            result.logs().len(),
            0,
            "Reverted inner CALL with value must not leave an EIP-7708 Transfer log behind"
        );
    }

    /// Regression test for phantom EIP-7708 logs:
    /// an inner CREATE with endowment whose initcode reverts must not leave a Transfer log behind.
    #[test]
    fn test_zero5_reverted_create_with_value_emits_no_eip7708_log() {
        use arc_execution_config::{chainspec::localdev_with_hardforks, hardforks::ArcHardfork};
        use revm_primitives::TxKind;

        let chain_spec = localdev_with_hardforks(&[
            (ArcHardfork::Zero3, 0),
            (ArcHardfork::Zero4, 0),
            (ArcHardfork::Zero5, 0),
        ]);
        let sender = Address::repeat_byte(0x33);
        let factory_contract = Address::repeat_byte(0x44);
        let amount = U256::from(100);

        // Initcode: PUSH1 0x00 PUSH1 0x00 REVERT
        let revert_initcode = vec![0x60, 0x00, 0x60, 0x00, 0xfd];
        let factory_runtime = create_with_value_bytecode(&revert_initcode, amount);

        let mut db = create_db(&[(sender, 1000)]);
        db.insert_account_info(
            factory_contract,
            revm::state::AccountInfo {
                balance: U256::from(1000),
                nonce: 1,
                code_hash: keccak256(factory_runtime.bytecode()),
                code: Some(factory_runtime),
                account_id: None,
            },
        );
        let mut evm = create_arc_evm(chain_spec.clone(), db);
        let tx = TxEnv {
            caller: sender,
            kind: TxKind::Call(factory_contract),
            value: U256::ZERO,
            gas_limit: 120_000,
            gas_price: 0,
            chain_id: Some(chain_spec.chain_id()),
            ..Default::default()
        };

        let result = evm.transact_one(tx).expect("nested CREATE should execute");
        assert!(
            result.is_success(),
            "Outer transaction should succeed; only the inner CREATE should revert, got {:?}",
            result
        );
        assert_eq!(
            result.logs().len(),
            0,
            "Reverted inner CREATE with value must not leave an EIP-7708 Transfer log behind"
        );
    }

    /// Zero5: EIP-7708 Transfer log must precede precompile logs in the journal.
    ///
    /// When a CALL with value targets a precompile that emits its own logs, the
    /// EIP-7708 Transfer log from the value transfer must appear before any logs
    /// produced by the precompile execution.
    #[test]
    fn test_zero5_transfer_log_precedes_precompile_log() {
        use alloy_primitives::{Log as PrimLog, LogData};
        use reth_evm::precompiles::DynPrecompile;
        use revm::handler::SYSTEM_ADDRESS;
        use revm::precompile::{PrecompileId, PrecompileOutput};

        // A distinctive address for the mock precompile, not overlapping with real ones.
        const MOCK_PRECOMPILE: Address = address!("ff00000000000000000000000000000000000099");

        // A distinctive log address so we can tell precompile logs from transfer logs.
        const MOCK_LOG_ADDRESS: Address = address!("aa00000000000000000000000000000000000001");

        // Build a PrecompilesMap that includes the mock precompile.
        let spec = SpecId::PRAGUE;
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5]);
        let mut precompile_map = ArcPrecompileProvider::create_precompiles_map(spec, flags);
        precompile_map.set_precompile_lookup(move |address: &Address| {
            if *address == MOCK_PRECOMPILE {
                Some(DynPrecompile::new_stateful(
                    PrecompileId::Custom("MOCK_LOG_EMITTER".into()),
                    move |mut input| {
                        // Emit a log via the journal so it appears in the journal log list.
                        input.internals.log(PrimLog {
                            address: MOCK_LOG_ADDRESS,
                            data: LogData::new_unchecked(vec![], Bytes::new()),
                        });
                        Ok(PrecompileOutput::new(0, Bytes::new()))
                    },
                ))
            } else {
                None
            }
        });

        // Build the ArcEvm with our custom precompile map.
        let db = create_db(&[(ADDRESS_A, 10_000)]);
        let mut evm = create_test_evm_with_precompiles(db, flags, precompile_map);

        // Warm-load accounts so journal state is populated for transfer_loaded.
        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();
        evm.ctx_mut().journal_mut().load_account(ADDRESS_A).unwrap();
        evm.ctx_mut()
            .journal_mut()
            .load_account(MOCK_PRECOMPILE)
            .unwrap();

        // CALL with value from ADDRESS_A to MOCK_PRECOMPILE.
        // bytecode_address must equal the precompile address so PrecompilesMap::run finds it.
        let frame = FrameInit {
            frame_input: FrameInput::Call(Box::new(CallInputs {
                scheme: CallScheme::Call,
                target_address: MOCK_PRECOMPILE,
                bytecode_address: MOCK_PRECOMPILE,
                known_bytecode: None,
                value: CallValue::Transfer(U256::from(100)),
                input: CallInput::Bytes(Bytes::new()),
                gas_limit: 500_000,
                is_static: false,
                caller: ADDRESS_A,
                return_memory_offset: 0..0,
            })),
            memory: SharedMemory::default(),
            depth: 1,
        };

        // Record the log count before frame_init.
        let logs_before = evm.ctx().journal().logs().len();

        let result = evm.frame_init(frame);
        assert!(result.is_ok(), "frame_init should succeed");

        let ctx = evm.ctx();
        let logs = ctx.journal().logs();
        let new_logs = &logs[logs_before..];

        assert!(
            new_logs.len() >= 2,
            "Expected at least 2 logs (transfer + precompile), got {}",
            new_logs.len()
        );

        // First log must be the EIP-7708 Transfer log from the value transfer.
        assert_eq!(
            new_logs[0].address, SYSTEM_ADDRESS,
            "First log should be the EIP-7708 Transfer log from system address, got {:?}",
            new_logs[0].address
        );

        // Second log must be the mock precompile's log.
        assert_eq!(
            new_logs[1].address, MOCK_LOG_ADDRESS,
            "Second log should be the mock precompile log, got {:?}",
            new_logs[1].address
        );
    }

    /// Regression test: a CALL with value to a precompile that reverts must roll back
    /// the EIP-7708 Transfer log via Arc's checkpoint_revert (the precompile path at
    /// line 507, distinct from the non-precompile path tested by
    /// `test_zero5_reverted_call_with_value_emits_no_eip7708_log`).
    #[test]
    fn test_zero5_reverted_precompile_call_with_value_emits_no_eip7708_log() {
        use reth_evm::precompiles::DynPrecompile;
        use revm::precompile::{PrecompileError, PrecompileId};

        const MOCK_PRECOMPILE: Address = address!("ff00000000000000000000000000000000000099");

        let spec = SpecId::PRAGUE;
        let flags = ArcHardforkFlags::with(&[ArcHardfork::Zero5]);
        let mut precompile_map = ArcPrecompileProvider::create_precompiles_map(spec, flags);
        precompile_map.set_precompile_lookup(move |address: &Address| {
            if *address == MOCK_PRECOMPILE {
                Some(DynPrecompile::new_stateful(
                    PrecompileId::Custom("MOCK_REVERTER".into()),
                    move |_input| Err(PrecompileError::other("authorization failed")),
                ))
            } else {
                None
            }
        });

        let db = create_db(&[(ADDRESS_A, 10_000)]);
        let mut evm = create_test_evm_with_precompiles(db, flags, precompile_map);

        evm.ctx_mut()
            .journal_mut()
            .load_account(NATIVE_COIN_CONTROL_ADDRESS)
            .unwrap();
        evm.ctx_mut().journal_mut().load_account(ADDRESS_A).unwrap();
        evm.ctx_mut()
            .journal_mut()
            .load_account(MOCK_PRECOMPILE)
            .unwrap();

        let logs_before = evm.ctx().journal().logs().len();

        let frame = FrameInit {
            frame_input: FrameInput::Call(Box::new(CallInputs {
                scheme: CallScheme::Call,
                target_address: MOCK_PRECOMPILE,
                bytecode_address: MOCK_PRECOMPILE,
                known_bytecode: None,
                value: CallValue::Transfer(U256::from(100)),
                input: CallInput::Bytes(Bytes::new()),
                gas_limit: 500_000,
                is_static: false,
                caller: ADDRESS_A,
                return_memory_offset: 0..0,
            })),
            memory: SharedMemory::default(),
            depth: 1,
        };

        let result = evm.frame_init(frame);
        assert!(
            result.is_ok(),
            "frame_init should succeed (precompile failure is a Result, not an Err)"
        );

        let ctx = evm.ctx();
        let new_logs = &ctx.journal().logs()[logs_before..];
        assert_eq!(
            new_logs.len(),
            0,
            "Reverting precompile CALL with value must not leave an EIP-7708 Transfer log behind, got {} logs",
            new_logs.len()
        );
    }

    // ----- Revm upgrade checklist -----

    /// Guard against silent drift when upgrading revm. If this test fails, the revm
    /// dependency version has changed and the forked/mirrored functions below must be
    /// reviewed for upstream behavioral changes:
    ///
    /// 1. [`ArcEvm::inspect_frame_init_impl`] + [`ArcEvm::frame_start_with_trace`] in
    ///    `crates/evm/src/evm.rs`
    ///    — mirrors `InspectorEvmTr::inspect_frame_init` from `revm-inspector`
    ///    — <https://github.com/bluealloy/revm/blob/v103/crates/inspector/src/traits.rs#L98-L137>
    ///
    /// 2. [`init_frame`] in `crates/evm/src/evm.rs`
    ///    — mirrors `Evm::frame_init` from `revm-handler` (borrow-split variant)
    ///    — see [`revm::handler::EvmTr::frame_init`]
    ///
    /// 3. `arc_network_selfdestruct_impl` in `crates/evm/src/opcode.rs`
    ///    — forked from `revm/crates/interpreter/src/instructions/host.rs` (SELFDESTRUCT)
    ///    — <https://github.com/bluealloy/revm/blob/v97/crates/interpreter/src/instructions/host.rs#L387>
    ///
    /// 4. `istanbul_sstore_cost` logic in `crates/precompiles/src/helpers.rs`
    ///    — mirrors revm's `istanbul_sstore_cost` gas calculation
    #[test]
    fn revm_version_check() {
        const EXPECTED_REVM_VERSION: &str = "34.0.0";
        let workspace_toml = include_str!("../../../Cargo.toml");
        let expected = format!("revm = {{ version = \"{EXPECTED_REVM_VERSION}\"");
        assert!(
            workspace_toml.contains(&expected),
            "revm version has changed from {EXPECTED_REVM_VERSION}. \
             Review all forked/mirrored revm functions listed in this test's doc comment."
        );
    }
}
