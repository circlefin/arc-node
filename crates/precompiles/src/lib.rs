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
//! ### Stateless Precompiles
//! These are simple precompiles that perform computations without modifying state.
//! They are ideal for:
//! - Cryptographic operations
//! - Mathematical/deterministic computations
//! - Data transformations
//!
//! Example:
//! ```rust,ignore
//! // Define the interface using Solidity ABI
//! sol! {
//!     interface IStatelessPrecompile {
//!         function doSomething(uint256 first, uint8 second, bool third, string memory message)
//!             external returns (uint256 result);
//!     }
//! }
//!
//! // Implement using the stateless! macro
//! stateless!(stateless_precompile_fn, input, gas_limit; {
//!     IStatelessPrecompile::doSomethingCall => |call| {
//!         // Access decoded parameters directly
//!         call.first + U256::from(call.second) + U256::from(call.third) + U256::from(call.message.len())
//!     },
//! });
//! ```
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
//!         function setCounter(uint256 newValue) external returns (uint256 previousValue);
//!     }
//! }
//!
//! // Implement using the stateful! macro
//! stateful!(run_stateful_precompile, context, inputs, gas_limit; {
//!     IStatefulPrecompile::incrementCall => |_call| {
//!         // Read current value
//!         let (output, gas_counter) = read(context, ADDRESS, COUNTER_STORAGE_KEY, gas_limit)?;
//!         let current = U256::from_be_slice(&output.bytes);
//!
//!         // Write new value
//!         let new_value = current + U256::from(1);
//!         write(context, ADDRESS, COUNTER_STORAGE_KEY, &new_value.to_be_bytes_vec(), gas_counter)
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
//! For stateless precompiles:
//! ```rust,ignore
//! stateless!(my_precompile_fn, input, gas_limit; {
//!     IMyPrecompile::myFunctionCall => |call| {
//!         // Your logic here
//!         call.param * U256::from(2)
//!     },
//! });
//! ```
//!
//! For stateful precompiles:
//! ```rust,ignore
//! stateful!(run_my_precompile, context, inputs, gas_limit; {
//!     IMyPrecompile::myFunctionCall => |call| {
//!         // Read/write storage as needed
//!         read(context, MY_PRECOMPILE_ADDRESS, StorageKey::from(0), gas_limit)
//!     },
//! });
//! ```
//!
//! ### Step 4: Register the Precompile
//! For stateless precompiles, add to `custom_stateless_precompiles()`:
//! ```rust,ignore
//! precompiles.extend([PrecompileWithAddress::from((
//!     MY_PRECOMPILE_ADDRESS,
//!     PrecompileFn::from(my_precompile_fn as fn(&[u8], u64) -> PrecompileResult),
//! ))]);
//! ```
//!
//! For stateful precompiles, add to the `stateful_precompiles` HashMap in `Default::default()`:
//! ```rust,ignore
//! stateful_precompiles.insert(
//!     MY_PRECOMPILE_ADDRESS,
//!     run_my_precompile as StatefulPrecompileFn,
//! );
//! ```
//!
//! ## Gas Accounting
//!
//! Both macros handle gas accounting automatically:
//! - Stateless precompiles consume all provided gas
//! - Stateful precompiles track gas usage through storage operations
//! - Out-of-gas errors are handled gracefully
//!
//! ## Storage Operations
//!
//! The `read` and `write` helper functions provide storage access:
//! - `read`: Costs 2,100 gas, returns stored value
//! - `write`: Costs 41,000 gas (21,000 base + 20,000 SSTORE)
//!
//! When chaining operations, pass the gas counter between calls:
//! ```rust,ignore
//! let (output, gas_counter) = read(context, address, key, gas_limit)?;
//! write(context, address, key, &new_value, gas_counter)
//! ```

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
