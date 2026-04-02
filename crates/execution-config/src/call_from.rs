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

//! Addresses of contracts authorized to call the CallFrom precompile.
//!
//! These are CREATE2-precomputed addresses deployed with a zero salt.

use alloy_primitives::{address, Address};

/// Address of the `Memo` contract (CREATE2-deployed, zero salt).
pub const MEMO_ADDRESS: Address = address!("9702466268ccF55eAB64cdf484d272Ac08d3b75b");

/// Address of the `Multicall3From` contract (CREATE2-deployed, zero salt).
pub const MULTICALL3_FROM_ADDRESS: Address = address!("Eb7cc06E3D3b5F9F9a5fA2B31B477ff72bB9c8b6");
