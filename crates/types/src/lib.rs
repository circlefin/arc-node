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

// adapted from https://github.com/informalsystems/malachite/tree/v0.4.0/code/crates/test
#![forbid(unsafe_code)]
#![deny(trivial_casts, trivial_numeric_casts)]

mod address;
mod aliases;
mod certificate;
mod config;
mod consensus_params;
mod context;
mod height;
mod proposal;
pub mod proposal_monitor;
mod proposal_part;
mod proposal_parts;
mod validator_set;
mod value;
mod vote;

pub mod block;
pub mod codec;
pub mod evidence;
pub mod proposer;
pub mod proto;
pub mod rpc_sync;
pub mod signing;
pub mod ssz;
pub mod sync;

pub use malachitebft_core_types::Round;

pub use crate::address::*;
pub use crate::aliases::*;
pub use crate::certificate::*;
pub use crate::config::*;
pub use crate::consensus_params::*;
pub use crate::context::*;
pub use crate::height::*;
pub use crate::proposal::*;
pub use crate::proposal_part::*;
pub use crate::proposal_parts::*;
pub use crate::validator_set::*;
pub use crate::value::*;
pub use crate::vote::*;
