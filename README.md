# Arc crates.io placeholders

This orphan branch contains placeholder crates for Arc package names whose real
implementations are currently not publishable to crates.io.

The real Arc node source is developed in the public repository:

https://github.com/circlefin/arc-node

These placeholder releases are not functional libraries. They reserve crate
names for the Arc project while the real implementation crates continue to
depend on Reth SDK crates that are not available from crates.io at compatible
versions.

Do not depend on `0.0.0-placeholder` releases for runtime functionality.

## Placeholder Packages

- `arc-eth-engine`
- `arc-evm`
- `arc-evm-node`
- `arc-execution-config`
- `arc-execution-payload`
- `arc-execution-txpool`
- `arc-execution-validation`
- `arc-node-consensus`
- `arc-node-execution`
- `arc-precompiles`
