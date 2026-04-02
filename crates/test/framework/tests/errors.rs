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

//! Error path / negative tests for the test framework.
//!
//! Each test exercises a specific failure mode (spawn, kill, restart,
//! channel closed, lagged receiver, handler error, timeout, missing terminal step)
//! and verifies the framework surfaces a clear panic message.
//!
//! Fault-injection [`NodeRunner`] implementations are co-located here so each
//! runner sits next to the test(s) that use it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::B256;
use arc_consensus_types::Height;
use arc_test_framework::events::ArcEvent;
use arc_test_framework::expected::Expected;
use arc_test_framework::mock::MockNodeHandle;
use arc_test_framework::node::TestNodeConfig;
use arc_test_framework::params::TestParams;
use arc_test_framework::{
    scenarios, HandlerResult, Layer, NodeHandle, NodeId, NodeRunner, TestBuilder,
};
use async_trait::async_trait;
use rstest::rstest;
use tokio::sync::broadcast;

// ===========================================================================
// FailSpawnRunner: spawn() always fails
// ===========================================================================

#[derive(Clone)]
struct FailSpawnRunner;

#[async_trait]
impl NodeRunner for FailSpawnRunner {
    type Handle = MockNodeHandle;

    fn new(_: usize, _: &[TestNodeConfig], _: TestParams) -> Self {
        Self
    }

    async fn spawn(&self, _id: NodeId) -> eyre::Result<Self::Handle> {
        Err(eyre::eyre!("spawn failed intentionally"))
    }
}

#[tokio::test]
#[should_panic(expected = "spawn failed")]
async fn spawn_failure_fails_test() {
    scenarios::single_node_reach_height(3)
        .run::<FailSpawnRunner>(Duration::from_secs(5))
        .await;
}

// ===========================================================================
// ChannelClosedRunner: channel closed immediately
// ===========================================================================

struct ClosedHandle {
    rx: broadcast::Receiver<ArcEvent>,
}

#[async_trait]
impl NodeHandle for ClosedHandle {
    fn subscribe(&self) -> broadcast::Receiver<ArcEvent> {
        self.rx.resubscribe()
    }

    async fn kill_cl(&self) -> eyre::Result<()> {
        Ok(())
    }

    async fn kill_el(&self) -> eyre::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct ChannelClosedRunner;

#[async_trait]
impl NodeRunner for ChannelClosedRunner {
    type Handle = ClosedHandle;

    fn new(_: usize, _: &[TestNodeConfig], _: TestParams) -> Self {
        Self
    }

    async fn spawn(&self, _id: NodeId) -> eyre::Result<Self::Handle> {
        let (tx, rx) = broadcast::channel(16);
        drop(tx);
        Ok(ClosedHandle { rx })
    }
}

#[tokio::test]
#[should_panic(expected = "event channel closed")]
async fn channel_closed_fails_test() {
    scenarios::single_node_reach_height(3)
        .run::<ChannelClosedRunner>(Duration::from_secs(5))
        .await;
}

// ===========================================================================
// LaggedRunner: floods events to trigger RecvError::Lagged
// ===========================================================================

#[derive(Clone)]
struct LaggedRunner;

#[async_trait]
impl NodeRunner for LaggedRunner {
    type Handle = MockNodeHandle;

    fn new(_: usize, _: &[TestNodeConfig], _: TestParams) -> Self {
        Self
    }

    async fn spawn(&self, _id: NodeId) -> eyre::Result<Self::Handle> {
        let (tx, _) = broadcast::channel(4);
        let tx_clone = tx.clone();
        let task = tokio::spawn(async move {
            for h in 1..=128u64 {
                if tx_clone
                    .send(ArcEvent::BlockProduced {
                        number: Height::new(h),
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

#[tokio::test]
#[should_panic(expected = "event receiver lagged")]
async fn lagged_receiver_fails_test() {
    scenarios::single_node_reach_height(1_000)
        .run::<LaggedRunner>(Duration::from_secs(5))
        .await;
}

// ===========================================================================
// OnEventClosedRunner: sends one event then closes channel
// ===========================================================================

#[derive(Clone)]
struct OnEventClosedRunner;

#[async_trait]
impl NodeRunner for OnEventClosedRunner {
    type Handle = ClosedHandle;

    fn new(_: usize, _: &[TestNodeConfig], _: TestParams) -> Self {
        Self
    }

    async fn spawn(&self, _id: NodeId) -> eyre::Result<Self::Handle> {
        let (tx, rx) = broadcast::channel(16);
        let _ = tx.send(ArcEvent::BlockProduced {
            number: Height::new(1),
            hash: B256::ZERO,
        });
        drop(tx);
        Ok(ClosedHandle { rx })
    }
}

#[tokio::test]
#[should_panic(expected = "event channel closed while in OnEvent handler")]
async fn on_event_channel_closed_fails_test() {
    scenarios::on_event_wait_forever()
        .run::<OnEventClosedRunner>(Duration::from_secs(5))
        .await;
}

// ===========================================================================
// FailKillRunner: handle whose kill_cl() and kill_el() always fail
// ===========================================================================

struct FailKillHandle {
    tx: broadcast::Sender<ArcEvent>,
    _task: tokio::task::JoinHandle<()>,
}

#[async_trait]
impl NodeHandle for FailKillHandle {
    fn subscribe(&self) -> broadcast::Receiver<ArcEvent> {
        self.tx.subscribe()
    }

    async fn kill_cl(&self) -> eyre::Result<()> {
        Err(eyre::eyre!("kill failed intentionally"))
    }

    async fn kill_el(&self) -> eyre::Result<()> {
        Err(eyre::eyre!("kill failed intentionally"))
    }
}

#[derive(Clone)]
struct FailKillRunner;

#[async_trait]
impl NodeRunner for FailKillRunner {
    type Handle = FailKillHandle;

    fn new(_: usize, _: &[TestNodeConfig], _: TestParams) -> Self {
        Self
    }

    async fn spawn(&self, _id: NodeId) -> eyre::Result<Self::Handle> {
        let (tx, _) = broadcast::channel(64);
        let tx_clone = tx.clone();
        let task = tokio::spawn(async move {
            for h in 1..=100u64 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if tx_clone
                    .send(ArcEvent::BlockProduced {
                        number: Height::new(h),
                        hash: B256::ZERO,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Ok(FailKillHandle { tx, _task: task })
    }
}

#[tokio::test]
#[should_panic(expected = "crash/kill failed")]
async fn kill_failure_on_crash_fails_test() {
    scenarios::crash_only(2, Layer::Both)
        .run::<FailKillRunner>(Duration::from_secs(5))
        .await;
}

#[rstest]
#[case::both(Layer::Both)]
#[case::cl(Layer::Consensus)]
#[case::el(Layer::Execution)]
#[tokio::test]
#[should_panic(expected = "restart failed")]
async fn kill_failure_on_restart_without_crash(#[case] layer: Layer) {
    scenarios::restart_without_crash(2, Duration::from_millis(50), layer)
        .run::<FailKillRunner>(Duration::from_secs(5))
        .await;
}

#[tokio::test]
#[should_panic(expected = "failed to kill node before expect")]
async fn kill_failure_on_expect_fails_test() {
    scenarios::single_node_expect_decisions(2, Expected::AtLeast(1), Layer::Both)
        .run::<FailKillRunner>(Duration::from_secs(5))
        .await;
}

// ===========================================================================
// FailRestartRunner: first spawn succeeds, subsequent calls fail
//
// NOTE: Uses a single AtomicBool, so only suitable for single-node tests.
// With multiple nodes the second node's initial spawn would fail instead
// of a restart.
// ===========================================================================

#[derive(Clone)]
struct FailRestartRunner {
    spawned: Arc<AtomicBool>,
}

#[async_trait]
impl NodeRunner for FailRestartRunner {
    type Handle = MockNodeHandle;

    fn new(_: usize, _: &[TestNodeConfig], _: TestParams) -> Self {
        Self {
            spawned: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn spawn(&self, _id: NodeId) -> eyre::Result<Self::Handle> {
        if self.spawned.swap(true, Ordering::SeqCst) {
            return Err(eyre::eyre!("restart spawn failed intentionally"));
        }
        let (tx, _) = broadcast::channel(64);
        let tx_clone = tx.clone();
        let task = tokio::spawn(async move {
            for h in 1..=100u64 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if tx_clone
                    .send(ArcEvent::BlockProduced {
                        number: Height::new(h),
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

#[tokio::test]
#[should_panic(expected = "restart failed: restart spawn failed intentionally")]
async fn restart_failure_fails_test() {
    scenarios::crash_and_restart(1, 2, Duration::from_millis(50), 3, Layer::Both)
        .run::<FailRestartRunner>(Duration::from_secs(5))
        .await;
}

// ===========================================================================
// SimpleEmitterRunner: emits events so handlers can run
// ===========================================================================

#[derive(Clone)]
struct SimpleEmitterRunner;

#[async_trait]
impl NodeRunner for SimpleEmitterRunner {
    type Handle = MockNodeHandle;

    fn new(_: usize, _: &[TestNodeConfig], _: TestParams) -> Self {
        Self
    }

    async fn spawn(&self, _id: NodeId) -> eyre::Result<Self::Handle> {
        let (tx, _) = broadcast::channel(64);
        let tx_clone = tx.clone();
        let task = tokio::spawn(async move {
            for h in 1..=100u64 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if tx_clone
                    .send(ArcEvent::BlockProduced {
                        number: Height::new(h),
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

// ===========================================================================
// Task panic: verify check_results surfaces the panic payload
// ===========================================================================

#[tokio::test]
#[should_panic(expected = "handler panicked on purpose")]
async fn task_panic_surfaces_in_check_results() {
    let mut test = TestBuilder::<()>::new();

    test.add_node()
        .start()
        .on_event(|event, _state| {
            if let ArcEvent::BlockProduced { number, .. } = event {
                if *number >= Height::new(2) {
                    panic!("handler panicked on purpose");
                }
            }
            Ok(HandlerResult::WaitForNextEvent)
        })
        .success();

    test.build()
        .run::<SimpleEmitterRunner>(Duration::from_secs(5))
        .await;
}
