# Install

The Arc node can be installed in three ways:
downloading pre-built binaries via [`arcup`](#pre-built-binary),
[building from source](#build-from-source),
or using [Docker](#docker) images.

After installation, refer to [Running an Arc Node](./running-an-arc-node.md)
for how to start the node (binaries or Docker Compose).

## Versions

Versions of the Arc node across networks may not be compatible.
Consult the table below to confirm which version to run for each network.

| Network     | Version |
|-------------|---------|
| Arc Testnet | v0.6.0  |

## Pre-built Binary

This repository includes `arcup`, a script that installs Arc node binaries
into `$ARC_BIN_DIR`, defaulting to `$ARC_HOME/bin` and then `~/.arc/bin`.
The pre-built archives include `arc-node-execution`, `arc-node-consensus`,
and `arc-snapshots`.

Supported release targets:

| Operating system | CPU architecture | Release target |
|------------------|------------------|----------------|
| Linux | x86_64 / amd64 | `x86_64-unknown-linux-gnu` |
| Linux | arm64 / aarch64 | `aarch64-unknown-linux-gnu` |
| macOS | Apple silicon | `aarch64-apple-darwin` |

Linux pre-built binaries are dynamically linked against GNU libc. The current
release artifacts require glibc 2.39 or newer; Ubuntu 24.04 is known to work.
Older distributions, such as Debian 12/bookworm, may fail with errors like
`GLIBC_2.38 not found` or `GLIBC_2.39 not found`. In that case, use a newer
Linux distribution/container, Docker images, or build from source.

Install the latest release:

```sh
curl -L https://raw.githubusercontent.com/circlefin/arc-node/main/arcup/install | bash
```

More precisely, the [configured paths](./running-an-arc-node.md#configure-paths)
for Arc nodes are based on the `$ARC_HOME` variable, with `~/.arc` as default value.
If `$ARC_BIN_DIR` is not set, its default value is `$ARC_HOME/bin`, defaulting
to `~/.arc/bin`.
`$ARC_BIN_DIR` must be part of the system `PATH`.

To be sure that the binaries installed under `$ARC_BIN_DIR` are available in
the `PATH`, load the produced environment file:

```sh
export ARC_HOME="${ARC_HOME:-$HOME/.arc}"
source "$ARC_HOME/env"
```

To install a specific version, run `arcup` with `--install`:

```sh
arcup --install v0.6.0
```

Next, verify that the three Arc binaries are installed:

```sh
arc-snapshots --version
arc-node-execution --version
arc-node-consensus --version
```

The `arcup` script should also be in the `PATH`
and can be used to update Arc binaries:

```sh
arcup
```

To update the installer script itself:

```sh
arcup --self-update
```

To remove `arcup`, the installed Arc binaries, and the generated shell env
files without deleting node data:

```sh
arcup --uninstall
```

`arcup` always verifies the downloaded archive against its `.sha256` file.
GPG signature verification is disabled until the Arc release signing key is
published. If an archive for your operating system or CPU architecture is not
available in the selected release, `arcup` prints the expected asset name and
the available release assets.

Troubleshooting:

- `Unsupported architecture`: use one of the supported targets above, or build
  from source.
- `Failed to download ...`: confirm the selected version exists and includes
  an archive for your platform.
- `Checksum verification failed`: retry the install; if it persists, do not run
  the downloaded binaries.
- On macOS, if manually copied binaries are blocked by quarantine attributes,
  run `xattr -dr com.apple.quarantine "$ARC_BIN_DIR"` after reviewing the
  downloaded files.

## Build from Source

The Arc node source code is available in the
https://github.com/circlefin/arc-node repository:

**1. Clone `arc-node`**

```sh
git clone https://github.com/circlefin/arc-node.git
cd arc-node
git checkout $VERSION
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

With Rust installed, install the dependencies for your operating system:

- **Ubuntu:** `sudo apt-get install libclang-dev pkg-config build-essential`
- **macOS:** `brew install llvm pkg-config` and then
  `export LIBCLANG_PATH="$(brew --prefix llvm)/lib"`
- **Windows:** `choco install llvm` or `winget install LLVM.LLVM`

These are needed to build bindings for Arc node execution's database.

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

## Docker

Running an Arc node requires two Docker images — one for each layer:

| Image | Description |
|-------|-------------|
| `arc-execution` | Execution Layer (EL) — EVM, RPC, transaction pool |
| `arc-consensus` | Consensus Layer (CL) — BFT consensus, follow mode |

You can either pull pre-built images from the public registry or build them
from source. Both approaches are described below.

### Pre-built Docker images

Pre-built multi-arch images (amd64 and arm64) are published to
[Cloudsmith](https://cloudsmith.io/~circle/repos/arc-network/packages/).

Set `$ARC_VERSION` to the release from the [Versions](#versions) table,
then pull:

```sh
docker pull docker.cloudsmith.io/circle/arc-network/arc-execution:$ARC_VERSION
docker pull docker.cloudsmith.io/circle/arc-network/arc-consensus:$ARC_VERSION
```

### Build Docker images

Alternatively, build images from a release tag:

```sh
git clone https://github.com/circlefin/arc-node.git && cd arc-node
git checkout v$ARC_VERSION
docker buildx bake \
  --set "*.args.GIT_COMMIT_HASH=$(git rev-parse v$ARC_VERSION^{commit})" \
  --set "*.args.GIT_VERSION=v$ARC_VERSION" \
  --set "*.args.GIT_SHORT_HASH=$(git rev-parse --short v$ARC_VERSION^{commit})" \
  --set "arc-execution.tags=arc-execution:$ARC_VERSION" \
  --set "arc-consensus.tags=arc-consensus:$ARC_VERSION"
```

After obtaining the images, see
[Running an Arc Node: Docker](./running-an-arc-node.md#docker)
for how to start the node.
