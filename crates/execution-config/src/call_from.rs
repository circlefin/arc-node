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
pub const MEMO_ADDRESS: Address = address!("5294E9927c3306DcBaDb03fe70b92e01cCede505");

/// Address of the `Multicall3From` contract (CREATE2-deployed, zero salt).
pub const MULTICALL3_FROM_ADDRESS: Address = address!("A3E6c63b16321E39a61551Dc1A38689b04d62E42");
