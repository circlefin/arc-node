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

//! Arc Network - A custom Reth node implementation
//!
//! This crate demonstrates how to build a custom blockchain node using Reth
//! with custom EVM configuration, precompiles, and transaction pool.

mod args;

pub use args::patch_node_command_defaults;

pub mod metrics;

// Re-export commonly used types
pub use arc_evm::{ArcEvmConfig, ArcEvmFactory};
pub use arc_evm_node::ArcEngineValidator;
pub use arc_execution_validation::ArcConsensus;
