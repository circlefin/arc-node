# Contributing to Arc Node

## Working with Protocol Buffers

This project uses Protocol Buffers for consensus and node communication (except consensus-critical serialization). Proto definitions are located in `crates/types/proto` and `crates/remote-signer/proto`. We use [buf](https://buf.build/) to lint, format, and check for breaking changes in our proto files.

> **Prerequisite:** `buf` must be installed before using these targets. See [Prerequisites](README.md#prerequisites) for installation instructions.

### Available Make Targets

- `make buf-lint` - Lint protobuf files to ensure they follow best practices
- `make buf-format` - Format protobuf files (this is included in `make lint`)
- `make buf-breaking` - Check for breaking changes against the master branch

### Before Committing Changes

If you modify any `.proto` files, always run `make buf-lint` and `make buf-breaking` to ensure your changes don't introduce linting issues or breaking changes. The `buf-breaking` command compares your changes against the master branch to detect any backwards-incompatible modifications. Breaking changes should be carefully reviewed and documented as they can impact existing deployments.

### CI

CI action runs the breaking change detection step on every pull request. To skip this step for a specific pull request, you can add the `buf skip breaking` label to the PR. See [Skip breaking change detection using labels](https://buf.build/docs/bsr/ci-cd/github-actions/#skip-breaking-change-detection-using-labels).

Note: `make lint` automatically runs `buf-format`.

### (Optional) Pre-commit hooks

Developers may install [pre-commit](https://pre-commit.com/) hooks, which will handle all the formatting and linting automatically.

```bash
pre-commit install
```
