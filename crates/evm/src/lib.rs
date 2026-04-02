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

//! Arc EVM configuration, executor, and handler.
//!
//! This crate contains the EVM customization for Arc Network: block assembler,
//! block executor, handler, opcode overrides, and frame result utilities.

pub mod assembler;
pub mod evm;
pub mod executor;
pub mod frame_result;
pub mod handler;
mod log;
pub mod opcode;
pub mod subcall;
#[cfg(test)]
mod subcall_test;

// Re-export commonly used types
pub use evm::{ArcEvm, ArcEvmConfig, ArcEvmFactory};
