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

//! Custom precompiles for Arc Chain
//!
//! This module provides a framework for implementing custom precompiles in Arc Chain.
//! Precompiles are special contracts deployed at fixed addresses that provide optimized
//! implementations of commonly used functionality.
//!
//! ## Types of Precompiles
//!
//! ### Stateful Precompiles
//! These precompiles can read from and write to storage, making them suitable for:
//! - Managing on-chain state
//! - Implementing complex protocols
//! - Building upgradeable logic
//!
//! Example from our implementation:
//! ```rust,ignore
//! // Define storage keys
//! const COUNTER_STORAGE_KEY: StorageKey = StorageKey::ZERO;
//!
//! // Define the interface
//! sol! {
//!     interface IStatefulPrecompile {
//!         function increment() external returns (uint256 newValue);
//!         function getCounter() external view returns (uint256 value);
//!     }
//! }
//!
//! // Implement using the precompile! macro. Each arm receives the calldata bytes
//! // following the 4-byte selector (`input` below) and must evaluate to
//! // `Result<PrecompileOutput, PrecompileErrorOrRevert>`.
//! precompile!(run_stateful_precompile, precompile_input, hardfork_flags; {
//!     IStatefulPrecompile::incrementCall => |_input| {
//!         (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
//!             let mut gas_counter = Gas::new(precompile_input.gas);
//!             let mut precompile_input = precompile_input;
//!
//!             let output = read(
//!                 &mut precompile_input.internals,
//!                 ADDRESS,
//!                 COUNTER_STORAGE_KEY,
//!                 &mut gas_counter,
//!                 hardfork_flags,
//!             )?;
//!             let new_value = U256::from_be_slice(&output) + U256::from(1);
//!
//!             write(
//!                 &mut precompile_input.internals,
//!                 ADDRESS,
//!                 COUNTER_STORAGE_KEY,
//!                 &new_value.to_be_bytes_vec(),
//!                 &mut gas_counter,
//!                 hardfork_flags,
//!             )?;
//!
//!             Ok(PrecompileOutput::new(gas_counter.used(), new_value.abi_encode().into()))
//!         })()
//!     },
//! });
//! ```
//!
//! ## Creating a New Precompile
//!
//! ### Step 1: Choose an Address
//! Select a unique address for your precompile. Convention is to use low addresses:
//! ```rust,ignore
//! const MY_PRECOMPILE_ADDRESS: Address = address!("0x0000000000000000000000000000000000000044");
//! ```
//!
//! ### Step 2: Define the Interface
//! Use the `sol!` macro to define your precompile's Solidity interface:
//! ```rust,ignore
//! sol! {
//!     interface IMyPrecompile {
//!         function myFunction(uint256 param) external returns (uint256);
//!     }
//! }
//! ```
//!
//! ### Step 3: Implement the Logic
//!
//! ```rust,ignore
//! precompile!(run_my_precompile, precompile_input, hardfork_flags; {
//!     IMyPrecompile::myFunctionCall => |input| {
//!         (|| -> Result<PrecompileOutput, PrecompileErrorOrRevert> {
//!             let mut gas_counter = Gas::new(precompile_input.gas);
//!             let mut precompile_input = precompile_input;
//!
//!             let args = IMyPrecompile::myFunctionCall::abi_decode_raw(input)
//!                 .map_err(|_| PrecompileErrorOrRevert::new_reverted_with_penalty(
//!                     gas_counter,
//!                     PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY,
//!                     ERR_EXECUTION_REVERTED,
//!                 ))?;
//!
//!             let output = read(
//!                 &mut precompile_input.internals,
//!                 MY_PRECOMPILE_ADDRESS,
//!                 StorageKey::from(0),
//!                 &mut gas_counter,
//!                 hardfork_flags,
//!             )?;
//!
//!             Ok(PrecompileOutput::new(gas_counter.used(), output))
//!         })()
//!     },
//! });
//! ```
//!
//! ### Step 4: Register the Precompile
//! Add a match arm to `ArcPrecompileProvider::create_precompiles_map` in
//! `precompile_provider.rs`:
//! ```rust,ignore
//! MY_PRECOMPILE_ADDRESS => Some(DynPrecompile::new_stateful(
//!     PrecompileId::Custom("MY_PRECOMPILE".into()),
//!     move |input| run_my_precompile(input, hardfork_flags),
//! )),
//! ```
//!
//! ## Gas Accounting
//!
//! The `precompile!` macro does not track gas on its own — each arm constructs a `Gas`
//! counter from `precompile_input.gas` and threads `&mut gas_counter` through the helpers
//! (`read`, `write`, `emit_event`, `balance_incr`, …). Helpers mutate the counter in place
//! and return `PrecompileErrorOrRevert::Error(OutOfGas)` when the remaining gas is
//! insufficient. The macro adds `PRECOMPILE_ABI_DECODE_REVERT_GAS_PENALTY` when the
//! selector is unknown or the input is shorter than 4 bytes; arms should use the same
//! penalty when ABI decoding fails.
//!
//! ## Storage Operations
//!
//! `read` and `write` take a mutable borrow of `precompile_input.internals`, the gas
//! counter, and the active hardfork flags. `read` returns the stored value as
//! big-endian `Bytes`; `write` returns `()`:
//!
//! ```rust,ignore
//! let output = read(
//!     &mut precompile_input.internals,
//!     address,
//!     key,
//!     &mut gas_counter,
//!     hardfork_flags,
//! )?;
//! let current = U256::from_be_slice(&output);
//!
//! write(
//!     &mut precompile_input.internals,
//!     address,
//!     key,
//!     &new_value.to_be_bytes_vec(),
//!     &mut gas_counter,
//!     hardfork_flags,
//! )?;
//! ```
//!
//! Gas costs:
//! - `read`: 2,100 gas pre-Zero5; EIP-2929 warm/cold pricing from Zero5+
//!   (`WARM_STORAGE_READ_COST` / `COLD_SLOAD_COST`).
//! - `write`: 2,900 gas pre-Zero5; EIP-2929 / EIP-2200 pricing from Zero5+.

pub mod helpers;
mod macros;
mod native_coin_authority;
pub mod native_coin_control;
pub mod pq;
pub mod precompile_provider;
pub mod system_accounting;
pub use native_coin_authority::INativeCoinAuthority;
pub use native_coin_authority::NATIVE_COIN_AUTHORITY_ADDRESS;
pub mod subcall;
pub use native_coin_control::INativeCoinControl;
pub use native_coin_control::NATIVE_COIN_CONTROL_ADDRESS;

pub mod call_from;

#[cfg(any(test, feature = "test-utils"))]
pub mod pq_test_vectors;
