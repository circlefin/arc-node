# Integration Tests

Integration test runner for Arc nodes. Provides
[`ArcNodeRunner`](src/runner.rs), a
[`NodeRunner`](../framework/src/lib.rs) implementation that spawns real
in-process Arc nodes and runs shared test
[scenarios](../framework/src/scenarios.rs) against them.

This crate exists separately from `arc-test-framework` so that the framework
(builder API, step sequencer, scenarios, mock runner) compiles without pulling
in the full node dependency tree.

## What it exercises

Each spawned node consists of:

- **Execution layer** — Reth `ArcNode` with IPC sockets in a per-node temp dir
- **Consensus layer** — Malachite `App` connecting to execution via Engine API (IPC)
- **Event bridge** — background task mapping `TxEvent<ArcContext>` → `ArcEvent`
- **P2P networking** — real libp2p peers on `127.0.0.1`

## Port allocation

Each node N in test T gets:

| Port | Formula |
|------|---------|
| Consensus P2P | `26000 + T × 100 + N × 10` |
| Consensus RPC | `31000 + T × 100 + N × 10` |
| Reth ports | default + `T × 100 + N × 10` offset |
| Execution IPC | `<temp_dir>/reth.ipc`, `<temp_dir>/auth.ipc` |

## Running

```sh
# All integration tests
cargo nextest run -p arc-test-integration

# Single test
cargo nextest run -p arc-test-integration -- validators_reach_height_3

# With verbose logging
RUST_LOG=arc_test_framework=debug,arc_node_consensus=info \
  cargo nextest run -p arc-test-integration --no-capture
```

## Test scenarios

Tests reuse shared scenarios from `arc_test_framework::scenarios`. Each
scenario defines _what_ happens (nodes, steps, assertions); this crate only
decides _how_ — by wiring `ArcNodeRunner` with appropriate timeouts.

The `LOCAL_DEV` genesis has 5 validators with 20 VP each (100 total), so BFT
quorum requires 67 VP. Every test **must spawn exactly 5 validator nodes**
because the registry is pre-seeded with BIP39 children 2..=6 at genesis; any
unfilled slot is treated as an offline validator and stalls rounds where it is
the proposer. Full (non-validating) nodes may be added on top; they are not
in the registry.

Example scenarios:
- `validators_reach_height_3`: all validators finalize 3 blocks
- `crash_and_restart`: 5 nodes; first crashes at height 3, restarts after 1 s, reaches height 10
- `validators_and_full_nodes_reach_height_3`: validators reach height 5, 2 full nodes reach height 3
- `delayed_start`: 5 nodes; last joins 3 s late, reaches height 10
- `wait_until_decision`: validators wait for decision at height 3
- `expect_at_least_decisions`: validators; assert ≥ 3 decisions by height 3

## Adding a new integration test

1. If the scenario is reusable, add a function to
   [`arc_test_framework::scenarios`](../framework/src/scenarios.rs).
2. Add a test in [`tests/basic.rs`](tests/basic.rs) that calls the
   scenario with `ArcNodeRunner` and an appropriate timeout.
3. Add a matching mock test in
   [`arc-test-framework/tests/basic.rs`](../framework/tests/basic.rs) for fast
   CI feedback.
