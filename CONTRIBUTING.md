# Contributing to Arc Node

First, thank you for your interest in improving Arc Node!

There are multiple opportunities to contribute at any level. It doesn't matter if you are just getting started with Rust or are the most weathered expert, we can use your help.

No contribution is too small and all contributions are valued.

This document will help you get started. Do not let the document intimidate you. It should be considered as a guide to help you navigate the process.

If you contribute to this project, your contributions will be made to the project under Apache 2.0 license.

## Code of Conduct

The Arc Node project adheres to the [Rust Code of Conduct][rust-coc]. This code of conduct describes the minimum behavior expected from all contributors.

## Ways to contribute

There are three ways you can contribute to Arc Node:

1. **By opening an issue:** For example, if you believe that you have uncovered a bug
   in Arc Node, creating a [new issue][new-issue] in the issue tracker is the way to report it.
2. **By adding context:** Providing additional context to [existing issues][existing-issues],
   such as screenshots and code snippets to help resolve issues.
3. **By resolving issues:** Typically this is done in the form of either
   demonstrating that the issue reported is not a problem after all, or more often,
   by opening a pull request that fixes the underlying problem, in a concrete and
   reviewable manner.

> [!IMPORTANT]
> Please see the [README](./README.md) for how to set up your environment, build Arc Node, and run the test suite.

### Scope of Contributions

At this time, we will not be accepting contributions that only fix spelling or grammatical errors in documentation, code or elsewhere.

### Policy on Unsolicited Contributions

We do not accept unsolicited contributions. The following types of PRs will be closed immediately:

- PRs submitted without prior issue assignment or maintainer approval
- PRs that only fix typos, formatting, or make superficial "improvements"
- New documentation or features that were not requested
- Refactoring or "code quality improvements" that were not discussed beforehand

Repeat offenders may be blocked from the repository.

**How to contribute properly:**
1. Find an existing issue you'd like to work on, or open a new issue describing your proposed change
2. Comment on the issue requesting assignment and wait for maintainer approval
3. Only submit a PR after you have been assigned to the issue

### Pull Request Requirements

Pull requests will only be accepted if they meet **ALL** of the following criteria:

1. The submitter must be a core contributor to Arc Node
    * OR the submitter must have been explicitly assigned to the issue that the PR addresses
2. The PR must address an existing issue in our issue tracker
3. The PR description must clearly reference the issue number it resolves (for example `Closes: #XXX`) and explain how it resolves the issue
4. The PR must comply with all other contribution standards (code style, testing requirements, etc.)

**Pull requests that do not meet these requirements will be closed without review.**

If you are interested in contributing but are not a core contributor, please comment on an existing issue to request assignment before submitting a PR.

### Getting Help

If you have reviewed existing documentation and still have questions, or you
are having problems, you can get help by [opening an issue][new-issue].

## Working with Protocol Buffers

This project uses Protocol Buffers for consensus and node communication (except consensus-critical serialization). Proto definitions are located in `crates/types/proto` and `crates/remote-signer/proto`. We use [buf](https://buf.build/) to lint, format, and check for breaking changes in our proto files.

> **Prerequisite:** `buf` must be installed before using these targets. See [Prerequisites](README.md#prerequisites) for installation instructions.

### Available Make Targets

- `make buf-lint` - Lint protobuf files to ensure they follow best practices
- `make buf-format` - Format protobuf files (this is included in `make lint`)
- `make buf-breaking` - Check for breaking changes against the main branch

### Before Committing Changes

If you modify any `.proto` files, always run `make buf-lint` and `make buf-breaking` to ensure your changes don't introduce linting issues or breaking changes. The `buf-breaking` command compares your changes against the main branch to detect any backwards-incompatible modifications. Breaking changes should be carefully reviewed and documented as they can impact existing deployments.

### CI

CI action runs the breaking change detection step on every pull request. To skip this step for a specific pull request, you can add the `buf skip breaking` label to the PR. See [Skip breaking change detection using labels](https://buf.build/docs/bsr/ci-cd/github-actions/#skip-breaking-change-detection-using-labels).

Note: `make lint` automatically runs `buf-format`.

### (Optional) Pre-commit hooks

Developers may install [pre-commit](https://pre-commit.com/) hooks, which will handle all the formatting and linting automatically.

```bash
pre-commit install
```

[rust-coc]: https://rust-lang.org/policies/code-of-conduct/
[new-issue]: https://github.com/circlefin/arc-node/issues/new
[existing-issues]: https://github.com/circlefin/arc-node/issues
