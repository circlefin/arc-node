# Test Framework

A framework for defining and executing multi-node Arc integration tests, based
on [Malachite's test framework](https://github.com/informalsystems/malachite/tree/main/code/crates/test/framework).

It provides a builder API, step sequencer, and event monitoring for testing
full-stack Arc nodes with real networking.

## Overview

The framework decouples _what_ a test does from _how_ nodes are run:

- **What** the test definition: "start 3 validators, wait until block 5, crash
  node 2, restart it after 3 seconds, assert it catches up to block 10."
- **How** the `NodeRunner` implementation: spawn real in-process Arc nodes
  with IPC Engine API, or (in the future) run Byzantine nodes, or run under
  deterministic simulation.

You define nodes with a fluent builder, chain steps (`wait`, `crash`, `restart`, `assert`),
and then hand the test to a `NodeRunner` that decides how to spawn and wire them.

Unlike the Malachite test framework (which tests consensus in isolation), this
framework tests **full-stack Arc nodes**. Each test node runs both an execution
layer (Reth) and a consensus layer (Malachite), connected via Engine API.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                      Test definition                         │
│   TestBuilder::new()                                         │
│     .add_node()  →  start / crash / wait_until_block / ...   │
│     .add_node()  →  ...                                      │
│     .build()     →  Test { nodes, id }                       │
└────────────────────────────┬─────────────────────────────────┘
                             │
                   run_test::<R, S>(...)
                             │
          ┌──────────────────┴────────────────────┐
          │  NodeRunner (trait)                   │
          │  - new(id, nodes, params, config)     │
          │  - spawn(id) → NodeHandle             │
          └──────────────────┬────────────────────┘
                             │
                             ▼
                  ┌─────────────────────┐
                  │  NodeRunner impl    │
                  │  (MockRunner, or    │
                  │   future real node  │
                  │   runner)           │
                  └─────────────────────┘
```

Each node runs concurrently in a `JoinSet` task. The step sequencer processes
each node's steps in order, reacting to events from a unified `ArcEvent` stream
that bridges both consensus and execution layer activity.

### Modules

| Module      | Description                                                                                    |
| ----------- | ---------------------------------------------------------------------------------------------- |
| `lib`       | `TestBuilder`, `Test`, `run_test`, `NodeRunner` trait, `NodeHandle` trait, step execution loop |
| `node`      | `TestNode` builder with fluent API, `Step` enum, `HandlerResult`, event handler types          |
| `params`    | `TestParams`: test-wide configuration (consensus timeouts, block time, gas limit)              |
| `expected`  | `Expected` enum for flexible decision count assertions (`Exactly`, `AtLeast`, `AtMost`, ...)   |
| `events`    | `ArcEvent`: unified event type bridging consensus and execution layer events                   |
| `mock`      | `MockNodeHandle`: shared mock `NodeHandle` implementation used by test-local runners           |
| `scenarios` | Reusable test scenario builders: runner-agnostic `Test` constructors for common patterns       |
| `logging`   | `init_logging()`: tracing-subscriber setup with `RUST_LOG` env filter                          |

## Core Abstractions

### `NodeRunner` trait

The key abstraction for node management. Any test runner must implement:

```rust
#[async_trait]
pub trait NodeRunner: Clone + Send + Sync + 'static {
    type Handle: NodeHandle;

    fn new(test_id: usize, nodes: &[TestNodeConfig], params: TestParams) -> Self;
    async fn spawn(&self, id: NodeId) -> eyre::Result<Self::Handle>;
    async fn restart(&self, id: NodeId, handle: Self::Handle, layer: node::Layer) -> eyre::Result<Self::Handle>
}
```

- **`new`** receives all nodes (for validator set construction) and test-wide params.
- **`spawn`** starts a node and returns a handle that supports event subscription and lifecycle control.
- **`restart`** restarts one or both layers of a node, returning the updated handle.

Unlike Malachite's `NodeRunner<Ctx>` (generic over `Context`), this trait is
Arc-specific. It always uses `ArcContext`, which simplifies the type signatures.

### `NodeHandle` trait

```rust
#[async_trait]
pub trait NodeHandle: Send + Sync + 'static {
    fn subscribe(&self) -> broadcast::Receiver<ArcEvent>;
    async fn kill_cl(&self) -> eyre::Result<()>;
    async fn kill_el(&self) -> eyre::Result<()>;
    async fn shutdown_cl(&self) -> eyre::Result<()> { self.kill_cl().await }
    async fn shutdown_el(&self) -> eyre::Result<()> { self.kill_el().await }
}
```

- **`subscribe`** returns a receiver for the unified event stream.
- **`kill_cl` / `kill_el`** hard-abort the consensus / execution layer (crash
  semantics). Tasks are cancelled immediately; OS resources may linger briefly.
- **`shutdown_cl` / `shutdown_el`** gracefully shut down the consensus /
  execution layer, waiting for ports, file locks, and other OS resources to be
  released before returning. The defaults delegate to the corresponding `kill_*`
  methods; implementations that manage real OS resources should override them.

The per-layer methods reflect Arc's two-binary architecture and allow
fault-injection scenarios that target a single layer (e.g., crash consensus
while execution keeps running). Two helpers in `lib.rs`, `kill_layer()` and
`shutdown_layer()`, dispatch to the appropriate method(s) based on the `Layer`
enum. `NodeRunner::restart` uses `shutdown_layer` to ensure a clean slate before
respawning.

**Drop contract:** Implementations must clean up owned resources (child processes,
background tasks) when dropped. The framework may drop handles without calling
`kill_cl`/`kill_el` first (e.g., on timeout), so relying solely on explicit kill
calls will leak resources.

### `ArcEvent` unified event enum

Bridges consensus events (from `TxEvent<ArcContext>`) and execution events into
a single stream:

```rust
pub enum ArcEvent {
    // Consensus layer
    ConsensusStartedHeight { height: Height },
    ConsensusDecided { height: Height, certificate: CommitCertificate<ArcContext> },
    ConsensusFinalized { height: Height },
    ConsensusProposedValue { height: Height, round: Round },

    // Execution layer
    BlockProduced { number: Height, hash: B256 },
}
```

### `TestNode` and the Step Sequencer

Each `TestNode` carries a list of `Step`s executed sequentially:

| Step                        | Description                                                          |
| --------------------------- | -------------------------------------------------------------------- |
| `WaitUntilBlock(height)`    | Block until the node produces or decides a block at the given height |
| `WaitUntilDecision(height)` | Block until consensus decides the given height                       |
| `OnEvent(handler)`          | Receive events and call a closure until it returns `ContinueTest`    |
| `Crash(delay, layer)`       | Kill the given layer(s) of the node (optionally after a delay)       |
| `Restart(delay, layer)`     | Shutdown the given layer(s), then respawn the node after a delay         |
| `Expect(expected, layer)`   | Stop the given layer(s), then assert the number of decisions         |
| `Success`                   | Mark the node's test as passed                                       |
| `Fail(reason)`              | Mark the node's test as failed                                       |

The `Layer` enum controls which layer(s) a lifecycle step targets:

| Variant            | Target               |
| ------------------ | -------------------- |
| `Layer::Consensus` | Consensus layer only |
| `Layer::Execution` | Execution layer only |
| `Layer::Both`      | Both layers          |

Event handlers return a `HandlerResult`:

| Variant                   | Effect                            |
| ------------------------- | --------------------------------- |
| `WaitForNextEvent`        | Keep listening for the next event |
| `ContinueTest`            | Move on to the next step          |
| `SleepAndContinueTest(d)` | Sleep for `d`, then move on       |

## Usage

For runnable examples, see `tests/basic.rs` and `tests/errors.rs` (both use `MockRunner`).

### Reusable scenarios

The `scenarios` module provides pre-built test definitions for common patterns.
Each function returns a `Test` that can be run with any `NodeRunner`:

### Basic test

Spin up four validator nodes and wait for each to reach block height 3:

```rust
use std::time::Duration;
use arc_test_framework::{scenarios, TestParams};

scenarios::validators_reach_height(4, 3)
    .run::<MockRunner>(Duration::from_secs(60))
    .await;
```

### Custom parameters

Override consensus timeouts and set a target block time:

```rust
let params = TestParams {
    consensus_timeout_propose: Duration::from_secs(5),
    consensus_timeout_commit: Duration::from_secs(1),
    target_block_time: Some(Duration::from_millis(500)),
    ..TestParams::default()
};

scenarios::validators_reach_height(4, 3)
    .run_with_params::<MockRunner>(Duration::from_secs(60), params)
    .await;
```

### Crash and restart

Kill a node at height 3, bring it back after 5 seconds, and verify it catches up to height 6:

```rust
use arc_test_framework::Layer;

test.add_node()
    .with_voting_power(40)
    .start()
    .wait_until_block(3)
    .crash(Layer::Both)
    .restart_after(Layer::Both, Duration::from_secs(5))
    .wait_until_block(6)
    .success();
```

### Layer-targeted crash

Crash only the consensus layer at height 3, leaving execution running:

```rust
use arc_test_framework::Layer;

test.add_node()
    .with_voting_power(40)
    .start()
    .wait_until_block(3)
    .crash(Layer::Consensus)
    .success();
```

All lifecycle builder methods (`crash`, `crash_after`, `restart_after`,
`expect_decisions`) take a `Layer` parameter to select which layer(s) to target.

### Event-driven assertions

Use `on_event` to inspect individual events and control when the test proceeds:

```rust
test.add_node()
    .start()
    .on_event(|event, _state| {
        if let ArcEvent::ConsensusDecided { height, .. } = event {
            println!("Block decided at height {height}");
        }
        if let ArcEvent::BlockProduced { number, .. } = event {
            if *number >= Height::new(5) {
                return Ok(HandlerResult::ContinueTest);
            }
        }
        Ok(HandlerResult::WaitForNextEvent)
    })
    .success();
```

### Decision count expectations

Assert that a node participated in at least 10 consensus decisions:

```rust
test.add_node()
    .start()
    .wait_until_block(10)
    .expect_decisions(Expected::AtLeast(10), Layer::Both);
```

### Full nodes (non-validators)

Add a non-validator node (voting power 0) that syncs blocks from the network:

```rust
test.add_node()
    .full_node()     // Sets voting_power = 0
    .start()
    .wait_until_block(5)
    .success();
```

### Delayed start

Start a node 5 seconds after the others to test late-joining behavior:

```rust
test.add_node()
    .start_after(Duration::from_secs(5))
    .wait_until_block(3)
    .success();
```

### Stateful event handlers

Track produced block numbers in a `Vec` and continue once 5 blocks are collected.
The generic parameter `S` on `TestBuilder<S>` provides mutable per-node state
accessible to event handlers:

```rust
let mut test = TestBuilder::<Vec<u64>>::new();

test.add_node()
    .start()
    .on_event(|event, state: &mut Vec<u64>| {
        if let ArcEvent::BlockProduced { number, .. } = event {
            state.push(number.as_u64());
            if state.len() >= 5 {
                return Ok(HandlerResult::ContinueTest);
            }
        }
        Ok(HandlerResult::WaitForNextEvent)
    })
    .success();
```

## `TestParams` Reference

| Field                         | Type               | Default | Description                                          |
| ----------------------------- | ------------------ | ------- | ---------------------------------------------------- |
| `consensus_timeout_propose`   | `Duration`         | `3s`    | Timeout for the propose phase                        |
| `consensus_timeout_prevote`   | `Duration`         | `1s`    | Timeout for the prevote phase                        |
| `consensus_timeout_precommit` | `Duration`         | `1s`    | Timeout for the precommit phase                      |
| `consensus_timeout_commit`    | `Duration`         | `500ms` | Timeout for the commit phase                         |
| `target_block_time`           | `Option<Duration>` | `None`  | Target block time (None = no target)                 |
| `block_gas_limit`             | `Option<u64>`      | `None`  | Block gas limit override (None = chain spec default) |

## `Expected` Reference

| Variant          | Check         |
| ---------------- | ------------- |
| `Exactly(n)`     | `actual == n` |
| `AtLeast(n)`     | `actual >= n` |
| `AtMost(n)`      | `actual <= n` |
| `LessThan(n)`    | `actual < n`  |
| `GreaterThan(n)` | `actual > n`  |

## Logging

The framework initializes `tracing-subscriber` via `init_logging()`, which is called
automatically by `run_test`. Log levels can be tuned with environment variables:

```sh
# Enable debug-level output for the test framework
RUST_LOG=arc_test_framework=debug cargo test ...

# Or use RUST_LOG for fine-grained control
RUST_LOG=arc_test_framework=debug,arc_node_consensus=trace cargo test ... -- --nocapture
```
