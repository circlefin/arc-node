# Install

The Arc node binaries can be installed by [building from source](#build-from-source).

After the installation, refer to [Running an Arc Node](./running-an-arc-node.md)
for how to run an Arc node.

> **Pre-built binaries** and **Docker images** are coming soon.

## Versions

Versions of the Arc node across networks may not be compatible.
Consult the table below to confirm which version to run for each network.

| Network     | Version |
|-------------|---------|
| Arc Testnet | v0.6.0  |

## Build from Source

The Arc node source code is available in the
https://github.com/circlefin/arc-node repository:

**1. Clone `arc-node`**

```sh
git clone https://github.com/circlefin/arc-node.git
cd arc-node
git checkout $VERSION
git submodule update --init --recursive
```

`$VERSION` is a tag for a released version.
Refer to the [Versions](#versions) section to find out which one to use.

**2. Install Rust:**

Make sure that you have [rust](https://rust-lang.org/tools/install/) installed.
If not, it can be installed with the following commands:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

**3. Build and install:**

The following commands produce three Arc node binaries: 
`arc-node-execution`, `arc-node-consensus`, and `arc-snapshots`:

```sh
cargo install --path crates/node
cargo install --path crates/malachite-app
cargo install --path crates/snapshots
```

`cargo install` places compiled binaries into `~/.cargo/bin`, which is added
to `PATH` by loading `~/.cargo/env`.
Include the parameter `--root $BASE_DIR` to install the compiled binaries into
`$BASE_DIR/bin` instead (for instance, `--root /usr/local`).

In either case, Arc node binaries should be in the `PATH`.
Verify by calling them:

```sh
arc-snapshots --version
arc-node-execution --version
arc-node-consensus --version
```

