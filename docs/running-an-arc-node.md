# Running an Arc Node

Arc is an open, EVM-compatible Layer-1 blockchain. Anyone can run an Arc node — no permission required. Running your own node gives you independent verification of the chain and direct API access to the network.

## What Your Node Does

- **Verifies every block** — Every block is cryptographically verified against the signatures of the validator set before it is accepted. Your node independently confirms that validators finalized each block;
- **Executes every transaction** — Every transaction is re-executed locally through the EVM. Your node maintains its own copy of the complete blockchain state;
- **Exposes a local RPC endpoint** — Your node provides a standard Ethereum JSON-RPC API (`http://localhost:8545`) for querying blocks, balances, and transactions, and for submitting calls directly against your own verified state.

An Arc node is composed of two processes:

- **Execution Layer (EL)**: executes finalized transactions and maintains the state of the blockchain;
- **Consensus Layer (CL)**: fetches finalized blocks, verifies their cryptographic signatures, and passes them to the EL for execution.

You can run a node using [binaries](#binaries) or [Docker](#docker).
Refer to the [installation](installation.md) instructions to obtain the
binaries or Docker images.

## Binaries

### Configure paths

This guide adopts the following variables to define paths of Arc components:

| Variable        | Meaning                                                                    | Default               |
|-----------------|----------------------------------------------------------------------------|-----------------------|
| `ARC_HOME`      | Base directory of installation. Base location of data directories.         | `~/.arc`              |
| `ARC_EXECUTION` | Data directory for the Execution layer (EL)                                | `$ARC_HOME/execution` |
| `ARC_CONSENSUS` | Data directory for the Consensus layer (CL)                                | `$ARC_HOME/consensus` |
| `ARC_BIN_DIR`   | Directory where Arc binaries are installed. Must be included in the `PATH` | `$ARC_HOME/bin`       |
| `ARC_RUN`       | Runtime directory for both Execution (EL) and Consensus (CL) layers.       | `/run/arc`            |

In a simplified version, define `$ARC_HOME` and `$ARC_RUN` variables once,
then use the derived variables in the remaining of this guide:

```sh
cat << "EOF" > ~/.arc_env
# Base directory for Arc node data (default: ~/.arc)
ARC_HOME="${ARC_HOME:-$HOME/.arc}"
ARC_BIN_DIR="${ARC_BIN_DIR:-$ARC_HOME/bin}"

# Linux runtime directory:
ARC_RUN="/run/arc"

# macOS runtime directory:
# ARC_RUN="$ARC_HOME/run"

ARC_EXECUTION=$ARC_HOME/execution
ARC_CONSENSUS=$ARC_HOME/consensus

export ARC_HOME ARC_BIN_DIR ARC_RUN ARC_EXECUTION ARC_CONSENSUS
export PATH="$ARC_BIN_DIR:$PATH"
EOF
```

Source it to load these variables into your current shell session:

```sh
source ~/.arc_env
```

Or using the POSIX shorthand: `. ~/.arc_env`

### Setup directories

The standard `arcup` installation sets up `$ARC_HOME=~/.arc` as base directory.
Create the **data directories** for the execution and consensus layers:

```sh
mkdir -p "$ARC_EXECUTION" "$ARC_CONSENSUS" "$ARC_BIN_DIR"
```

To set up the **runtime directory** in a **Linux** environment:

```sh
sudo install -d -o $USER "$ARC_RUN"
```

> When running Arc as a systemd service, `RuntimeDirectory=arc`
> sets up `/run/arc` automatically — the last command is not needed.

To set up the **runtime directory** in a **macOS** environment,
uncomment the `ARC_RUN="$ARC_HOME/run"` line above and run:

```sh
mkdir -p "$ARC_RUN"
```

Confirm that the installed binaries are available before downloading snapshots:

```sh
arc-snapshots --version
arc-node-execution --version
arc-node-consensus --version
```

### Download snapshots

Syncing a new Arc node from genesis is currently not supported.
A **snapshot** is needed to bootstrap the node:

```sh
arc-snapshots download \
  --chain=arc-testnet \
  --el-profile=full \
  --execution-path "$ARC_EXECUTION" \
  --consensus-path "$ARC_CONSENSUS"
```

Published snapshots use reth's storage v2 format. Rather than shipping the
execution layer as one compressed archive, it publishes a manifest listing each
database component on its own, which is what lets `--el-profile` fetch part of a
snapshot instead of all of it. The consensus layer is still a single `.tar.lz4`
archive. Everything below simply calls these snapshots.

The `arc-snapshots` binary is part of the Arc node installation. It queries
https://snapshots.arc.network for the newest snapshot that has both layers
published, whatever its retention, and restores them into `$ARC_EXECUTION` and
`$ARC_CONSENSUS`. The execution artifact is a reth manifest, which
`arc-snapshots` restores by invoking `arc-node-execution` (also part of the
installation), which must be on `PATH` or named by `ARC_EXECUTION_BINARY`.

`--el-profile` chooses how much execution-layer history to fetch: `minimal`,
`full`, or `archive`. It defaults to `minimal`, including for an explicit
manifest URL. The example passes `full` because the execution layer below starts
with `--full`. An archive node must pass `--el-profile=archive`; omitting the
flag produces a minimal restore. An explicit `.tar.lz4` execution URL still
selects the native archive restore and ignores this flag.

Automatically resolved manifest URLs carry no query string, and they must not.
Reth derives each component's URL by dropping the manifest's filename and
appending the component's as plain string concatenation, so a signed URL keeps
its query through that step and the component filename lands on the end of the
query string instead of the path. Every component is then fetched from an
address that does not exist. Do not pass a presigned manifest URL by hand.

A rerun without `--force` leaves a layer alone if it already holds the requested
snapshot. If it resolves a *newer* snapshot, the layer is replaced rather than
merged into, so expect a full download. And if a layer holds data that no restore
recorded — a node synced from genesis, or an earlier restore that did not finish —
the command stops and asks for `--force` rather than guessing which it is.
`--force` replaces both layers regardless of what they hold. See
[`crates/snapshots/README.md`](../crates/snapshots/README.md#restore-behavior)
for the exact rules.

> **Download sizes:** At the moment of writing the combined execution and consensus
layers uncompressed data is about 250GB.

### Initialize consensus layer

This is a one-time setup, producing the private key file used as network identity:

```sh
arc-node-consensus init --home $ARC_CONSENSUS
```

### Start execution layer

The Execution Layer (EL) is deployed by the `arc-node-execution` binary and started as follows:

```sh
arc-node-execution node \
  --chain arc-testnet \
  --datadir $ARC_EXECUTION \
  --full \
  --ipcpath $ARC_RUN/reth.ipc \
  --auth-ipc --auth-ipc.path $ARC_RUN/auth.ipc \
  --http --http.addr 127.0.0.1 --http.port 8545 \
  --http.api eth,net,web3,txpool,trace,debug \
  --rpc.forwarder https://rpc.testnet.arc.io/ \
  --metrics 127.0.0.1:9001 \
  --disable-discovery \
  --enable-arc-rpc
```

The `--chain` parameter configures the genesis file.
By using `--chain arc-testnet`, the genesis configuration bundled in the binary is adopted.
Replace with `--chain /path/to/genesis.json` if you have a custom genesis file.

The `--http`, `--http.addr`, and `--http.port` parameters expose a standard Ethereum
[JSON-RPC API](https://reth.rs/jsonrpc/intro).
The `--http.api` parameter defines the available RPC endpoints.
The `--rpc.forwarder` parameter routes requests not served locally to an existing RPC node.

The `arc-node-execution` binary accepts all parameters of a `reth` node.
Refer to its [documentation](https://reth.rs/cli/reth/node/) for details.

For externally-reachable nodes, consider adding `--public-api`. It
enforces hiding of pending-tx RPCs (a potential MEV vector) and warns if
`--http.api` / `--ws.api` exposes namespaces beyond the safe set
(`eth`, `net`, `web3`, `rpc`).

On high-traffic public endpoints, raise `--rpc.max-connections` (default `250`)
and `--rpc.max-subscriptions-per-connection` (default `32`) if clients see
`MaxConnections` or `TooManySubscriptions` errors. The defaults bound WebSocket
log-fanout memory growth and should only be raised, not lowered.

### Start consensus layer

After starting the [execution layer](#start-execution-layer), in a different terminal, start the consensus layer:

```sh
arc-node-consensus start \
  --home $ARC_CONSENSUS \
  --full \
  --eth-socket $ARC_RUN/reth.ipc \
  --execution-socket $ARC_RUN/auth.ipc \
  --rpc.addr 127.0.0.1:31000 \
  --follow \
  --follow.endpoint https://rpc.testnet.arc.io,wss=rpc.testnet.arc.io \
  --follow.endpoint https://rpc.drpc.testnet.arc.io,wss=rpc.drpc.testnet.arc.io \
  --follow.endpoint https://rpc.blockdaemon.testnet.arc.io,wss=rpc.blockdaemon.testnet.arc.io/websocket \
  --execution-persistence-backpressure \
  --execution-persistence-backpressure-threshold=50 \
  --metrics 127.0.0.1:29000
```

The consensus layer attempts to connect to the execution layer via the provided
`--eth-socket`.
For this reason, always start the execution layer first.
Otherwise, the consensus layer may fail to start, if it fails to connect to the
companion execution layer.

The consensus layer operates in the **follow** mode.
We provide three endpoints from which the node retrieves finalized blocks.

### Verify operation

After starting both the consensus and execution layer, wait about 30 seconds.
Then, check the latest block height:

```sh
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{ "jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1}'
```

The produced output is in JSON format.
The `result` field represents the next block height, in hexadecimal
(you can use `printf "%0d"` to translate it into decimal).
It should increase over time.
If it remains `0x0`, check the logs of the consensus layer for errors.
Common causes are a missing or incomplete snapshot, mismatched `$ARC_RUN`
between the two processes, or the consensus layer not reaching any follow
endpoint.

> Notice that this command queries the execution layer's HTTP server offering
> a local JSON-RPC API.
> If the address and port of the HTTP endpoint are configured differently than
> the above example, adapt the command accordingly.

## Docker

As an alternative to running binaries directly, you can run an Arc node
using Docker containers. See [Installation: Docker](installation.md#docker)
for how to obtain the images.

### Prerequisites

- [Docker Engine](https://docs.docker.com/engine/install/) 24+ with BuildKit
- [Docker Compose](https://docs.docker.com/compose/install/) v2
- Meets the [system requirements](#system-requirements)

### Set environment variables

The compose file reads images from environment variables. Set the version,
data directory, and image references before running any `docker compose`
command. Refer to the [Versions](installation.md#versions) table for the
current release:

```sh
export ARC_VERSION=<version>
export ARC_HOME=~/.arc
```

If you pulled pre-built images from Cloudsmith:

```sh
export ARC_EXECUTION_IMAGE=docker.cloudsmith.io/circle/arc-network/arc-execution:$ARC_VERSION
export ARC_CONSENSUS_IMAGE=docker.cloudsmith.io/circle/arc-network/arc-consensus:$ARC_VERSION
```

If you built the images locally:

```sh
export ARC_EXECUTION_IMAGE=arc-execution:$ARC_VERSION
export ARC_CONSENSUS_IMAGE=arc-consensus:$ARC_VERSION
```

### Prepare data directory

Create the `$ARC_HOME` directory on the host before running Docker Compose.
If it doesn't exist, Docker will create it as root and the `arc-snapshots`
container will fail with permission errors:

```sh
mkdir -p "${ARC_HOME:-$HOME/.arc}"
```

### Download the compose file

Download `docker-compose.yml` into a working directory:

```sh
curl -O https://raw.githubusercontent.com/circlefin/arc-node/v${ARC_VERSION}/deployments/docker-compose.yml
```

### Start

Run from the directory containing `docker-compose.yml`:

```sh
docker compose up -d
```

On the first run, init containers automatically:

1. Download the latest complete testnet snapshot pair. See
   [download sizes](#download-snapshots) for a measured minimal restore.
2. Initialize the consensus layer private key
3. Prepare the shared IPC socket volume

The init containers run again on every `docker compose up`. Normally they finish in
seconds, because each layer already holds the snapshot it was given. Two situations
are exceptions.

**A newer snapshot has been published.** With no URLs in the command, the
service asks the API for the latest snapshot on every run, and a newer one
usually exists within hours. It restores that one instead, which costs a second
full download and moves the node back to the snapshot's block, discarding
whatever it had synced since.

**A previous restore did not finish.** A layer is marked as restored only after
its download and extraction have both finished, so an interrupted run leaves
data behind with no mark on it. On the next run the service cannot tell that
half-written snapshot apart from a directory you filled yourself, by syncing
from genesis or by running a validator, so it stops with an error rather than
delete someone else's data. Nothing else starts either, because the rest of the
stack waits on this container. Add `--force` to let it overwrite the layer,
bring the stack up once, then take the flag out again:

```yaml
    command:
      - download
      - --chain=arc-testnet
      - --el-profile=minimal
      - --force
      - --execution-path=/data/execution
      - --consensus-path=/data/consensus
```

`FORCE_SNAPSHOT_RESTORE=true` has no effect on this stack. `arc-snapshots` does not
read it; `--force` is how a clean restore is requested.

> The init container runs as root so it can set file ownership for the
> main services (UID 999). No manual `chown` is needed.

### Verify

On the first run, wait for the init containers to finish downloading snapshots
(`docker compose logs -f arc-snapshots`). Once the EL and CL containers start,
wait about 30 seconds, then check the latest block height:

```sh
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{ "jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1}'
```

The `result` field should increase over time as the node catches up with the
network. If it remains `0x0`, check logs:

```sh
docker compose logs -f
```

### Docker monitoring

The containers expose Prometheus metrics on the host:

| Endpoint | Description |
|----------|-------------|
| `localhost:9001/metrics` | Execution Layer metrics |
| `localhost:29000/metrics` | Consensus Layer metrics |

### Stop

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

## Separated hosts

> [!WARNING]
> Running EL and CL on separate hosts requires the RPC/HTTP Engine API transport,
> which is deprecated as of `v0.8.0` and will be removed in `v0.9.0`.
> Run both layers on the same host and use IPC instead (see the
> [Binaries](#binaries) section).
> The consensus layer logs a startup warning when any RPC option is set.

The [Binaries](#binaries) section describes the setup of the execution
(EL) and consensus (CL) layers running in the same host.
The two processes interact via Inter-Process Communication (IPC),
namely using local sockets to which both processes have read and write access.

To run EL and CL in separated hosts, the two processes must instead interact
using the Remote Procedure Call (RPC) protocol.

### Authentication

To authenticate the connection between EL and CL, a JSON Web Token (JWT) is employed:

```sh
openssl rand -hex 32 | tr -d "\n" > "$ARC_HOME/jwtsecret"
chmod 600 "$ARC_HOME/jwtsecret"
```

Notice that both hosts must have access to this random token file.
Generate it in one host and securely copy it into the other host.

### Execution layer

From the [Start execution layer](#start-execution-layer) instructions, three
changes are required:

1. Remove all flags related to IPC communication: `--ipcpath`, `--auth-ipc`,
   `--auth-ipc.path`;
2. Rebind the eth JSON-RPC off loopback so the consensus layer's host can reach
   it: change `--http.addr 127.0.0.1` to an interface the consensus layer can
   reach. The consensus layer runs a startup connectivity check against this endpoint
   and will not start if it is unreachable.
3. Add the following parameters to configure the authenticated Engine API:
```sh
  --authrpc.addr 0.0.0.0 \
  --authrpc.port 8551 \
  --authrpc.jwtsecret "$ARC_HOME/jwtsecret"
```

If the consensus layer runs persistence backpressure — the base
[Start consensus layer](#start-consensus-layer) example enables it — the
execution layer must also run a WebSocket server exposing the `reth` namespace,
or backpressure stays inactive (the consensus layer still starts and retries the
connection in the background). The consensus layer derives the WebSocket address
from `--eth-rpc-endpoint` (http→ws, port + 1), i.e. port `8546`:

```sh
  --ws --ws.addr 0.0.0.0 --ws.port 8546 --ws.api reth
```

> [!IMPORTANT]
> With this setup, ports 8545 (eth JSON-RPC), 8546 (WebSocket, when
> backpressure is enabled), and 8551 (Engine API) are exposed on all network
> interfaces (`0.0.0.0`). Configure the firewall to restrict access to these ports
> to the consensus layer's host. The Engine API (8551) controls block production —
> **never** expose it to the public internet.

### Consensus layer

From the [Start consensus layer](#start-consensus-layer) instructions, two changes are required:

1. Remove all flags related to IPC communication: `--eth-socket` and `--execution-socket`;
2. Add the following parameters to configure the RPC interaction:
```sh
  --eth-rpc-endpoint http://$EL_ADDR:8545 \
  --execution-endpoint http://$EL_ADDR:8551 \
  --execution-jwt "$ARC_HOME/jwtsecret"
```

Where `EL_ADDR` is the network address (IP or hostname) of the host running the execution layer.

The `--eth-rpc-endpoint` parameter refers to the EL's HTTP server exposing a
standard and open Ethereum [JSON-RPC API](https://reth.rs/jsonrpc/intro).

The `--execution-endpoint` parameter should match the EL's `--authrpc`
address and port, exposing the _protected_ RPC endpoint.

---

## RPC Provider Nodes

RPC provider nodes (relay nodes) serve public JSON-RPC traffic on behalf of the
network. They differ from follow nodes in two key ways:

1. **Direct peering** — they connect to network sentries via devp2p (EL) and
   libp2p (CL), rather than using follow-mode HTTP/WebSocket endpoints.
2. **Public exposure** — they expose RPC endpoints to the public internet,
   requiring a more restrictive configuration.

This section covers the configuration specific to RPC provider nodes. It assumes
you have completed the initial setup steps from [Binaries](#binaries) (paths,
directories, snapshots, consensus init). For EL ↔ CL communication options (IPC
vs RPC), see the [Binaries](#binaries) and [Separated hosts](#separated-hosts)
sections.

### RPC namespaces

Only the following namespaces may be exposed:

```text
eth,net,web3,rpc
```

All other namespaces **must not** be enabled. Specifically, the following are
prohibited: `txpool`, `debug`, `trace`, `admin`, `flashbots`, `mev`, `ots`.

```bash
--http.api eth,net,web3,rpc
--ws.api eth,net,web3,rpc
--public-api
```

The `--public-api` flag enforces these restrictions at runtime: it hides
pending-transaction RPCs and warns at startup if `--http.api` or `--ws.api`
exposes namespaces outside the safe set.

To verify, call `rpc_modules`:

```bash
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rpc_modules","params":[],"id":1}' | jq .result
```

Expected output (the `arc` namespace is added automatically by `--enable-arc-rpc`):

```json
{
  "arc": "1.0",
  "net": "1.0",
  "rpc": "1.0",
  "eth": "1.0",
  "web3": "1.0"
}
```

If you see `txpool`, `debug`, `trace`, `admin`, or any other unexpected
namespace, the node is misconfigured.

### Arc RPC extension

The `--enable-arc-rpc` flag adds a custom `arc` JSON-RPC namespace with two
methods:

- `arc_getCertificate(height)` — returns the BFT commit certificate for a given
  block height
- `arc_getVersion()` — returns build and version information

The EL proxies certificate requests to the co-located CL's REST API, defaulting
to `http://127.0.0.1:31000`. Override with `--arc-rpc-upstream-url` or the
`ARC_RPC_UPSTREAM_URL` environment variable if the CL listens on a different
address. Your load balancer must allow `arc_getCertificate` on the EL's RPC port
(8545).

`arc_getCertificate` responses are safe to cache by height at the edge — once a
block is finalized, any valid certificate for that height is valid indefinitely.
The same applies to `eth_getBlockByNumber` — finalized block payloads are
immutable and safe to cache by block number.

### Transaction propagation

Transactions must only be propagated to trusted peers:

```bash
--tx-propagation-policy Trusted
```

### Peering

Use `--trusted-peers` with the enode URLs provided during onboarding:

```bash
--trusted-peers <ENODE_URLS>
```

RPC nodes run as full nodes:

```bash
--full
```

### Prohibited configuration

Do **not** use any of the following:

| Flag / Feature                    | Reason                                                |
| --------------------------------- | ----------------------------------------------------- |
| `--rpc.forwarder`                 | Request forwarding to upstream must not be configured |
| Bundle APIs                       | Bundle submission endpoints must not be exposed       |
| `--http.api all` / `--ws.api all` | Exposes prohibited namespaces                         |

### Firewall and network

| Port  | Service                 | Protocol  | Exposure                                                              |
| ----- | ----------------------- | --------- | --------------------------------------------------------------------- |
| 8545  | EL HTTP RPC             | TCP       | **Public** — serves end users and permissionless nodes                |
| 8546  | EL WebSocket RPC        | TCP       | **Public** — serves end users and permissionless nodes                |
| 30303 | EL P2P (devp2p)         | TCP + UDP | **Public** — allows permissionless nodes to gossip transactions       |
| 27000 | CL P2P (libp2p)         | TCP       | **Restricted** — allow only IPs of peers provided during onboarding   |
| 8551  | EL Engine API (authrpc) | TCP       | **Internal only** — CL-to-EL communication, never expose externally   |
| 31000 | CL RPC                  | TCP       | **Internal only** — required for CL operation, never expose externally|

Key rules:

- RPC ports (8545, 8546) and EL P2P port (30303) are the **only** ports that
  should be open to the public internet.
- CL P2P port (27000) must be reachable by network sentries but **not** by the
  general public. Use IP allowlisting or the `--p2p.persistent-peers-only` flag.
- Engine API (8551) and CL RPC (31000) must **never** be exposed outside the
  host. Bind to `127.0.0.1` if EL and CL are colocated, or use a private
  network interface.
- If you enable metrics (`--metrics` for both EL and CL), restrict the metrics
  ports accordingly.
- Deploy a reverse proxy or load balancer in front of ports 8545 and 8546 with
  rate limiting, request size limits, and connection throttling. The node itself
  does not enforce per-client request limits, so without an external layer,
  a single client can saturate the RPC interface.

### Consensus layer configuration

The CL on an RPC node syncs decided blocks from sentry endpoints rather than
using follow mode:

```bash
--p2p.persistent-peers <SENTRY_MULTIADDRS>
```

Replace `<SENTRY_MULTIADDRS>` with the peer multiaddrs provided during onboarding.

To restrict CL P2P to only the configured persistent peers (recommended, since
RPC nodes should only communicate with sentries):

```bash
--p2p.persistent-peers-only
```

The `--rpc.addr` flag is **required** when `--enable-arc-rpc` is enabled on the
EL:

```bash
--rpc.addr=127.0.0.1:31000
```

#### Sync-only mode

By default the CL participates in the consensus protocol. To run a node that
only syncs blocks without participating in consensus (recommended for RPC
nodes):

```bash
--no-consensus
```

When set, the node only runs the synchronization protocol and does not subscribe
to consensus-related gossip topics.

### Complete example

This example uses IPC for EL ↔ CL communication (recommended when colocated).
See [Separated hosts](#separated-hosts) for the RPC alternative.

```bash
# Execution Layer (start first)
arc-node-execution node \
  --chain=arc-testnet \
  --datadir=$ARC_EXECUTION \
  --trusted-peers <ENODE_URLS> \
  --full \
  --http --http.addr=0.0.0.0 --http.port=8545 \
  --http.api eth,net,web3,rpc \
  --ws --ws.addr=0.0.0.0 --ws.port=8546 \
  --ws.api eth,net,web3,rpc \
  --enable-arc-rpc \
  --public-api \
  --tx-propagation-policy Trusted \
  --metrics 127.0.0.1:9001 \
  --ipcpath=$ARC_RUN/reth.ipc \
  --auth-ipc \
  --auth-ipc.path=$ARC_RUN/auth.ipc

# Consensus Layer (start after EL is healthy)
arc-node-consensus start \
  --home=$ARC_CONSENSUS \
  --p2p.persistent-peers <SENTRY_MULTIADDRS> \
  --p2p.persistent-peers-only \
  --rpc.addr=127.0.0.1:31000 \
  --eth-socket=$ARC_RUN/reth.ipc \
  --execution-socket=$ARC_RUN/auth.ipc \
  --no-consensus \
  --metrics 127.0.0.1:29000
```

**Important notes:**

- Start the EL first. The CL connects to the EL on startup, so it will fail if
  the EL is not running.
- The `--rpc.addr` flag on the CL is **required** because of the EL
  `--enable-arc-rpc` flag.
- When using IPC, both processes must have read/write access to the socket
  directory. If running in containers, mount the same directory into both.

### Checklist

**General:**

- [ ] `arc-node-consensus init` has been run
- [ ] `--http.api` and `--ws.api` set to `eth,net,web3,rpc` only
- [ ] Prohibited namespaces (`txpool`, `debug`, `trace`, `admin`, `flashbots`, `mev`, `ots`) return "Method not found"
- [ ] `--enable-arc-rpc` is set and `arc_getCertificate` is accessible on port 8545
- [ ] `--public-api` is set on the EL
- [ ] Pending-transaction RPCs are hidden — verify with:
  ```bash
  curl -s -X POST http://localhost:8545 \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_newPendingTransactionFilter","params":[],"id":1}' \
    | jq .error
  ```
  This should return an error (code `-32001`), not a filter ID.
- [ ] `--tx-propagation-policy Trusted` is set
- [ ] `--rpc.forwarder` is **not** configured
- [ ] No bundle APIs are exposed
- [ ] `--trusted-peers` is set to the enode URLs provided during onboarding
- [ ] `--full` is set on the EL
- [ ] Ports 8545, 8546 and 30303 are open to the public internet
- [ ] Port 27000 (CL P2P) is restricted to peer IPs provided during onboarding
- [ ] Engine API (8551, if using RPC) and CL RPC (31000) are not externally accessible
- [ ] CL has `--rpc.addr` set
- [ ] CL has `--no-consensus` set (recommended)

**If using IPC:**

- [ ] JWT secret is **not** configured (mutually exclusive with IPC)
- [ ] EL has `--ipcpath`, `--auth-ipc`, and `--auth-ipc.path` set
- [ ] CL has `--eth-socket` and `--execution-socket` set
- [ ] Both processes can read/write the socket directory

**If using RPC:**

- [ ] JWT secret file has been generated and is accessible to both EL and CL
- [ ] EL exposes auth RPC via `--authrpc.addr`, `--authrpc.port`, and `--authrpc.jwtsecret`
- [ ] CL connects to EL via `--eth-rpc-endpoint`, `--execution-endpoint`, and `--execution-jwt`
- [ ] `--arc-rpc-upstream-url` points to the CL's RPC address (the default `http://127.0.0.1:31000` only works when colocated)

---

## Operational Guide

### System Requirements

| Component | Minimum |
|-----------|---------|
| CPU | Higher clock speed over core count |
| Memory | 64 GB+ |
| Storage | 1 TB+ NVMe SSD (TLC recommended) |
| Network | Bandwidth: Stable 24 Mbps+ |


Check out [reth system requirements](https://reth.rs/run/system-requirements/) for more info on EL configuration.

**Note**: during periods of sustained high load, such as during startup or extended sync if the node is far behind, the execution layer memory may surge on some hardware. This should not be an issue if running with the suggested System Requirements. However, if you do observe this, you can enable backpressure to throttle the pace of execution according to the speed of disk writes, which will constrain memory growth.

Backpressure works by having the consensus layer subscribe to the execution
layer's `reth_subscribePersistedBlock` notification and pause block replay until
the execution layer's persisted height catches up.
Activate it on the consensus layer:

```sh
--execution-persistence-backpressure \
--execution-persistence-backpressure-threshold=10
```

No `--http.api` change is needed on the execution layer.
How the notification reaches the consensus layer depends on the transport:

- **IPC (recommended):** automatic. Reth serves the `reth` namespace on the
  `--ipcpath` socket by default, so the [primary setup](#start-execution-layer)
  above works as-is.
- **HTTP/RPC transport (deprecated):** the notification is a subscription,
  which plain HTTP JSON-RPC cannot carry. The execution layer must also run a
  WebSocket server that exposes the `reth` namespace:

  ```sh
  --ws --ws.addr 127.0.0.1 --ws.port 8546 --ws.api reth
  ```

  The consensus layer derives the WebSocket URL from `--eth-rpc-endpoint`
  (http→ws, port + 1), or takes it from `--execution-ws-endpoint`.
  Bind `--ws.addr` to an interface the consensus layer can reach: `127.0.0.1` works
  only when both layers share a host.
  On [separated hosts](#separated-hosts) use a reachable interface and firewall
  port 8546 to the consensus layer's host.
  If the consensus layer cannot reach the WebSocket server it still starts —
  backpressure just never engages.
  Adding `reth` to `--http.api` does **not** enable the subscription.

Note: arc-node is alpha software and this performance issue is actively being worked on.

### Production Deployment

For production, run both processes as systemd services.

> **Note:** The service files below use `$USER` and `$HOME`, which the shell expands to your current username and home directory before writing the file. Review the generated file with `sudo cat /etc/systemd/system/arc-execution.service` after creation to confirm the paths are correct.

#### Execution Layer Service

```sh
sudo tee /etc/systemd/system/arc-execution.service > /dev/null <<EOF
[Unit]
Description=Arc Node - Execution Layer
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$USER
Group=$USER
RuntimeDirectory=arc
Environment=RUST_LOG=info
WorkingDirectory=$HOME/.arc
ExecStart=/usr/local/bin/arc-node-execution node \
  --chain arc-testnet \
  --datadir $HOME/.arc/execution \
  --full \
  --disable-discovery \
  --ipcpath /run/arc/reth.ipc \
  --auth-ipc \
  --auth-ipc.path /run/arc/auth.ipc \
  --http \
  --http.addr 127.0.0.1 \
  --http.port 8545 \
  --http.api eth,net,web3,txpool,trace,debug \
  --metrics 127.0.0.1:9001 \
  --enable-arc-rpc \
  --rpc.forwarder https://rpc.testnet.arc.io/

Restart=always
RestartSec=10
KillSignal=SIGTERM
TimeoutStopSec=300
StandardOutput=journal
StandardError=journal
SyslogIdentifier=arc-execution
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
EOF
```

#### Consensus Layer Service

```sh
sudo tee /etc/systemd/system/arc-consensus.service > /dev/null <<EOF
[Unit]
Description=Arc Node - Consensus Layer
After=arc-execution.service
Requires=arc-execution.service

[Service]
Type=simple
User=$USER
Group=$USER
Environment=RUST_LOG=info
WorkingDirectory=$HOME/.arc
ExecStart=/usr/local/bin/arc-node-consensus start \
  --home $HOME/.arc/consensus \
  --full \
  --eth-socket /run/arc/reth.ipc \
  --execution-socket /run/arc/auth.ipc \
  --rpc.addr 127.0.0.1:31000 \
  --follow \
  --follow.endpoint https://rpc.testnet.arc.io,wss=rpc.testnet.arc.io \
  --follow.endpoint https://rpc.drpc.testnet.arc.io,wss=rpc.drpc.testnet.arc.io \
  --follow.endpoint https://rpc.blockdaemon.testnet.arc.io,wss=rpc.blockdaemon.testnet.arc.io/websocket \
  --execution-persistence-backpressure \
  --execution-persistence-backpressure-threshold=50 \
  --metrics 127.0.0.1:29000

Restart=always
RestartSec=10
KillSignal=SIGTERM
TimeoutStopSec=300
StandardOutput=journal
StandardError=journal
SyslogIdentifier=arc-consensus
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
EOF
```

#### Enable and Start

```sh
sudo systemctl daemon-reload
sudo systemctl enable arc-execution arc-consensus
sudo systemctl start arc-execution arc-consensus
```

### Monitoring

For a Prometheus + Grafana setup on a single host, see [Monitoring an Arc Node](./monitoring.md).

```sh
# Check service status
sudo systemctl status arc-execution
sudo systemctl status arc-consensus

# Check block height (should be steadily increasing)
cast block-number --rpc-url http://localhost:8545

# Check latest block
cast block --rpc-url http://localhost:8545

# View logs
sudo journalctl -u arc-execution -f
sudo journalctl -u arc-consensus -f
```

> `cast` requires [Foundry](https://book.getfoundry.sh/getting-started/installation).

For production monitoring, scrape the Prometheus metrics endpoints with Grafana:

| Endpoint | Description |
|----------|-------------|
| `localhost:9001/metrics` | Execution Layer metrics |
| `localhost:29000/metrics` | Consensus Layer metrics |

### Pruning

The `--full` and `--minimal` flags are accepted by both the CL and EL and will enable pruning.

> **Caution:** EL pruning increases memory usage and may cause out-of-memory
> issues on constrained machines. If you encounter memory pressure, enable
> backpressure (see [System Requirements](#system-requirements) section) and remove
> `--full` after the first successful start.

`--full` and `--minimal` are the execution layer's two pruning presets:
`--full` retains more history (the last 237,600 blocks for most data),
`--minimal` far less (for example, 64 blocks of receipts); running with neither
keeps everything (archive). Run `arc-node-execution --help`, or see the
[execution binary reference](../crates/node/README.md), for the exact
per-preset retention. Published snapshots are in `archive` format (i.e., not
pruned), while `--el-profile` decides how much execution data to restore. Use
the profile corresponding to how the node will run: `minimal` with `--minimal`,
`full` with `--full`, or `archive` with neither preset. Starting `--minimal`
against a datadir restored with `--el-profile=full` or `--el-profile=archive`
still needs the offline procedure below:

> [!IMPORTANT]
> **Switching to `--minimal` after a snapshot restore.** Starting directly
> with `--minimal` against a datadir restored from a `--full` or archive
> snapshot makes the online pruner delete the whole difference between the two
> presets while racing to tip — the node can get stuck in sync. Prune offline
> first:
>
> 1. Stop `arc-node-execution` and `arc-node-consensus`.
> 2. Delete `$ARC_EXECUTION/reth.toml`.
> 3. Briefly start the EL with `--minimal` (plus your steady-state flags)
>    until the `Saving prune config to toml file` log line, then stop it. The
>    `prune` subcommand reads its target profile from `reth.toml`, not from
>    `--minimal`, so this step is what writes it.
> 4. Confirm `$ARC_EXECUTION/reth.toml` contains `prune.profile = "minimal"`.
> 5. Run `arc-node-execution prune --datadir $ARC_EXECUTION`.
> 6. Start both layers with `--minimal`.
>
> To change preset later (`--minimal` ↔ `--full`, or to/from archive), delete
> `reth.toml` and restart with the preset flag you want.
