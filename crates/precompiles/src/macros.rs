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

/// Macro for creating stateful precompiles with function-selector dispatch and revert
/// conversion. Gas accounting and ABI decoding are the arm body's responsibility.
///
/// # Syntax
/// ```rust,ignore
/// precompile!(fn_name, precompile_input, hardfork_flags; {
///     Interface::functionCall => |calldata_bytes| {
///         // Must evaluate to Result<PrecompileOutput, PrecompileErrorOrRevert>.
///         // `calldata_bytes` is the input after the 4-byte selector.
///     },
///     // Additional functions...
/// });
/// ```
///
/// The generated function takes `(PrecompileInput, ArcHardforkFlags)` and returns
/// `Result<PrecompileOutput, PrecompileError>`. Arms that return
/// `PrecompileErrorOrRevert::Revert(...)` are converted into an `Ok(PrecompileOutput)`
/// carrying the revert payload; `PrecompileErrorOrRevert::Error(...)` becomes `Err`.
/// If the calldata is shorter than 4 bytes or the selector is unknown, the macro
/// charges `PRECOMPILE_EARLY_REVERT_GAS_PENALTY` and returns a revert.
///
/// # Example
/// ```rust,ignore
/// precompile!(run_counter_precompile, precompile_input, hardfork_flags; {
///     ICounter::incrementCall => |_input| {
///         (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
///             let mut gas_counter = Gas::new(precompile_input.gas);
///             let mut precompile_input = precompile_input;
///
///             let output = read(
///                 &mut precompile_input.internals,
///                 ADDRESS,
///                 KEY,
///                 &mut gas_counter,
///             )?;
///             let new_value = U256::from_be_slice(&output) + U256::from(1);
///
///             write(
///                 &mut precompile_input.internals,
///                 ADDRESS,
///                 KEY,
///                 &new_value.to_be_bytes_vec(),
///                 &mut gas_counter,
///             )?;
///
///             Ok(PrecompileOutput::new(gas_counter.used(), new_value.abi_encode().into()))
///         })()
///     },
///     ICounter::getCountCall => |_input| {
///         (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
///             let mut gas_counter = Gas::new(precompile_input.gas);
///             let mut precompile_input = precompile_input;
///
///             let output = read(
///                 &mut precompile_input.internals,
///                 ADDRESS,
///                 KEY,
///                 &mut gas_counter,
///             )?;
///
///             Ok(PrecompileOutput::new(gas_counter.used(), output))
///         })()
///     },
/// });
/// ```
///
/// The macro handles:
/// - Function selector matching
/// - Fallback revert (with gas penalty) for unknown selectors or truncated input
/// - Conversion of `PrecompileErrorOrRevert` into the final `Result`
///
/// ABI decoding, gas accounting, and output encoding remain the arm body's job; use
/// `helpers::abi_decode_raw_validated` on the supplied calldata bytes when
/// you need the decoded arguments.
#[macro_export]
macro_rules! precompile {
    ($fn_name:ident, $precompile_input:ident, $hardfork_flags:ident; {
        $(
            $fn_call:path => |$arg:ident| $body:expr
        ),* $(,)?
    }) => {
        pub(crate) fn $fn_name(
            $precompile_input: reth_evm::precompiles::PrecompileInput,
            $hardfork_flags: arc_execution_config::hardforks::ArcHardforkFlags,
        ) -> Result<reth_ethereum::evm::revm::precompile::PrecompileOutput, reth_ethereum::evm::revm::precompile::PrecompileError> {
            let input_bytes = $precompile_input.data;
            let reservoir = $precompile_input.reservoir;
            let gas_counter = revm_interpreter::Gas::new($precompile_input.gas);

            if input_bytes.len() < 4 {
                return $crate::helpers::PrecompileErrorOrRevert::new_reverted_with_penalty(
                    gas_counter, reservoir, PRECOMPILE_EARLY_REVERT_GAS_PENALTY, "Input too short").into();
            }

            let selector: [u8; 4] = input_bytes[0..4].try_into().unwrap();

            let result: Result<reth_ethereum::evm::revm::precompile::PrecompileOutput, $crate::helpers::PrecompileErrorOrRevert> = match selector {
                $(
                    sel if sel == <$fn_call>::SELECTOR => {
                        let $arg = input_bytes.get(4..).unwrap_or_default();
                        $body
                    }
                ),*
                _ => {
                    return $crate::helpers::PrecompileErrorOrRevert::new_reverted_with_penalty(
                        gas_counter, reservoir, PRECOMPILE_EARLY_REVERT_GAS_PENALTY, "Invalid selector").into();
                },
            };

            match result {
                Ok(output) => Ok(output),
                Err(err_or_revert) => err_or_revert.into(),
            }
        }
    };
}
