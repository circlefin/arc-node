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

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

//! Arc E2E Test Framework
//!
//! New tests should use `ArcTestNode`, which exposes the live node and small
//! helpers for common Engine API and transaction operations.

pub mod chainspec;
mod node;
mod setup;

pub use alloy_primitives::TxKind;
pub use node::{ArcTestNode, DEFAULT_FEE_RECIPIENT};
pub use setup::ArcSetup;
