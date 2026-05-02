# Execution E2E Tests

Rust e2e tests for the execution layer using `ArcTestBuilder` with mock consensus (`reth-engine-local`). Tests run a real Reth node without requiring a network.

## Quick Reference

Test pattern:

```rust
ArcTestBuilder::new()
    .with_setup(ArcSetup::new())     // configure node + wallet
    .with_action(...)                 // add sequential actions
    .run().await                      // execute all actions
```

Hardfork testing: use `localdev_with_hardforks()` for custom activation blocks.
Default `ArcSetup::new()` activates ALL hardforks at block 0.

## Running Tests

```
cargo nextest run --locked -p arc-execution-e2e
```

This crate has an `integration` feature for tests requiring additional infrastructure, but standard e2e tests run without it.

## Full Guidance

See `.claude/skills/arc-rust-e2e-tests/SKILL.md` for:
- Complete action catalog (10 actions)
- Hardfork gating patterns (4 patterns for Zero3–Zero6)
- Chainspec helpers
- Worked examples

## Key Files

- `src/lib.rs` — ArcTestBuilder, re-exports
- `src/action.rs` — Action trait
- `src/setup.rs` — ArcSetup (node initialization)
- `src/chainspec.rs` — chainspec helper re-exports
- `src/actions/` — all available actions
- `tests/` — test files (13 scenarios)
