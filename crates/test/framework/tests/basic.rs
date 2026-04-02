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

//! Tests for the test framework using [`MockRunner`].
//!
//! Covers happy-path step sequencing, event handling, assertion logic, and
//! error paths that only need the standard mock runner. Fault-injection
//! tests (custom runners with broken spawn/kill) live in `errors.rs`.

use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::B256;
use arc_consensus_types::Height;
use arc_test_framework::events::ArcEvent;
use arc_test_framework::expected::Expected;
use arc_test_framework::mock::MockNodeHandle;
use arc_test_framework::node::TestNodeConfig;
use arc_test_framework::params::TestParams;
use arc_test_framework::{scenarios, HandlerResult, Layer, NodeId, NodeRunner, TestBuilder};
use async_trait::async_trait;
use rstest::rstest;
use tokio::sync::broadcast;

// ===========================================================================
// MockRunner: standard mock that emits block + decision events
// ===========================================================================

/// Mock runner that simulates block production by emitting events.
///
/// Each spawned node emits `ConsensusDecided` + `BlockProduced` pairs for
/// heights 1..=100, one pair every 50 ms.
#[derive(Clone)]
struct MockRunner {
    _node_configs: Arc<Vec<TestNodeConfig>>,
}

#[async_trait]
impl NodeRunner for MockRunner {
    type Handle = MockNodeHandle;

    fn new(_test_id: usize, nodes: &[TestNodeConfig], _params: TestParams) -> Self {
        Self {
            _node_configs: Arc::new(nodes.to_vec()),
        }
    }

    async fn spawn(&self, _id: NodeId) -> eyre::Result<Self::Handle> {
        let (tx, _) = broadcast::channel(64);
        let tx_clone = tx.clone();

        // Simulate block production by emitting events.
        let task = tokio::spawn(async move {
            for h in 1..=100u64 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let height = Height::new(h);
                if tx_clone
                    .send(ArcEvent::ConsensusDecided {
                        height,
                        certificate: dummy_certificate(h),
                    })
                    .is_err()
                {
                    break;
                }
                if tx_clone
                    .send(ArcEvent::BlockProduced {
                        number: height,
                        hash: B256::ZERO,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Ok(MockNodeHandle::new(tx, Some(task)))
    }
}

fn dummy_certificate(
    height: u64,
) -> malachitebft_core_types::CommitCertificate<arc_consensus_types::ArcContext> {
    use arc_consensus_types::{ArcContext, Height, ValueId};
    use malachitebft_core_types::CommitCertificate;

    CommitCertificate::<ArcContext> {
        height: Height::new(height),
        round: malachitebft_core_types::Round::new(0),
        value_id: ValueId::new(B256::ZERO),
        commit_signatures: vec![],
    }
}

// ===========================================================================
// Happy-path tests
// ===========================================================================

#[tokio::test]
async fn four_nodes_reach_height_3() {
    scenarios::validators_reach_height(4, 3)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn node_with_event_handler() {
    let mut test = TestBuilder::<Vec<u64>>::new();

    test.add_node()
        .start()
        .on_event(|event, state: &mut Vec<u64>| {
            if let ArcEvent::BlockProduced { number, .. } = event {
                state.push(number.as_u64());
                if *number >= Height::new(3) {
                    return Ok(HandlerResult::ContinueTest);
                }
            }
            Ok(HandlerResult::WaitForNextEvent)
        })
        .success();

    test.build()
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn node_with_state() {
    let mut test = TestBuilder::<Vec<u64>>::new();

    test.add_node()
        .with_state(vec![999])
        .start()
        .on_event(|event, state: &mut Vec<u64>| {
            if let ArcEvent::BlockProduced { number, .. } = event {
                state.push(number.as_u64());
                if *number >= Height::new(3) {
                    assert_eq!(state[0], 999);
                    return Ok(HandlerResult::ContinueTest);
                }
            }
            Ok(HandlerResult::WaitForNextEvent)
        })
        .success();

    test.build()
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn expect_decisions() {
    scenarios::expect_decisions(1, 5, Expected::AtLeast(5), Layer::Both)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[rstest]
#[case::both(Layer::Both)]
#[case::cl(Layer::Consensus)]
#[case::el(Layer::Execution)]
#[tokio::test]
async fn crash_and_restart(#[case] layer: Layer) {
    scenarios::crash_and_restart(1, 3, Duration::from_millis(100), 5, layer)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn validators_and_full_nodes() {
    scenarios::validators_and_full_nodes_reach_height(2, 2, 3)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn delayed_start() {
    scenarios::delayed_start(2, Duration::from_millis(200), 3)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[rstest]
#[case::both(Layer::Both)]
#[case::cl(Layer::Consensus)]
#[case::el(Layer::Execution)]
#[tokio::test]
async fn crash_only(#[case] layer: Layer) {
    scenarios::crash_only(3, layer)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[rstest]
#[case::both(Layer::Both)]
#[case::cl(Layer::Consensus)]
#[case::el(Layer::Execution)]
#[tokio::test]
async fn restart_without_crash(#[case] layer: Layer) {
    scenarios::restart_without_crash(2, Duration::from_millis(50), layer)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}
// ---------------------------------------------------------------------------
// Additional step coverage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wait_until_decision() {
    scenarios::wait_until_decision(1, 5)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

// ---------------------------------------------------------------------------
// SleepAndContinueTest handler result
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sleep_and_continue_test() {
    let mut test = TestBuilder::<()>::new();

    test.add_node()
        .start()
        .on_event(|event, _state| {
            if let ArcEvent::BlockProduced { number, .. } = event {
                if *number >= Height::new(2) {
                    return Ok(HandlerResult::SleepAndContinueTest(Duration::from_millis(
                        50,
                    )));
                }
            }
            Ok(HandlerResult::WaitForNextEvent)
        })
        .wait_until_block(5)
        .success();

    test.build()
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

// ---------------------------------------------------------------------------
// Expected variants in integration (Expect step)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expect_exactly() {
    scenarios::expect_decisions(1, 5, Expected::Exactly(5), Layer::Both)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn expect_at_most() {
    scenarios::expect_decisions(1, 3, Expected::AtMost(5), Layer::Both)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn expect_less_than() {
    scenarios::expect_decisions(1, 3, Expected::LessThan(5), Layer::Both)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn expect_greater_than() {
    scenarios::expect_decisions(1, 3, Expected::GreaterThan(1), Layer::Both)
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

// ---------------------------------------------------------------------------
// Error paths using MockRunner
// ---------------------------------------------------------------------------

#[tokio::test]
#[should_panic(expected = "event handler error")]
async fn handler_error_fails_test() {
    let mut test = TestBuilder::<()>::new();

    test.add_node()
        .start()
        .on_event(|_event, _state| Err(eyre::eyre!("handler blew up")))
        .success();

    test.build().run::<MockRunner>(Duration::from_secs(5)).await;
}

#[tokio::test]
#[should_panic(expected = "intentional failure")]
async fn fail_step_fails_test() {
    let mut test = TestBuilder::<()>::new();

    test.add_node()
        .start()
        .wait_until_block(2)
        .fail("intentional failure");

    test.build().run::<MockRunner>(Duration::from_secs(5)).await;
}

#[rstest]
#[case::both(Layer::Both)]
#[case::cl(Layer::Consensus)]
#[case::el(Layer::Execution)]
#[tokio::test]
#[should_panic(expected = "expected exactly 1 decisions, got")]
async fn failed_expectation_fails_test(#[case] layer: Layer) {
    let mut test = TestBuilder::<()>::new();
    test.add_node()
        .start()
        .wait_until_block(5)
        .expect_decisions(Expected::Exactly(1), layer);
    test.build()
        .run::<MockRunner>(Duration::from_secs(10))
        .await;
}

#[tokio::test]
#[should_panic(expected = "timed out")]
async fn timeout_fails_test() {
    let mut test = TestBuilder::<()>::new();

    test.add_node().start().wait_until_block(999_999).success();

    test.build()
        .run::<MockRunner>(Duration::from_millis(200))
        .await;
}

#[tokio::test]
#[should_panic(expected = "explicit terminal step")]
async fn missing_terminal_step_fails_test() {
    let mut test = TestBuilder::<()>::new();

    test.add_node().start().wait_until_block(2);

    test.build().run::<MockRunner>(Duration::from_secs(5)).await;
}

#[tokio::test]
#[should_panic(expected = "test must have at least one node")]
async fn empty_test_fails() {
    TestBuilder::<()>::new()
        .build()
        .run::<MockRunner>(Duration::from_secs(1))
        .await;
}

#[tokio::test]
async fn run_with_params_uses_custom_params() {
    let params = TestParams {
        consensus_timeout_propose: Duration::from_secs(10),
        ..TestParams::default()
    };

    scenarios::validators_reach_height(1, 3)
        .run_with_params::<MockRunner>(Duration::from_secs(10), params)
        .await;
}
