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

/// Macro for creating stateful precompiles with automatic ABI decoding and gas accounting.
///
/// # Syntax
/// ```rust,ignore
/// stateful!(function_name, context, inputs, gas_limit; {
///     Interface::functionCall => |decoded_args| {
///         // Your implementation here
///         // Must return Result<(PrecompileOutput, u64), PrecompileError>
///     },
///     // Additional functions...
/// });
/// ```
///
/// # Example
/// ```rust,ignore
/// stateful!(run_counter_precompile, context, inputs, gas_limit; {
///     ICounter::incrementCall => |_call| {
///         let (current_output, gas_counter) = read(context, ADDRESS, KEY, gas_limit)?;
///         let current = U256::from_be_slice(&current_output.bytes);
///         let new_value = current + U256::from(1);
///         write(context, ADDRESS, KEY, &new_value.to_be_bytes_vec(), gas_counter)
///     },
///     ICounter::getCountCall => |_call| {
///         read(context, ADDRESS, KEY, gas_limit)
///     },
/// });
/// ```
///
/// The macro handles:
/// - Function selector matching
/// - ABI decoding of inputs
/// - Gas accounting
/// - Error conversion
/// - Output formatting
#[macro_export]
macro_rules! stateful {
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
            let gas_counter = revm_interpreter::Gas::new($precompile_input.gas);

            if input_bytes.len() < 4 {
                return $crate::helpers::PrecompileErrorOrRevert::new_reverted_with_penalty(
                    gas_counter, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, "Input too short").into();
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
                        gas_counter, PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY, "Invalid selector").into();
                },
            };

            match result {
                Ok(output) => Ok(output),
                Err(err_or_revert) => match err_or_revert {
                    $crate::helpers::PrecompileErrorOrRevert::Revert(output) => Ok(output),
                    $crate::helpers::PrecompileErrorOrRevert::Error(error) => Err(error),
                },
            }
        }
    };
}

/// Macro for creating stateless precompiles with automatic ABI encoding/decoding.
///
/// # Syntax
/// ```rust,ignore
/// stateless!(function_name, input, gas_limit; {
///     Interface::functionCall => |decoded_call| {
///         // Your implementation here
///         // Must return a value that implements SolValue
///     },
///     // Additional functions...
/// });
/// ```
///
/// # Example
/// ```rust,ignore
/// stateless!(math_precompile_fn, input, gas_limit; {
///     IMath::addCall => |call| {
///         call.a + call.b
///     },
///     IMath::multiplyCall => |call| {
///         call.a * call.b
///     },
/// });
/// ```
///
/// The macro handles:
/// - Function selector matching
/// - ABI decoding of inputs
/// - ABI encoding of outputs
/// - Error handling
/// - Gas consumption (uses all provided gas)
#[macro_export]
macro_rules! stateless {
    ($fn_name:ident, $input:ident, $gas_limit:ident; {
        $(
            $fn_call:path => |$call:ident| $handler:expr
        ),* $(,)?
    }) => {
        pub(crate) fn $fn_name($input: &[u8], $gas_limit: u64) -> PrecompileResult {
            if $input.len() < 4 {
                return Err(PrecompileError::Other("Input too short".into()));
            }

            let selector = &$input[0..4];

            let output = match selector {
                $(
                    sel if sel == <$fn_call>::SELECTOR => {
                        let $call = <$fn_call>::abi_decode($input).map_err(|e| {
                            PrecompileError::Other(format!("ABI decode error: {e:?}").into())
                        })?;
                        let result = $handler;
                        result.abi_encode()
                    }
                )*
                _ => return Err(PrecompileError::Other("Invalid selector".into())),
            };

            Ok(PrecompileOutput::new($gas_limit, output.into()))
        }
    };
}
