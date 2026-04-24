# Running an Arc Node with Docker

As an alternative to [building from source](running-an-arc-node.md), you can
run an Arc node using Docker containers. The setup uses the same **follow mode**
as the binary guide, with IPC between the execution and consensus containers.

## Prerequisites

- [Docker Engine](https://docs.docker.com/engine/install/) 24+ with BuildKit
- [Docker Compose](https://docs.docker.com/compose/install/) v2
- Meets the [system requirements](running-an-arc-node.md#system-requirements)

## Docker images

Running an Arc node requires two Docker images — one for each layer:

| Image | Description |
|-------|-------------|
| `arc-execution` | Execution Layer (EL) — EVM, RPC, transaction pool |
| `arc-consensus` | Consensus Layer (CL) — BFT consensus, follow mode |

You can either pull pre-built images from the public registry or build them
from source. Both approaches are described below.

Throughout this guide, the compose file reads images from two environment
variables. Set the version once and export both before running any
`docker compose` command:

```sh
export ARC_VERSION=0.6.0
export ARC_HOME=~/.arc
```

### Public Docker images

Pre-built multi-arch images (amd64 and arm64) are published to
[Cloudsmith](https://cloudsmith.io/~circle/repos/arc-network/packages/).

Optionally, you can pull the images from the public repository. This step can
be skipped, as the images will be pulled automatically by `docker compose`.

```sh
docker pull docker.cloudsmith.io/circle/arc-network/arc-execution:$ARC_VERSION
docker pull docker.cloudsmith.io/circle/arc-network/arc-consensus:$ARC_VERSION
```

Export the aliases `docker compose` is expecting for the Docker images.

```sh
export ARC_EXECUTION_IMAGE=docker.cloudsmith.io/circle/arc-network/arc-execution:$ARC_VERSION
export ARC_CONSENSUS_IMAGE=docker.cloudsmith.io/circle/arc-network/arc-consensus:$ARC_VERSION
```

### Build images

Alternatively, build images from a release tag or a commit hash:

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

Then export the local image tags:

```sh
export ARC_EXECUTION_IMAGE=arc-execution:$ARC_VERSION
export ARC_CONSENSUS_IMAGE=arc-consensus:$ARC_VERSION
```

## Prepare data directory

Create the `$ARC_HOME` directory on the host before running Docker Compose. If it doesn't exist, Docker will create it as root and the `arc-snapshots` container will fail with permission errors:

```sh
mkdir -p "${ARC_HOME:-$HOME/.arc}"
```

## Download the compose file

Download `docker-compose.yml` into a working directory:

```sh
curl -O https://raw.githubusercontent.com/circlefin/arc-node/v${ARC_VERSION}/deployments/docker-compose.yml
```

## Start

If you have already exported `ARC_EXECUTION_IMAGE`, `ARC_CONSENSUS_IMAGE`, and
`ARC_HOME` as described above, run from the directory containing
`docker-compose.yml`:

```sh
docker compose up -d
```

Or with all variables inline:

```sh
export ARC_VERSION=0.6.0 ARC_HOME=~/.arc
export ARC_EXECUTION_IMAGE=docker.cloudsmith.io/circle/arc-network/arc-execution:$ARC_VERSION \
  ARC_CONSENSUS_IMAGE=docker.cloudsmith.io/circle/arc-network/arc-consensus:$ARC_VERSION
docker compose up -d
```

On the first run, init containers automatically:

1. Download the latest testnet snapshots (~84 GB compressed — see
   [download sizes](./running-an-arc-node.md#download-snapshots) for details)
2. Initialize the consensus layer private key
3. Prepare the shared IPC socket volume

Subsequent runs detect that initialization is already complete and start
immediately.

> The init container runs as root so it can set file ownership for the
> main services (UID 999). No manual `chown` is needed.

## Verify

On the first run, wait for the init containers to finish downloading snapshots
(`docker compose logs -f arc-snapshots`). Once the EL and CL containers start,
wait about 30 seconds, then check the latest block height:

```sh
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{ "jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1}'
```

The `result` field should increase over time as the node catches up with the
network. Initial sync from a snapshot may take several hours depending on how
far behind the snapshot is.

If the result remains `0x0`, check logs:

```sh
docker compose logs -f
```

## Monitoring

The containers expose Prometheus metrics on the host:

| Endpoint | Description |
|----------|-------------|
| `localhost:9001/metrics` | Execution Layer metrics |
| `localhost:29000/metrics` | Consensus Layer metrics |

## Stop

```sh
docker compose down
```

Node data persists in `~/.arc/` (or the path set by `ARC_HOME`). To remove
all data and start fresh:

```sh
docker compose down -v   # also removes the named sockets volume
rm -rf ~/.arc
```

> **Warning:** This permanently deletes the consensus layer private key
> (network identity). It cannot be recovered.
