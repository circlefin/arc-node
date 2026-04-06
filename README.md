# Arc Node

> [!IMPORTANT]
> Arc is currently in testnet, and this is alpha software currently undergoing audits.

The Economic OS for the internet

[![Website](https://img.shields.io/badge/Website-arc.network-blue)](https://www.arc.network/)

Arc is an open EVM-compatible layer 1 built on [Malachite](https://github.com/circlefin/malachite) consensus, delivering the performance and reliability needed to meet the new demands of the global internet economy. 

## Features

- **USDC as Gas** - Pay gas in USDC for low, predictable fees on any transaction  
- **Deterministic Sub-second Finality** - Near-instant settlement finality powered by Malachite BFT consensus engine  
- **Circle Platform Integration** - Integrates with Circle’s full-stack platform (e.g., USDC, Wallets, CCTP, Gateway) to help you go from prototype to production faster  
- **(Coming soon) Opt-in Configurable Privacy** - Native privacy tooling enables selective shielding of sensitive financial data while preserving auditability

## Documentation

- 🚀 **[Execution](crates/node/README.md)** - Execution binary and configuration
- 🗳️ **[Consensus](crates/malachite-app/README.md)** - Consensus binary and configuration
- More: see Arc [developer docs](https://docs.arc.network/arc/concepts/welcome-to-arc) for guides, APIs, and specs

## Development

### Repository setup

Clone the repository (or pull the latest changes). This repository uses Git submodules; initialize and update them with:

```bash
git submodule update --init --recursive
```

**Tip:** To automatically fetch submodules on `git pull`, run in the repo root:

```bash
git config submodule.recurse true
git config fetch.recurseSubmodules on-demand
```

### Prerequisites

- [Node.js](https://nodejs.org/)
- [Foundry](https://getfoundry.sh/)
- [Hardhat](https://hardhat.org/)
- [Protobuf](https://github.com/protocolbuffers/protobuf)
- [TypeScript](https://www.typescriptlang.org/)
- [Yarn](https://yarnpkg.com/)
- [Buf](https://github.com/bufbuild/buf)

Install required tools on MacOS with Homebrew:

```bash
brew install protobuf node yarn bufbuild/buf/buf

curl -L https://foundry.paradigm.xyz | bash
foundryup
```

**Note:** Hardhat only supports **even** Node.js versions (e.g., 20.x, 22.x). Odd versions like 25.x are not supported. See [Hardhat's Node.js support policy](https://v2.hardhat.org/hardhat-runner/docs/reference/stability-guarantees#node.js-versions-support) for details.

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

### Local Testnet

Launch a full local testnet with 5 execution nodes, 5 consensus nodes, plus Prometheus, Grafana, and Blockscout:

```bash
make testnet
```

**Note:** If your development environment requires installing custom CA certificates, you can add them to the `deployments/certs` directory. They must be PEM-encoded and have a `.crt` extension. They will be automatically installed into the Docker images at build time.

To export a certificate from your system's keychain (macOS):

```bash
security find-certificate -p -c '<cert name>' > deployments/certs/<cert name>.crt
```

Interact with the testnet:

```bash
# Spam transactions
make testnet-spam

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
