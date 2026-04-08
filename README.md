<p align="center">
  <a href="https://www.arc.network/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/assets/arc-logo-light.svg">
      <img alt="Arc" src="docs/assets/arc-logo-dark.svg" width="auto" height="120">
    </picture>
  </a>
</p>

<p align="center">The Economic OS for the internet</p>

<p align="center">
  <a href="https://www.arc.network/"><img src="https://img.shields.io/badge/Website-arc.network-blue" alt="Website"></a>
</p>

> [!IMPORTANT]
> Arc is currently in testnet, and this is alpha software currently undergoing audits.

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

## Run a Node

See [Installation — Build from Source](docs/installation.md#build-from-source) for how to build and install arc-node binaries, and [Running an Arc Node](docs/running-an-arc-node.md) for configuration and startup.

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

## Acknowledgements

arc-node is open-source software, licensed under Apache 2.0, built from a number of open source libraries, and inspired by others. We would like to highlight several of them in particular and credit the teams that develop and maintain them.

[Malachite](https://github.com/circlefin/malachite) -  Malachite, a flexible BFT consensus engine written in Rust, was originally developed at [Informal Systems](https://github.com/informalsystems) and [is now maintained by Circle](https://www.circle.com/blog/introducing-arc-an-open-layer-1-blockchain-purpose-built-for-stablecoin-finance) as part of Arc. We thank Informal Systems for originating and stewarding Malachite, and their continued contributions to the project.  

[Reth / Paradigm](https://github.com/paradigmxyz/reth) - Reth is an EVM execution client that is used in Arc's execution layer via Reth SDK. We thank the Paradigm team for continuing to push the envelope with Reth and their continued emphasis on performance, extensibility, and customization, as well as their commitment to open source. Additionally, we're big fans of the Foundry toolchain as well! 

[libp2p](https://github.com/libp2p/rust-libp2p) - libp2p is used extensively through the arc-node consensus layer, and we thank the team for their development of it.

[Tokio](https://github.com/tokio-rs/tokio) - Tokio is used extensively throughout the consensus and execution layers, and we are grateful to  the team for their continued development and maintenance of it.

[Celo](https://celo.org/) -  USDC is the native token on Arc and supports interacting with it through an ERC-20 interface; this "linked interface" design was first (as far as we know) pioneered on Celo, and we'd like to credit the team for devising it.

[Alloy-rs](https://github.com/alloy-rs/alloy) - Alloy is used throughout the consensus and execution layers, and we are very thankful  to the team for this excellent library.

[Revm](https://github.com/bluealloy/revm) - Revm is used via the Reth SDK in the execution layer as the core EVM implementation. We thank the team for their continued development and maintenance of it.

[Hardhat / Nomic Foundation](https://github.com/NomicFoundation/hardhat) - we thank the team for their continued development of the Hardhat toolchain. 

[Viem](https://github.com/wevm/viem) - we thank the team for their continued development of Viem and other libraries.
