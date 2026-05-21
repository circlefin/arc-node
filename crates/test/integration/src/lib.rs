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

//! Integration test runner for Arc nodes.
//!
//! This crate provides [`ArcNodeRunner`], a [`NodeRunner`](arc_test_framework::NodeRunner)
//! implementation that spawns real in-process Arc nodes (Reth EVM + Malachite
//! BFT + IPC Engine API + libp2p P2P).
//!
//! It lives in a separate crate from `arc-test-framework` so that the framework
//! itself (builder API, step sequencer, scenarios) compiles without pulling in
//! the full node dependency tree. Run integration tests with:
//!
//! ```sh
//! cargo nextest run -p arc-test-integration
//! ```

#![allow(clippy::arithmetic_side_effects)]

mod bridge;
mod runner;

pub use runner::ArcNodeRunner;
