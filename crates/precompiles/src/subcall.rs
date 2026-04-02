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

//! Subcall precompile trait and types.
//!
//! Defines the two-phase execution interface for precompiles that need to spawn
//! child EVM call frames. Implementations live alongside this module (e.g.,
//! [`crate::call_from::CallFromPrecompile`]). The registry and continuation storage
//! that drives the execution loop live in `arc-evm`.

use alloy_primitives::Bytes;
use revm::handler::FrameResult;
use revm::interpreter::interpreter_action::CallInputs;
use std::any::Any;

/// Trait for precompiles that spawn child EVM call frames.
///
/// Implementors define a two-phase execution model:
/// - [`init_subcall`](SubcallPrecompile::init_subcall): Decodes the precompile input and returns a
///   child call request plus any continuation state needed for completion.
/// - [`complete_subcall`](SubcallPrecompile::complete_subcall): Receives the child call result and
///   continuation state, producing the final precompile output.
///
/// # Constraint: child target must not be a subcall precompile
///
/// The child frame returned by `init_subcall` is dispatched via revm's standard
/// `init_with_context`, which does **not** consult the subcall registry. If
/// `child_inputs.target_address` is another subcall precompile address, the call will hit the
/// stub bytecode at that genesis address (typically `0x01` / `ADD`) and silently revert, rather
/// than triggering the target's two-phase execution.
///
/// Indirect calls work fine: if the child targets a regular contract that then CALLs a subcall
/// precompile, the CALL opcode triggers a new `frame_init` cycle through `ArcEvm`, which
/// consults the subcall registry as expected.
///
/// # No separate checkpoint around child execution
///
/// The subcall framework does **not** take a separate journal checkpoint around the child
/// execution. The child frame's own checkpoint (managed by revm's `make_call_frame` /
/// `process_next_action`) handles commit/revert based on the child's success or failure.
///
/// If `complete_subcall` returns `success: false` or `Err` when the child succeeded, the
/// child's state changes will **not** be reverted — they are already committed. Implementors
/// should be aware of this when designing their completion logic.
///
/// Returning `success: true` when the child failed is fine (e.g., `CallFromPrecompile`
/// always succeeds and encodes the child's outcome in its output bytes). The child's
/// checkpoint was already reverted, so no state leaks.
pub trait SubcallPrecompile: Send + Sync {
    /// Decode precompile input and produce a child call request.
    ///
    /// Called during `frame_init` when the EVM encounters a call to this precompile's address.
    /// Returns either a subcall request (with continuation data for `complete_subcall`) or an
    /// error.
    fn init_subcall(&self, inputs: &CallInputs) -> Result<SubcallInitResult, SubcallError>;

    /// Finalize the precompile result after the child call completes.
    ///
    /// Called during `frame_return_result` when the child frame finishes execution.
    /// Receives the continuation data from `init_subcall` and the child's frame result.
    ///
    /// # Note on checkpoint semantics
    ///
    /// Returning `success: false` or `Err` when the child succeeded will **not** revert
    /// the child's state changes. See the trait-level docs for details.
    fn complete_subcall(
        &self,
        continuation_data: SubcallContinuationData,
        child_result: &FrameResult,
    ) -> Result<SubcallCompletionResult, SubcallError>;

    /// Construct the child [`CallInputs`] for tracing purposes.
    ///
    /// Called by `ArcEvm::inspect_frame_init` *before* `init_subcall` to obtain the
    /// child call's identity (caller, target, gas limit, calldata). The returned
    /// inputs are passed to the inspector's `frame_start`, making the precompile
    /// transparent in `debug_traceTransaction` output — the trace shows the logical
    /// child call instead of the precompile address.
    ///
    /// The returned `CallInputs` should match what `init_subcall` would produce
    /// (same caller, target, gas limit, calldata). Implementations should share
    /// the decoding logic with `init_subcall` to keep them in sync.
    ///
    /// Returns `None` if the input cannot be decoded (e.g., malformed calldata).
    /// In that case the trace falls back to the precompile's own address, so the
    /// debug trace for error cases will differ from the successful case (showing
    /// the precompile call instead of the logical child call).
    fn trace_child_call(&self, _inputs: &CallInputs) -> Option<CallInputs> {
        None
    }
}

/// Result of a successful [`SubcallPrecompile::init_subcall`] execution.
pub struct SubcallInitResult {
    /// The child call inputs to execute.
    pub child_inputs: Box<CallInputs>,
    /// Opaque state carried from `init_subcall` to `complete_subcall`.
    pub continuation_data: SubcallContinuationData,
    /// Gas consumed by the precompile itself (ABI decoding, validation).
    /// Deducted from the caller's gas budget before the 63/64ths split.
    pub gas_overhead: u64,
}

/// Opaque state blob carried between `init_subcall` and `complete_subcall`.
pub struct SubcallContinuationData {
    /// Precompile-specific state (e.g., memo bytes, decoded parameters).
    pub state: Box<dyn Any + Send>,
}

/// Result of a successful [`SubcallPrecompile::complete_subcall`] execution.
pub struct SubcallCompletionResult {
    /// The output bytes to return to the caller.
    pub output: Bytes,
    /// Whether the precompile considers the call successful.
    pub success: bool,
}

/// Errors that can occur during subcall precompile execution.
#[derive(Debug, thiserror::Error)]
pub enum SubcallError {
    #[error("ABI decode error: {0}")]
    AbiDecodeError(String),
    #[error("unexpected frame result type (expected Call)")]
    UnexpectedFrameResult,
    #[error("insufficient gas: {0}")]
    InsufficientGas(String),
    #[error("internal error: {0}")]
    InternalError(String),
}
