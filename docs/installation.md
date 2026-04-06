# Installation

Arc node can be installed in two ways: a pre-built binary via `arcup` or building from source.

## Versions

Versions across networks may not be compatible. Consult the table below to confirm which version to run for each network.

| Network | Version |
|---------|---------|
| Arc Testnet | v0.6.0 |

## Pre-built Binary

`arcup` installs `arc-node-execution`, `arc-node-consensus`, and `arc-snapshots` to `~/.arc/bin`.

```sh
curl -L https://raw.githubusercontent.com/circlefin/arc-node/main/arcup/install | bash
```

After installing, restart your shell or run:

```sh
source ~/.arc/env
```

Verify the installation:

```sh
arc-snapshots --version
arc-node-execution --version
arc-node-consensus --version
```

To update in the future, run:

```sh
arcup
```

## Build from Source

**1. Install Rust:**

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**2. Clone the repository:**

```sh
git clone https://github.com/circlefin/arc-node.git
cd arc-node
```

**3. Build and install:**

```sh
cargo install --path crates/node --root /usr/local
cargo install --path crates/malachite-app --root /usr/local
cargo install --path crates/snapshots --root /usr/local
```

Verify:

```sh
arc-snapshots --version
arc-node-execution --version
arc-node-consensus --version
```

See [Running an Arc Node](./running-an-arc-node.md) for how to run the node after installation.
