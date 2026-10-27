// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
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

//! Interpreter loop for the inspected path of [`ArcEvm`](crate::evm::ArcEvm).
//!
//! Forked from revm-inspector v19.0.0 (bluealloy/revm tag `v107`), `inspect_instructions`,
//! `inspect_log` and `inspect_selfdestruct` in
//! <https://github.com/bluealloy/revm/blob/v107/crates/inspector/src/handler.rs#L225-L320>,
//! with the SELFDESTRUCT fix backported from <https://github.com/bluealloy/revm/pull/3805>
//! (revm-inspector 42.0.0 and later; reth v2.7.0 uses 43.0.3):
//!  - `inspect_selfdestruct` reads only the journal entries written by the SELFDESTRUCT
//!    itself. v19.0.0 reads the last entry of the transaction, so a SELFDESTRUCT that writes
//!    none (EIP-6780 selfdestruct to self) reported an earlier transfer's sender, recipient
//!    and value to the inspector.
//!
//! The helpers are private upstream, so they are copied too. Remove this module when
//! revm-inspector is upgraded to 42.0.0 or later.

use revm::bytecode::opcode;
use revm::context::{ContextTr, JournalEntry, JournalTr};
use revm::inspector::{Inspector, JournalExt};
use revm::interpreter::{
    instructions::InstructionTable,
    interpreter_types::{Jumps, LoopControl},
    Host, InstructionResult, Interpreter, InterpreterAction, InterpreterTypes,
};

/// Run Interpreter loop with inspection support.
///
/// This function is used to inspect the Interpreter loop.
/// It will call [`Inspector::step`] and [`Inspector::step_end`] after each instruction.
/// And [`Inspector::log`],[`Inspector::selfdestruct`] for each log and selfdestruct instruction.
pub(crate) fn inspect_instructions<CTX, IT>(
    context: &mut CTX,
    interpreter: &mut Interpreter<IT>,
    mut inspector: impl Inspector<CTX, IT>,
    instructions: &InstructionTable<IT, CTX>,
) -> InterpreterAction
where
    CTX: ContextTr<Journal: JournalExt> + Host,
    IT: InterpreterTypes,
{
    // MODIFIED (bluealloy/revm#3805): journal length before the current instruction.
    let mut instruction_journal_i = None;
    loop {
        inspector.step(interpreter, context);
        if interpreter.bytecode.is_end() {
            break;
        }

        let opcode = interpreter.bytecode.opcode();
        // MODIFIED (bluealloy/revm#3805)
        instruction_journal_i = Some(context.journal().journal().len());
        interpreter.step(instructions, context);

        if (opcode::LOG0..=opcode::LOG4).contains(&opcode) {
            inspect_log(interpreter, context, &mut inspector);
        }

        inspector.step_end(interpreter, context);

        if interpreter.bytecode.is_end() {
            break;
        }
    }

    let next_action = interpreter.take_next_action();

    // Handle selfdestruct.
    if let InterpreterAction::Return(result) = &next_action {
        if result.result == InstructionResult::SelfDestruct {
            // MODIFIED (bluealloy/revm#3805)
            if let Some(journal_i) = instruction_journal_i {
                inspect_selfdestruct(context, &mut inspector, journal_i);
            }
        }
    }

    next_action
}

#[inline(never)]
#[cold]
fn inspect_log<CTX, IT>(
    interpreter: &mut Interpreter<IT>,
    context: &mut CTX,
    inspector: &mut impl Inspector<CTX, IT>,
) where
    CTX: ContextTr<Journal: JournalExt> + Host,
    IT: InterpreterTypes,
{
    // `LOG*` instruction reverted.
    if interpreter
        .bytecode
        .action()
        .as_ref()
        .is_some_and(|x| x.is_return())
    {
        return;
    }

    // MODIFIED: `expect` instead of upstream's `unwrap`, which the workspace lints deny.
    let log = context
        .journal_mut()
        .logs()
        .last()
        .expect("a LOG* instruction that did not revert appended a log")
        .clone();
    inspector.log_full(interpreter, context, log);
}

#[inline(never)]
#[cold]
fn inspect_selfdestruct<CTX, IT>(
    context: &mut CTX,
    inspector: &mut impl Inspector<CTX, IT>,
    journal_i: usize,
) where
    CTX: ContextTr<Journal: JournalExt> + Host,
    IT: InterpreterTypes,
{
    // MODIFIED (bluealloy/revm#3805): only entries written by this SELFDESTRUCT. Reads through
    // `journal()`, as `journal_mut()` returns a temporary that would not outlive `entry` here.
    let entry = context
        .journal()
        .journal()
        .get(journal_i..)
        .and_then(|entries| entries.last());

    if let Some(
        JournalEntry::AccountDestroyed {
            address: contract,
            target: to,
            had_balance: balance,
            ..
        }
        | JournalEntry::BalanceTransfer {
            from: contract,
            to,
            balance,
            ..
        },
    ) = entry
    {
        inspector.selfdestruct(*contract, *to, *balance);
    }
}
