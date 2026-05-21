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

//! Integration tests for the CL+EL runner.
//!
//! # Validator count is fixed at 5
//!
//! The `ArcNodeRunner` boots Reth with `LOCAL_DEV`, whose `ValidatorRegistry`
//! (`0x3600…0002`) is pre-seeded at genesis with exactly 5 validators derived
//! from BIP39 children 2..=6 (see `scripts/hardhat/tasks/genesis.ts`
//! `numValidators=5` and `crates/malachite-cli/src/new.rs::generate_private_keys`).
//!
//! Malachite reads the active validator set from that contract on startup via
//! `engine_eth.get_active_validator_set`. If a test spawns fewer validator
//! nodes than the registry contains, the absent ones are still in the set;
//! when round-robin proposer selection lands on an offline validator the
//! round cannot complete, `Rebroadcast` timers fire, and the test's 60 s
//! per-node budget expires before consensus finalises block 1.
//!
//! Consequence: every test in this file spawns **five** validator nodes.
//! Full (non-validating) nodes may be added on top. They don't appear in the
//! registry and so don't constrain the validator count. If you need a test
//! with fewer or more validators you'll need a bespoke chainspec with a
//! matching registry; that's out of scope for the moment.

use std::time::Duration;

use arc_test_framework::expected::Expected;
use arc_test_framework::{scenarios, Layer};
use arc_test_integration::ArcNodeRunner;
use rstest::rstest;

#[tokio::test]
async fn validators_reach_height_3() {
    scenarios::validators_reach_height(5, 3)
        .run::<ArcNodeRunner>(Duration::from_secs(60))
        .await;
}

#[rstest]
#[case::both(Layer::Both)]
#[case::cl(Layer::Consensus)]
#[case::el(Layer::Execution)]
#[tokio::test]
async fn crash_and_restart(#[case] layer: Layer) {
    scenarios::crash_and_restart(5, 3, Duration::from_secs(1), 10, layer)
        .run::<ArcNodeRunner>(Duration::from_secs(60))
        .await;
}

#[tokio::test]
async fn validators_and_full_nodes_reach_height_3() {
    scenarios::validators_and_full_nodes_reach_height(5, 5, 2, 3)
        .run::<ArcNodeRunner>(Duration::from_secs(60))
        .await;
}

#[tokio::test]
async fn delayed_start() {
    scenarios::delayed_start(5, Duration::from_secs(3), 10)
        .run::<ArcNodeRunner>(Duration::from_secs(60))
        .await;
}

#[tokio::test]
async fn wait_until_decision() {
    scenarios::wait_until_decision(5, 3)
        .run::<ArcNodeRunner>(Duration::from_secs(60))
        .await;
}

#[rstest]
#[case::both(Layer::Both)]
#[case::cl(Layer::Consensus)]
#[case::el(Layer::Execution)]
#[tokio::test]
async fn expect_at_least_decisions(#[case] layer: Layer) {
    scenarios::expect_decisions(5, 3, Expected::AtLeast(3), layer)
        .run::<ArcNodeRunner>(Duration::from_secs(60))
        .await;
}
