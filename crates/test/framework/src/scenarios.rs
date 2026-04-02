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

//! Reusable test scenarios.
//!
//! Each function builds a [`Test`] that describes _what_ should happen,
//! independent of _how_ nodes are run. Call `.run::<Runner>(...)` on the
//! returned value to execute it with any [`NodeRunner`](crate::NodeRunner) implementation.

use std::time::Duration;

use crate::expected::Expected;
use crate::node::Layer;
use crate::{HandlerResult, Test, TestBuilder};

/// Add `n` background validators that start and reach block `height`.
fn add_background_validators(builder: &mut TestBuilder<()>, n: usize, height: u64) {
    for _ in 0..n {
        builder.add_validator_start_until(height).success();
    }
}

/// Add `n` background full nodes that start and reach block `height`.
fn add_background_full_nodes(builder: &mut TestBuilder<()>, n: usize, height: u64) {
    for _ in 0..n {
        builder
            .add_node()
            .full_node()
            .start()
            .wait_until_block(height)
            .success();
    }
}

/// `n` validators start and all reach the given block `height`.
pub fn validators_reach_height(n: usize, height: u64) -> Test<()> {
    assert!(n >= 1, "scenario requires at least 1 node");
    let mut builder = TestBuilder::<()>::new();
    add_background_validators(&mut builder, n, height);
    builder.build()
}

/// `n` nodes: first node starts, reaches block `crash_height`, crashes the given
/// `layer`, restarts after `restart_delay`, and reaches block `height` again.
/// Remaining nodes provide background consensus support.
pub fn crash_and_restart(
    n: usize,
    crash_height: u64,
    restart_delay: Duration,
    height: u64,
    layer: Layer,
) -> Test<()> {
    assert!(n >= 1, "scenario requires at least 1 node");
    assert!(
        crash_height < height,
        "crash_height must be less than height"
    );
    let mut builder = TestBuilder::<()>::new();
    builder
        .add_validator_start_until(crash_height)
        .crash(layer)
        .restart_after(layer, restart_delay)
        .wait_until_block(height)
        .success();
    add_background_validators(&mut builder, n - 1, height);
    builder.build()
}

/// `num_vals` validators plus `num_fulls` non-validator nodes, all reaching block `height`.
pub fn validators_and_full_nodes_reach_height(
    num_vals: usize,
    num_fulls: usize,
    height: u64,
) -> Test<()> {
    assert!(num_vals >= 1, "scenario requires at least 1 validator");
    assert!(num_fulls >= 1, "scenario requires at least 1 full node");
    let mut builder = TestBuilder::<()>::new();
    add_background_validators(&mut builder, num_vals, height);
    add_background_full_nodes(&mut builder, num_fulls, height);
    builder.build()
}

/// `n` nodes: `n - 1` start immediately and reach block `height`; the last node
/// starts after `delay` and also reaches `height`.
///
/// Important: first nodes need to be up for more time than the last node to
/// provide sync support (at least more than `delay`).
pub fn delayed_start(n: usize, delay: Duration, height: u64) -> Test<()> {
    assert!(n >= 1, "scenario requires at least 1 node");
    let mut builder = TestBuilder::<()>::new();
    add_background_validators(&mut builder, n - 1, height);
    builder
        .add_node()
        .start_after(delay)
        .wait_until_block(height)
        .success();
    builder.build()
}

/// `n` nodes: all start and wait until consensus decides the given `height`.
pub fn wait_until_decision(n: usize, height: u64) -> Test<()> {
    assert!(n >= 1, "scenario requires at least 1 node");
    let mut builder = TestBuilder::<()>::new();
    for _ in 0..n {
        builder
            .add_node()
            .start()
            .wait_until_decision(height)
            .success();
    }
    builder.build()
}

/// `n` nodes: first node starts, reaches block `height`, stops the given
/// `layer`, then asserts decision count matches `expected`. Remaining nodes
/// provide background consensus support.
pub fn expect_decisions(n: usize, height: u64, expected: Expected, layer: Layer) -> Test<()> {
    assert!(n >= 1, "scenario requires at least 1 node");
    let mut builder = TestBuilder::<()>::new();
    builder
        .add_validator_start_until(height)
        .expect_decisions(expected, layer);
    add_background_validators(&mut builder, n - 1, height);
    builder.build()
}

// ===========================================================================
// Single-node error-path scenarios
// ===========================================================================

/// Single node starts and reaches block `height`.
///
/// This is a convenience shorthand for `validators_reach_height(1, height)`,
/// useful when a test only needs one node (e.g. fault-injection runners).
pub fn single_node_reach_height(height: u64) -> Test<()> {
    validators_reach_height(1, height)
}

/// Single node starts, reaches block `height`, then crashes the given `layer`
/// (no restart).
pub fn crash_only(height: u64, layer: Layer) -> Test<()> {
    let mut builder = TestBuilder::<()>::new();
    builder
        .add_validator_start_until(height)
        .crash(layer)
        .success();
    builder.build()
}

/// Single node starts, reaches block `height`, then restarts the given
/// `layer` after `restart_delay` without a prior explicit crash (the
/// framework kills the node as part of the restart step).
pub fn restart_without_crash(height: u64, restart_delay: Duration, layer: Layer) -> Test<()> {
    let mut builder = TestBuilder::<()>::new();
    builder
        .add_validator_start_until(height)
        .restart_after(layer, restart_delay)
        .success();
    builder.build()
}

/// Single node starts with an `on_event` handler that always returns
/// [`HandlerResult::WaitForNextEvent`].
///
/// Useful for testing channel-close behavior: the handler never advances,
/// so any channel closure surfaces as an error.
pub fn on_event_wait_forever() -> Test<()> {
    let mut builder = TestBuilder::<()>::new();
    builder
        .add_node()
        .start()
        .on_event(|_event, _state| Ok(HandlerResult::WaitForNextEvent))
        .success();
    builder.build()
}

/// Single node starts, reaches block `height`, stops the given `layer`,
/// then asserts decision count matches `expected`.
///
/// This is a convenience shorthand for `expect_decisions(1, height, expected, layer)`.
pub fn single_node_expect_decisions(height: u64, expected: Expected, layer: Layer) -> Test<()> {
    expect_decisions(1, height, expected, layer)
}
