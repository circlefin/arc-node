# arc-evm-specs-tests

`arc-evm-specs-tests` is a fixture-consumer binary for `arc-execution-specs`.

Its purpose is:
- execute Ethereum state-test fixtures through the ARC EVM execution path
- expose that execution via a CLI binary that `arc-execution-specs` can call through `consume direct`

## How It Simulates ARC EVM Execution

`arc-evm-specs-tests` does not implement a second EVM. It is a harness that drives the existing ARC execution layer from `arc-evm` (`crates/evm`).

The runner implementation and design are sourced from upstream `revm`:

- source: <https://github.com/bluealloy/revm>
- statetest runner shape: [`revme/src/cmd/statetest/runner.rs`](https://github.com/bluealloy/revm/blob/main/bins/revme/src/cmd/statetest/runner.rs)
- root helper shape: [`revme/src/cmd/statetest/merkle_trie.rs`](https://github.com/bluealloy/revm/blob/main/bins/revme/src/cmd/statetest/merkle_trie.rs)

Execution flow:
1. `arc-evm-specs-tests` command entrypoint loads fixtures (`statetest` command).
2. It builds an `ArcEvmFactory` from `arc-evm`.
3. For each fixture test case, it builds env/tx input and calls `factory.create_evm(...)`.
4. It executes with `evm.transact_commit(tx)`.
5. It validates logs hash and state root against fixture expectations and returns normalized error classes/kinds.

That means the transaction execution engine is ARC EVM code (from `crates/evm/src/evm.rs`), while `arc-evm-specs-tests` is the harness around it.

## Execution Mode

Current adapter setup uses `LOCAL_DEV`, which means ARC localdev executes with the ARC hardforks active in that chain spec.

The fixture fork name still selects the Ethereum `cfg.spec`, but the underlying executor is ARC localdev.

This is an ARC-mode harness, not a pure Ethereum fork-isolated runner:
- the fixture fork still chooses the REVM `cfg.spec`
- the chain-level execution context is still ARC `LOCAL_DEV`
- ARC localdev behavior and ARC hardfork activation can therefore influence results even when the fixture fork is pre-ARC or pre-feature

Interpretation rule:
- use this runner to measure how ARC localdev behaves when driven by Ethereum statetest fixtures
- do not interpret the results as “stock Ethereum execution for that fixture fork in isolation”

If you need pure Ethereum fork-isolated validation, that should be a separate runner mode with a different chain-spec contract.


## Temporary Upstream Workarounds

This crate currently uses a two-pass fixture parse because `revm-statetest-types` does not yet expose all fields needed by ARC's harness flow.

- Verified against: `revm-statetest-types = 14.x`
- Workaround details:
  - extract `config.chainid` from raw JSON before typed deserialization
  - sanitize unsupported fields (`receipt`, `state` -> `postState`) before deserializing into `TestSuite`
- Removal condition: delete this workaround once upstream typed deserialization includes `config.chainid` and fixture fields needed for direct `TestSuite` parsing.

## How `arc-execution-specs` Consumes It

`arc-execution-specs` runs this binary through:

```bash
uv --project packages/testing run --python 3.12 consume direct \
  --bin $HOME/crcl/arc-node-project-name/target/release/arc-evm-specs-tests \
  --input $HOME/crcl/arc-execution-specs/fixtures/state_tests/for_prague \
  -m state_test -q
```

Integration contract:
- producer side (arc-node repo): build `arc-evm-specs-tests` binary and keep Rust health (`cargo test`, `cargo build`)
- consumer side (`arc-execution-specs`): drive fixtures, collect pass/fail output, and report compatibility metrics

## Build

```bash
cd $HOME/crcl/arc-node-project-name
cargo build --release -p arc-evm-specs-tests
```

Binary:

```text
$HOME/crcl/arc-node-project-name/target/release/arc-evm-specs-tests
```

## Consume Prague State Tests

```bash
cd $HOME/crcl/arc-execution-specs
uv --project packages/testing run --python 3.12 consume direct \
  --bin $HOME/crcl/arc-node-project-name/target/release/arc-evm-specs-tests \
  --input $HOME/crcl/arc-execution-specs/fixtures/state_tests/for_prague \
  -m state_test -q
```

## Focused Retest Examples

`extcodehash_via_call`:

```bash
cd $HOME/crcl/arc-execution-specs
uv --project packages/testing run --python 3.12 consume direct \
  --bin $HOME/crcl/arc-node-project-name/target/release/arc-evm-specs-tests \
  --input $HOME/crcl/arc-execution-specs/fixtures/state_tests/for_prague \
  -m state_test -k "test_extcodehash_via_call and fork_Prague" --maxfail=1 -ra
```

`precompile_absence`:

```bash
cd $HOME/crcl/arc-execution-specs
uv --project packages/testing run --python 3.12 consume direct \
  --bin $HOME/crcl/arc-node-project-name/target/release/arc-evm-specs-tests \
  --input $HOME/crcl/arc-execution-specs/fixtures/state_tests/for_prague \
  -m state_test -k "test_precompile_absence and fork_Prague" --maxfail=1 -ra
```

## Error Classes in Adaptor Output

`arc-evm-specs-tests` emits normalized tags in failure details:

- `error_class=HARNESS_PRECONDITION`
- `error_class=EXECUTION_MISMATCH`
- `error_kind=TX_ENV_BUILD_FAILED`
- `error_kind=UNEXPECTED_EXCEPTION`
- `error_kind=UNEXPECTED_SUCCESS`
- `error_kind=WRONG_EXCEPTION`
- `error_kind=LOGS_HASH_MISMATCH`
- `error_kind=STATE_ROOT_MISMATCH`

This is intended to be stable and reusable across suites, rather than requiring per-test string mapping.

## Output Metadata

Consume-direct JSON output includes structured variant metadata:

- `data_index`: index into fixture `transaction.data`
- `gas_index`: index into fixture `transaction.gasLimit`
- `value_index`: index into fixture `transaction.value`

The `variantId` string is still emitted for compatibility with existing
consume-direct tooling. Its suffix remains in the historical `d..._g..._v...`
format even though the structured JSON fields use the clearer names above.

## Reporting Artifacts

`consume direct` exposes multiple reporting layers, and they operate at
different granularities:

- `report_consume.html` is the pytest HTML report. Its primary unit is the
  collected pytest item, which is suite-level from the fixture consumer's
  point of view.
- the aggregate JSON written to stdout by `arc-evm-specs-tests` is the
  fixture-consumer contract consumed by `arc-execution-specs`; it summarizes
  each executed result as `name` / `variantId` / `pass` / `error`, with the
  optional JSON outcome attached alongside it.
- `per_test_outcomes.jsonl` is the ARC runner's variant-level artifact. Each
  line is one concrete fixture variant emitted through `--json-outcome`.

That means the HTML report is useful for high-level triage, but it does not
model fixture-internal transaction variants as first-class rows. The variant
detail lives in the runner output artifacts.

Practical interpretation:

- use `report_consume.html` to answer "which pytest cases failed?"
- use the aggregate JSON output to answer "what did the fixture consumer
  return for this run as a whole?"
- use `per_test_outcomes.jsonl` to answer "which exact fixture variant failed,
  and what were its state root, logs root, gas used, and normalized error?"

The stable link between those two views is the runner's formatted test ID:

- pytest failure details surface the full variant identifier
- per-test JSON lines emit the same identifier in the `test` field
- `variantId` carries the same concrete variant identity in aggregate
  consume-direct JSON output

This is why `format_test_id(...)` intentionally remains stable even though the
structured metadata fields were renamed to `data_index`, `gas_index`, and
`value_index`: the reporting contract depends on a stable concrete variant key.
