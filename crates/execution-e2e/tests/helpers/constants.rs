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

//! Shared constants for EIP-7708 e2e tests.
//!
//! Each test binary only uses a subset of these; unused items are expected.
#![allow(dead_code)]

use alloy_primitives::{address, Address};

// Re-export event signatures from the library to avoid duplication.
// Not all test files use both signatures, but they are shared constants.
#[allow(unused_imports)]
pub use arc_execution_e2e::actions::{NATIVE_COIN_TRANSFERRED_SIGNATURE, TRANSFER_EVENT_SIGNATURE};

/// EIP-7708 system address — emitter of Transfer logs under Zero5.
pub const SYSTEM_ADDRESS: Address = address!("0xfffffffffffffffffffffffffffffffffffffffe");

/// NativeCoinAuthority precompile — emitter of NativeCoinTransferred logs before Zero5.
pub const NATIVE_COIN_AUTHORITY_ADDRESS: Address =
    address!("0x1800000000000000000000000000000000000000");

/// First account from test mnemonic (0xf39Fd...), funded in localdev genesis.
pub const WALLET_FIRST_ADDRESS: Address = address!("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
