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

#![allow(clippy::unwrap_used)]

//! Arc Integration Test Framework
//!
//! A generic, runtime-agnostic framework for defining and executing multi-node
//! Arc integration tests. It provides the builder API, step sequencer, and event
//! monitoring used by both **integration tests** (real networking) and, in the
//! future, **DST** (deterministic simulation testing).
//!
//! The framework decouples _what_ a test does from _how_ nodes are run. You define
//! nodes with a fluent builder, chain steps (wait, crash, restart, assert), and then
//! hand the test to a [`NodeRunner`] implementation that decides how to spawn and
//! wire the nodes.

pub mod events;
pub mod expected;
pub mod logging;
pub mod mock;
pub mod node;
pub mod params;
pub mod scenarios;

pub use arc_consensus_types::Height;
pub use events::ArcEvent;
pub use expected::Expected;
pub use malachitebft_core_types::VotingPower;
pub use node::{HandlerResult, Layer, Step, TestNode};
pub use params::TestParams;

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio::task::JoinSet;
use tracing::{error, info};

/// Per-process counter so that tests running in the same binary (cargo test)
/// get distinct port ranges even though they share a PID.
static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Unique identifier for a node within a test.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(usize);

impl NodeId {
    /// Create a new node identifier
    pub const fn new(id: usize) -> Self {
        Self(id)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Handle to a running node, returned by [`NodeRunner::spawn`].
///
/// Provides event subscription and lifecycle control.
///
/// # Crash vs graceful shutdown
///
/// Two shutdown modes exist for each layer:
///
/// - **Crash** (`kill_cl`/`kill_el`): hard abort: tasks are cancelled
///   immediately, modelling an abrupt process death. OS resources like ports
///   and file locks may remain held briefly after the call returns.
///
/// - **Graceful** (`shutdown_cl`/`shutdown_el`): cooperative shutdown that
///   waits for the engine, store, and task executor to release OS resources
///   before returning. Used by [`NodeRunner::restart`] to ensure ports
///   and file locks are free before respawning.
///
/// The default `shutdown_*` implementations delegate to the corresponding
/// `kill_*` methods. Implementations that manage real OS resources should
/// override them to wait for resource release.
///
/// # Drop contract
///
/// Implementations **must** clean up owned resources (child processes, background
/// tasks, temporary files) when dropped. The framework may drop handles without
/// calling [`kill_cl`](Self::kill_cl)/[`kill_el`](Self::kill_el) first. For
/// example, when a per-node timeout fires. Relying solely on explicit kill calls
/// for cleanup will leak resources in those cases.
#[async_trait]
pub trait NodeHandle: Send + Sync + 'static {
    /// Subscribe to events from this node.
    fn subscribe(&self) -> broadcast::Receiver<ArcEvent>;

    /// Hard-abort the consensus layer (crash semantics).
    async fn kill_cl(&self) -> eyre::Result<()>;

    /// Hard-abort the execution layer (crash semantics).
    async fn kill_el(&self) -> eyre::Result<()>;

    /// Gracefully shut down the consensus layer, waiting for OS resources
    /// (ports, file locks) to be released before returning.
    async fn shutdown_cl(&self) -> eyre::Result<()> {
        self.kill_cl().await
    }

    /// Gracefully shut down the execution layer, waiting for OS resources
    /// (ports, file locks) to be released before returning.
    async fn shutdown_el(&self) -> eyre::Result<()> {
        self.kill_el().await
    }
}

/// Pluggable backend for spawning and managing test nodes.
///
/// Implementations decide _how_ nodes are run:
/// - Integration: real networking, real EVM, real consensus
/// - DST (future): simulated network, tick-based scheduling
#[async_trait]
pub trait NodeRunner: Clone + Send + Sync + 'static {
    /// The handle type returned when spawning a node.
    type Handle: NodeHandle;

    /// Create a new runner from the test definition.
    ///
    /// Receives all nodes (for validator set construction) and test-wide params.
    fn new(test_id: usize, nodes: &[node::TestNodeConfig], params: TestParams) -> Self;

    /// Spawn a node and return a handle for event subscription and control.
    async fn spawn(&self, id: NodeId) -> eyre::Result<Self::Handle>;

    /// Restart a specific layer of a node, returning the updated handle.
    ///
    /// The default implementation gracefully shuts down the requested layer(s),
    /// drops the old handle, and does a full respawn. Runners that support
    /// partial restarts (e.g. restarting only CL while keeping EL running)
    /// should override this.
    async fn restart(
        &self,
        id: NodeId,
        handle: Self::Handle,
        layer: node::Layer,
    ) -> eyre::Result<Self::Handle> {
        shutdown_layer(&handle, layer).await?;
        drop(handle);
        self.spawn(id).await
    }
}

/// A built test definition, ready to be executed by a [`NodeRunner`].
pub struct Test<S = ()> {
    /// Unique test identifier.
    id: usize,
    /// The nodes participating in this test.
    nodes: Vec<TestNode<S>>,
}

impl<S: Default + Send + 'static> Test<S> {
    /// Run this test with the given runner type, timeout, and default params.
    pub async fn run<R: NodeRunner>(self, timeout: Duration) {
        run_test::<R, S>(self, timeout, TestParams::default()).await;
    }

    /// Run this test with the given runner type, timeout, and params.
    pub async fn run_with_params<R: NodeRunner>(self, timeout: Duration, params: TestParams) {
        run_test::<R, S>(self, timeout, params).await;
    }
}

/// Builder for constructing multi-node test definitions.
pub struct TestBuilder<S = ()> {
    nodes: Vec<TestNode<S>>,
}

impl<S: Default> TestBuilder<S> {
    /// Create a new empty test builder.
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Add a new node to the test and return a mutable reference to configure it.
    pub fn add_node(&mut self) -> &mut TestNode<S> {
        let idx = self.nodes.len();
        self.nodes.push(TestNode::new(NodeId::new(idx)));
        &mut self.nodes[idx]
    }

    /// Add a node that starts and waits until it reaches the given block `height`.
    pub fn add_validator_start_until(&mut self, height: u64) -> &mut TestNode<S> {
        self.add_node().start().wait_until_block(height)
    }

    /// Finalize the test definition.
    ///
    /// Each call gets a unique test ID so that tests running in the same
    /// process (e.g. `cargo test`) don't collide on ports. The PID seed
    /// provides cross-process isolation for `cargo nextest`.
    ///
    /// The ID space (0..65536) is large enough to make accidental collisions
    /// unlikely across parallel test processes. Real `NodeRunner` implementations
    /// use this ID to derive per-node port ranges.
    pub fn build(self) -> Test<S> {
        let pid = std::process::id() as usize;
        let seq = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        Test {
            id: (pid.wrapping_add(seq)) % 65536,
            nodes: self.nodes,
        }
    }
}

impl<S: Default> Default for TestBuilder<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of running a single node's step sequence.
#[derive(Debug)]
enum NodeOutcome {
    Success,
    Failed(String),
}

fn node_failure(node_id: NodeId, reason: impl Into<String>) -> NodeOutcome {
    NodeOutcome::Failed(format!("node {node_id}: {}", reason.into()))
}

/// Receive the next event from the broadcast channel, converting channel errors
/// into [`NodeOutcome::Failed`] with a descriptive message.
async fn recv_or_fail(
    rx: &mut broadcast::Receiver<ArcEvent>,
    node_id: NodeId,
    context: &str,
) -> Result<ArcEvent, NodeOutcome> {
    match rx.recv().await {
        Ok(event) => Ok(event),
        Err(broadcast::error::RecvError::Lagged(n)) => Err(node_failure(
            node_id,
            format!("event receiver lagged by {n} events while {context}"),
        )),
        Err(broadcast::error::RecvError::Closed) => Err(node_failure(
            node_id,
            format!("event channel closed while {context}"),
        )),
    }
}

/// Kill the specified layer(s) of a node (crash semantics).
///
/// For `Layer::Both`, both layers are always attempted even if the first
/// fails, so that the second layer is not left running with leaked resources.
async fn kill_layer(handle: &impl NodeHandle, layer: node::Layer) -> eyre::Result<()> {
    match layer {
        node::Layer::Consensus => handle.kill_cl().await,
        node::Layer::Execution => handle.kill_el().await,
        node::Layer::Both => {
            combine_results(handle.kill_cl().await, handle.kill_el().await, "kill")
        }
    }
}

/// Gracefully shut down the specified layer(s) of a node.
///
/// For `Layer::Both`, both layers are always attempted even if the first
/// fails, so that the second layer is not left running with leaked resources.
async fn shutdown_layer(handle: &impl NodeHandle, layer: node::Layer) -> eyre::Result<()> {
    match layer {
        node::Layer::Consensus => handle.shutdown_cl().await,
        node::Layer::Execution => handle.shutdown_el().await,
        node::Layer::Both => combine_results(
            handle.shutdown_cl().await,
            handle.shutdown_el().await,
            "shutdown",
        ),
    }
}

/// Combine results from CL and EL operations, preserving both errors when
/// both layers fail. `op` labels the operation in the dual-failure message.
fn combine_results(cl: eyre::Result<()>, el: eyre::Result<()>, op: &str) -> eyre::Result<()> {
    match (cl, el) {
        (Ok(()), res) | (res, Ok(())) => res,
        (Err(cl_err), Err(el_err)) => Err(eyre::eyre!(
            "{op} failed on both layers — CL: {cl_err:#}; EL: {el_err:#}"
        )),
    }
}

/// Execute a test: spawn all nodes, run their steps, and check results.
async fn run_test<R: NodeRunner, S: Default + Send + 'static>(
    test: Test<S>,
    timeout: Duration,
    params: TestParams,
) {
    logging::init_logging();

    assert!(!test.nodes.is_empty(), "test must have at least one node");

    let node_configs: Vec<node::TestNodeConfig> = test
        .nodes
        .iter()
        .map(|n| node::TestNodeConfig {
            id: n.id,
            voting_power: n.voting_power,
        })
        .collect();

    let runner = R::new(test.id, &node_configs, params.clone());

    let mut join_set = JoinSet::new();

    for node in test.nodes {
        let runner = runner.clone();
        let node_id = node.id;
        join_set.spawn(async move {
            let result = tokio::time::timeout(timeout, run_node(runner, node)).await;
            match result {
                Ok(outcome) => outcome,
                Err(_) => {
                    error!(%node_id, "Node timed out after {timeout:?}");
                    node_failure(node_id, format!("timed out after {timeout:?}"))
                }
            }
        });
    }

    check_results(&mut join_set).await;
}

/// Run a single node through its step sequence.
async fn run_node<R: NodeRunner, S: Default + Send + 'static>(
    runner: R,
    node: TestNode<S>,
) -> NodeOutcome {
    let node_id = node.id;

    if !node.start_delay.is_zero() {
        info!(%node_id, delay = ?node.start_delay, "Waiting before starting node");
        tokio::time::sleep(node.start_delay).await;
    }

    let mut handle: R::Handle = match runner.spawn(node_id).await {
        Ok(h) => h,
        Err(e) => {
            error!(%node_id, error = ?e, "Failed to spawn node");
            return node_failure(node_id, format!("spawn failed: {e:#}"));
        }
    };

    let mut rx = handle.subscribe();
    let mut state = node.state;
    let mut decisions: usize = 0;

    for (step_idx, step) in node.steps.into_iter().enumerate() {
        info!(%node_id, step_idx, %step, "Executing step");

        match step {
            Step::WaitUntilBlock(target_height) => loop {
                let event = match recv_or_fail(&mut rx, node_id, "waiting for block").await {
                    Ok(event) => event,
                    Err(outcome) => return outcome,
                };
                match event {
                    ArcEvent::BlockProduced { number, .. } if number >= target_height => {
                        info!(%node_id, height = %number, "Reached target block");
                        break;
                    }
                    ArcEvent::ConsensusDecided { height, .. } => {
                        decisions += 1;
                        if height >= target_height {
                            info!(%node_id, %height, "Reached target block via decision");
                            break;
                        }
                    }
                    _ => {}
                }
            },

            Step::WaitUntilDecision(target_height) => loop {
                let event = match recv_or_fail(&mut rx, node_id, "waiting for decision").await {
                    Ok(event) => event,
                    Err(outcome) => return outcome,
                };
                if let ArcEvent::ConsensusDecided { height, .. } = event {
                    decisions += 1;
                    if height >= target_height {
                        info!(%node_id, %height, "Reached target decision");
                        break;
                    }
                }
            },

            Step::OnEvent(handler) => loop {
                let event = match recv_or_fail(&mut rx, node_id, "in OnEvent handler").await {
                    Ok(event) => event,
                    Err(outcome) => return outcome,
                };
                if matches!(event, ArcEvent::ConsensusDecided { .. }) {
                    decisions += 1;
                }
                match handler(&event, &mut state) {
                    Ok(HandlerResult::WaitForNextEvent) => continue,
                    Ok(HandlerResult::ContinueTest) => break,
                    Ok(HandlerResult::SleepAndContinueTest(d)) => {
                        tokio::time::sleep(d).await;
                        break;
                    }
                    Err(e) => {
                        return node_failure(node_id, format!("event handler error: {e}"));
                    }
                }
            },

            Step::Crash(delay, layer) => {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                if let Err(e) = kill_layer(&handle, layer).await {
                    return node_failure(node_id, format!("crash/kill failed: {e}"));
                }
                info!(%node_id, "Node crashed");
            }

            Step::Restart(delay, layer) => {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                handle = match runner.restart(node_id, handle, layer).await {
                    Ok(h) => h,
                    Err(e) => {
                        return node_failure(node_id, format!("restart failed: {e:#}"));
                    }
                };
                rx = handle.subscribe();
                info!(%node_id, "Node restarted");
            }

            Step::Expect(expected, layer) => {
                if let Err(e) = kill_layer(&handle, layer).await {
                    return node_failure(
                        node_id,
                        format!("failed to kill node before expect: {e}"),
                    );
                }
                if expected.check(decisions) {
                    info!(%node_id, decisions, %expected, "Expectation met");
                    info!(%node_id, "Node test passed");
                    return NodeOutcome::Success;
                } else {
                    return node_failure(
                        node_id,
                        format!("expected {expected} decisions, got {decisions}"),
                    );
                }
            }

            Step::Success => {
                info!(%node_id, "Node test passed");
                return NodeOutcome::Success;
            }

            Step::Fail(reason) => {
                return node_failure(node_id, reason);
            }
        }
    }

    node_failure(
        node_id,
        "step sequence ended without an explicit terminal step (use .success(), .fail(...), or .expect_decisions(...))"
            .to_string(),
    )
}

/// Collect results from all node tasks and fail if any node failed.
async fn check_results(join_set: &mut JoinSet<NodeOutcome>) {
    let mut failures = Vec::new();

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(NodeOutcome::Success) => {}
            Ok(NodeOutcome::Failed(reason)) => {
                error!(%reason, "Node failed");
                failures.push(reason);
            }
            Err(e) => {
                let msg = if e.is_panic() {
                    match e.into_panic().downcast::<String>() {
                        Ok(s) => format!("panic: {s}"),
                        Err(payload) => match payload.downcast::<&str>() {
                            Ok(s) => format!("panic: {s}"),
                            Err(_) => "panic: <non-string payload>".to_string(),
                        },
                    }
                } else {
                    format!("task cancelled: {e}")
                };
                error!(%msg, "Node task failed");
                failures.push(msg);
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Test failed with {} failure(s):\n{}",
            failures.len(),
            failures
                .iter()
                .enumerate()
                .map(|(i, f)| format!("  {}: {f}", i + 1))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    info!("All nodes passed");
}
