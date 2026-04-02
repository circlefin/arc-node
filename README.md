# Arc Node

> The Economic OS for the internet

[![Website](https://img.shields.io/badge/Website-arc.network-blue)](https://www.arc.network/)

An open Layer-1 blockchain purpose-built to unite programmable money and onchain innovation with real-world economic activity. Built on [Reth](https://github.com/paradigmxyz/reth) with [Malachite](https://github.com/circlefin/malachite) consensus, Arc delivers the performance, reliability, and liquidity needed to meet the demands of the global internet economy.

Arc is engineered for mass adoption with stablecoins as native gas, opt-in configurable privacy, and deterministic sub-second finality — making it uniquely suited for use cases like onchain credit, capital markets, stablecoin FX, and cross-border payments.

## Features

- **Stablecoins as Native Gas** - Low, predictable, fiat-based gas fees starting with USDC, enabling seamless transactions whether sending $1 or $1B
- **Opt-in Configurable Privacy** - Native privacy tooling enables selective shielding of sensitive financial data while preserving auditability
- **Deterministic Sub-second Finality** - Instant settlement finality powered by Malachite BFT consensus engine, meeting institutional regulatory standards
- **EVM Compatible** - Full Ethereum compatibility with custom precompiles for native coin operations, post-quantum signatures, and system accounting
- **Advanced Transaction Pool** - Configurable pool with custom validation, transaction denylist, and enterprise-grade reliability
- **Production-Ready Infrastructure** - Built on Reth's proven codebase with comprehensive testing, monitoring, and modular architecture

## Documentation

- 🚀 **[Execution](crates/node/README.md)** - Execution binary and configuration
- 🗳️ **[Consensus](crates/malachite-app/README.md)** - Consensus binary and configuration
- 📊 **[Profiling](docs/PROFILING.md)** - CPU and heap profiling with pprof

## Development

### Repository setup

Clone the repository (or pull the latest changes). This repository uses Git submodules for Foundry dependencies; initialize and update them with:

```bash
git submodule update --init --recursive
```

> **Tip:** To automatically fetch submodules on `git pull`, run in the repo root:
> ```bash
> git config submodule.recurse true
> git config fetch.recurseSubmodules on-demand
> ```

Note: USDC contract artifacts are committed to `assets/artifacts/stablecoin-contracts/` and do not require a separate build step.


### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (version pinned via `rust-toolchain.toml`)
- [Docker](https://docs.docker.com/get-docker/) (required for `make testnet`)
- [Node.js](https://nodejs.org/)
- [Foundry](https://getfoundry.sh/)
- [Hardhat](https://hardhat.org/)
- [Protobuf](https://github.com/protocolbuffers/protobuf)
- [TypeScript](https://www.typescriptlang.org/)
- [Yarn](https://yarnpkg.com/)
- [Buf](https://github.com/bufbuild/buf)

Install required tools on MacOS with Homebrew:

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

brew install protobuf node yarn bufbuild/buf/buf

curl -L https://foundry.paradigm.xyz | bash
foundryup
```

> **Note:** Hardhat only supports **even** Node.js versions (e.g., 20.x, 22.x). Odd versions like 25.x are not supported. See [Hardhat's Node.js support policy](https://v2.hardhat.org/hardhat-runner/docs/reference/stability-guarantees#node.js-versions-support) for details.

Install JavaScript dependencies:

```bash
npm install
```

### Build

Build the project:

```bash
make build
```

### Code Quality

Format and lint your code:

```bash
make lint
```

### Testing

The test suite includes unit tests, integration tests, contract tests, and smoke tests.

Run tests:

```bash
# Unit tests (Rust + linting)
make test-unit

# Integration tests
make test-it

# Contract tests (Solidity)
make test-unit-contract

# Smoke tests (end-to-end validation)
make smoke

# Run all tests
make test-all
```

### Coverage

Generate and view test coverage (requires [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov?tab=readme-ov-file#installation)):

```bash
# Install cargo-llvm-cov on MacOS with Homebrew (one-time setup)
brew install cargo-llvm-cov

# Generate coverage for unit tests
make cov-unit

# Generate HTML report and open in browser
make cov-show
```

### Local testnet

> **Note:** This refers to private, ephemeral testnets for local development,
> not the public Arc [Testnet](https://docs.arc.network/arc/tutorials/deploy-on-arc).
> See [Quake's README](crates/quake/README.md) for more details.

Launch a full local testnet with 5 execution nodes, 5 consensus nodes, plus Prometheus, Grafana, and Blockscout:

```bash
make testnet
```

> [!NOTE]
> If your development environment requires installing custom CA certificates, you can add them to the `deployments/certs` directory. They must be PEM-encoded and have a `.crt` extension. They will be automatically installed into the Docker images at build time.
>
> To export a certificate from your system's keychain (macOS):
> ```bash
> security find-certificate -p -c '<cert name>' > deployments/certs/<cert name>.crt
> ```

Interact with the testnet:

```bash
# Send transaction load
make testnet-load

# Stop the testnet
make testnet-down

# Clean up all resources
make testnet-clean
```

For an in-depth look at system design and individual components, check out the [Architecture Guide](docs/ARCHITECTURE.md). For architectural decisions and their rationale, refer to our [Architecture Decision Records (ADRs)](docs/adr/README.md).

## Contributing

We welcome contributions! Please follow these steps:

1. **Format and lint**: `make lint`
2. **Build**: `make build`
3. **Test**: `make test-unit`
4. **Check coverage**: `make cov-show`

For more details, see our [Contributing Guide](CONTRIBUTING.md).

## Resources

- [Arc Network](https://www.arc.network/) - Official Arc Network website
- [Arc Documentation](https://www.arc.network/) - Official Arc developer documentation
- [Reth](https://github.com/paradigmxyz/reth) - The underlying execution layer framework
- [Malachite](https://github.com/circlefin/malachite) - BFT consensus engine
- [Local Documentation](docs/) - Implementation guides and references
