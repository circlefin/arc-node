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

//! Test node definition with step sequencer and fluent builder API.

use std::fmt;
use std::time::Duration;

use arc_consensus_types::Height;
use malachitebft_core_types::VotingPower;

use crate::events::ArcEvent;
use crate::expected::Expected;
use crate::NodeId;

/// Which layer(s) a lifecycle step (crash, restart, expect) targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// Consensus layer only.
    Consensus,
    /// Execution layer only.
    Execution,
    /// Both layers.
    Both,
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Consensus => write!(f, "CL"),
            Self::Execution => write!(f, "EL"),
            Self::Both => write!(f, "Both"),
        }
    }
}

/// Result of an event handler, controlling the step sequencer's next action.
#[derive(Debug, Clone)]
pub enum HandlerResult {
    /// Keep listening for the next event.
    WaitForNextEvent,
    /// Move on to the next step immediately.
    ContinueTest,
    /// Sleep for the given duration, then move to the next step.
    SleepAndContinueTest(Duration),
}

/// A closure that handles events during an [`Step::OnEvent`] step.
pub type EventHandler<S> =
    Box<dyn Fn(&ArcEvent, &mut S) -> eyre::Result<HandlerResult> + Send + Sync + 'static>;

/// A single step in a node's test sequence.
///
/// Steps are executed sequentially. Each step blocks until its condition is met,
/// then the next step begins.
pub enum Step<S = ()> {
    /// Wait until the node produces or decides a block at the given height.
    WaitUntilBlock(Height),
    /// Wait until consensus decides the given height.
    WaitUntilDecision(Height),
    /// Receive events and call a handler until it returns
    /// [`HandlerResult::ContinueTest`] or [`HandlerResult::SleepAndContinueTest`].
    OnEvent(EventHandler<S>),
    /// Kill the node (optionally after a delay), targeting the given layer(s).
    Crash(Duration, Layer),
    /// Respawn the node (optionally after a delay), killing the given layer(s) first.
    Restart(Duration, Layer),
    /// Stop the given layer(s), then assert the number of decisions.
    Expect(Expected, Layer),
    /// Mark the node's test as passed.
    Success,
    /// Mark the node's test as failed with the given reason.
    Fail(String),
}

impl<S> fmt::Display for Step<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WaitUntilBlock(h) => write!(f, "WaitUntilBlock({h})"),
            Self::WaitUntilDecision(h) => write!(f, "WaitUntilDecision({h})"),
            Self::OnEvent(_) => write!(f, "OnEvent(...)"),
            Self::Crash(d, layer) => write!(f, "Crash({d:?}, {layer})"),
            Self::Restart(d, layer) => write!(f, "Restart({d:?}, {layer})"),
            Self::Expect(e, layer) => write!(f, "Expect({e}, {layer})"),
            Self::Success => write!(f, "Success"),
            Self::Fail(r) => write!(f, "Fail({r})"),
        }
    }
}

/// Static configuration extracted from a [`TestNode`] for the runner.
///
/// This is the subset of node config that the [`NodeRunner`](crate::NodeRunner) needs
/// to construct the validator set and allocate resources.
#[derive(Clone, Debug)]
pub struct TestNodeConfig {
    /// Node identifier.
    pub id: NodeId,
    /// Voting power (0 = full node / non-validator).
    pub voting_power: VotingPower,
}

/// A node participating in a test, with its configuration and step sequence.
///
/// Use the fluent builder methods to configure the node and define its behavior.
pub struct TestNode<S = ()> {
    /// Node identifier (assigned by [`TestBuilder`](crate::TestBuilder)).
    pub(crate) id: NodeId,
    /// Voting power (0 = full node / non-validator).
    pub(crate) voting_power: VotingPower,
    /// Delay before spawning this node.
    pub(crate) start_delay: Duration,
    /// Ordered list of steps to execute.
    pub(crate) steps: Vec<Step<S>>,
    /// Mutable per-node state available to event handlers.
    pub(crate) state: S,
}

impl<S: Default> TestNode<S> {
    /// Create a new node with default settings.
    pub(crate) fn new(id: NodeId) -> Self {
        Self {
            id,
            voting_power: 1,
            start_delay: Duration::ZERO,
            steps: Vec::new(),
            state: S::default(),
        }
    }

    // -- Configuration methods --

    /// Set the node's voting power. Use 0 for a full node (non-validator).
    pub fn with_voting_power(&mut self, power: VotingPower) -> &mut Self {
        self.voting_power = power;
        self
    }

    /// Configure this node as a full node (non-validator, voting power = 0).
    pub fn full_node(&mut self) -> &mut Self {
        self.voting_power = 0;
        self
    }

    /// Set the initial per-node state for event handlers.
    pub fn with_state(&mut self, state: S) -> &mut Self {
        self.state = state;
        self
    }

    // -- Step builder methods (fluent API) --

    /// Start the node immediately (no delay).
    pub fn start(&mut self) -> &mut Self {
        self.start_after(Duration::ZERO)
    }

    /// Start the node after a delay.
    pub fn start_after(&mut self, delay: Duration) -> &mut Self {
        self.start_delay = delay;
        self
    }

    /// Wait until the node reaches the given block height (via production or decision).
    pub fn wait_until_block(&mut self, height: u64) -> &mut Self {
        self.steps.push(Step::WaitUntilBlock(Height::new(height)));
        self
    }

    /// Wait until consensus decides the given height.
    pub fn wait_until_decision(&mut self, height: u64) -> &mut Self {
        self.steps
            .push(Step::WaitUntilDecision(Height::new(height)));
        self
    }

    /// Register an event handler that runs until it returns
    /// [`HandlerResult::ContinueTest`] or [`HandlerResult::SleepAndContinueTest`].
    pub fn on_event<F>(&mut self, handler: F) -> &mut Self
    where
        F: Fn(&ArcEvent, &mut S) -> eyre::Result<HandlerResult> + Send + Sync + 'static,
    {
        self.steps.push(Step::OnEvent(Box::new(handler)));
        self
    }

    /// Kill the given layer(s) of the node immediately.
    pub fn crash(&mut self, layer: Layer) -> &mut Self {
        self.crash_after(layer, Duration::ZERO)
    }

    /// Kill the given layer(s) of the node after a delay.
    pub fn crash_after(&mut self, layer: Layer, delay: Duration) -> &mut Self {
        self.steps.push(Step::Crash(delay, layer));
        self
    }

    /// Restart the given layer(s) of the node after a delay.
    pub fn restart_after(&mut self, layer: Layer, delay: Duration) -> &mut Self {
        self.steps.push(Step::Restart(delay, layer));
        self
    }

    /// Stop the given layer(s), then assert the number of consensus decisions.
    pub fn expect_decisions(&mut self, expected: Expected, layer: Layer) -> &mut Self {
        self.steps.push(Step::Expect(expected, layer));
        self
    }

    /// Mark this node's test as passed (must be the last step).
    pub fn success(&mut self) -> &mut Self {
        self.steps.push(Step::Success);
        self
    }

    /// Mark this node's test as failed.
    pub fn fail(&mut self, reason: impl Into<String>) -> &mut Self {
        self.steps.push(Step::Fail(reason.into()));
        self
    }
}
