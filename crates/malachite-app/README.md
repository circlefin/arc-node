# arc-node-consensus

This is a [Malachite][malachite] application that uses a channels-based API, developed for the Arc network.

It serves as a shim layer (proxy) between the execution client (EL), such as [reth][reth], and the consensus client (CL), Malachite. Communication with the EL is handled via the [Engine API][engine-api].

## Table of Contents

- [Usage](#usage)
  - [Init](#init)
  - [Start](#start)
    - [Validator](#a-validator)
    - [Full node](#b-full-node)
    - [Follow mode (RPC sync)](#c-follow-mode-rpc-sync)
  - [Optional Flags](#optional-flags)
  - [Remote Signing](#remote-signing)
  - [Download](#download)
  - [Key](#key)
  - [DB](#db)
    - [Migrate (Upgrade)](#migrate-upgrade)
    - [Compact](#compact)
- [Environment Variables](#environment-variables)
- [REST API](#rest-api)
  - [API Versioning](#api-versioning)
  - [Available Endpoints](#available-endpoints)
  - [Example API Usage](#example-api-usage)
  - [Deprecation Policy](#deprecation-policy)
- [Metrics](#metrics)

## Usage

### Init

```bash
arc-node-consensus init --home=~/.arc/consensus
```

Generates the private validator key file in `~/.arc/consensus/config/priv_validator_key.json`.

Use `--overwrite` to regenerate the key if it already exists:

```bash
arc-node-consensus init --home=~/.arc/consensus --overwrite
```

> The private validator key file contains the private key for the libp2p network identity.
>
> Moreover, when `--signing.remote` is disabled, this private key is also used for signing consensus messages and therefore constitutes the consensus identity from which the validator's address is derived.

### Start

#### a) Validator

A validator is a node that participates in consensus, proposes and votes on blocks, and is responsible for finalizing the blockchain.


**Minimal example**:

```bash
arc-node-consensus start \
   --home=~/.arc/consensus \
   --moniker=validator-1 \
   --eth-socket=/tmp/reth.ipc \
   --execution-socket=/tmp/auth.ipc \
   --minimal
```

**Full example with IPC** (recommended for colocated reth and malachite):

```bash
arc-node-consensus start \
   --home=~/.arc/consensus \
   --moniker=validator-1 \
   --p2p.addr=/ip4/172.19.0.5/tcp/27000 \
   --p2p.persistent-peers=/ip4/172.19.0.6/tcp/27000,/ip4/172.19.0.7/tcp/27000 \
   --metrics=172.19.0.5:29000 \
   --rpc.addr=0.0.0.0:31000 \
   --eth-socket=/tmp/reth.ipc \
   --execution-socket=/tmp/auth.ipc \
   --minimal
```

**Full example with RPC** (for remote deployments):

```bash
arc-node-consensus start \
   --home=~/.arc/consensus \
   --moniker=validator-1 \
   --p2p.addr=/ip4/172.19.0.5/tcp/27000 \
   --p2p.persistent-peers=/ip4/172.19.0.6/tcp/27000,/ip4/172.19.0.7/tcp/27000 \
   --metrics=0.0.0.0:29000 \
   --rpc.addr=0.0.0.0:31000 \
   --eth-rpc-endpoint=http://localhost:8545 \
   --execution-endpoint=http://localhost:8551 \
   --execution-jwt=jwtsecret \
   --minimal
```

Note: to generate a JWT (JSON web token), use the following command:

```bash
openssl rand -hex 32 | tr -d "\n" > "jwtsecret"
```

#### b) Full node

A full node participates in block and transaction propagation, verifies consensus rules, but does **not** propose or vote on blocks. It keeps up with consensus and validates all blocks/txs, ensuring a full copy of network state, but doesn't require a validator private key.

```bash
arc-node-consensus start \
   --home=~/.arc/consensus \
   --moniker=full-1 \
   --eth-socket=/tmp/reth.ipc \
   --execution-socket=/tmp/auth.ipc \
   --no-consensus \
   --full
```

#### c) Follow mode (RPC sync)

Follow mode syncs blocks from trusted RPC endpoints instead of participating in P2P consensus. The node fetches blocks via HTTP, verifies commit certificates, and applies them locally. This is useful for read-only nodes that sync from validators without joining the P2P network.

Follow mode implies `--no-consensus` automatically.

```bash
arc-node-consensus start \
   --home=~/.arc/consensus \
   --moniker=follower-1 \
   --eth-socket=/tmp/reth.ipc \
   --execution-socket=/tmp/auth.ipc \
   --follow \
   --follow.endpoint http://validator1:26658 \
   --follow.endpoint http://validator2:26658 \
   --full
```

Multiple `--follow.endpoint` flags can be provided for redundancy. The endpoint format supports an optional WebSocket override for streaming:

```
http://validator1:26658,ws=8546
https://example.com,wss=ws.example.com:1212
```

#### Optional Flags

- `--moniker` - Human-readable name for this node (if not provided, a random moniker like "brave-validator-742" will be generated)
- `--p2p.addr` - P2P listen multiaddr (default: `/ip4/0.0.0.0/tcp/27000`). Example: `/ip4/172.19.0.5/tcp/27000` or `/ip4/127.0.0.1/udp/27000/quic-v1`

- `--p2p.persistent-peers` - Comma-separated list of persistent peer multiaddrs
- `--p2p.persistent-peers-only` - Only allow connections to/from persistent peers (default: false). Useful for sentry node setups where a validator should only communicate with known trusted peers.
- `--no-consensus` - Run as a full node that syncs with the network but does not participate in consensus as a validator (no block proposing or voting).
- `--discovery` - Enable peer discovery (default: false)
- `--discovery.num-outbound-peers` - Number of outbound peers (default: 20)
- `--discovery.num-inbound-peers` - Number of inbound peers (default: 20)
- `--value-sync` - Enable value sync (default: true)
- `--metrics` - Enable metrics and set listen address (e.g., "0.0.0.0:29000")
- `--rpc.addr` - Enable RPC and set listen address (e.g., "0.0.0.0:31000")
- `--full` - Arc full-node pruning preset; sets `--prune.certificates.distance 237600`; mutually exclusive with `--minimal` and the individual `--prune.certificates.*` flags
- `--minimal` - Arc minimal-storage pruning preset; sets `--prune.certificates.distance 237600`; mutually exclusive with `--full` and the individual `--prune.certificates.*` flags
- `--prune.certificates.distance` - Keep certificates for the last N heights (default: 0, disabled/archive node); mutually exclusive with `--prune.certificates.before` and `--full/--minimal` presets
- `--prune.certificates.before` - Prune all certificates below this height (default: 0, disabled); mutually exclusive with `--prune.certificates.distance` and `--full/--minimal` presets
- `--log-level` - Log level: "trace", "debug", "info", "warn", "error" (default: "debug")
- `--log-format` - Log format: "plaintext" or "json" (default: "plaintext")
- `--pprof.addr` - Profiling server bind address (default: "0.0.0.0:6060")
- `--suggested-fee-recipient` - Address to receive tips and rewards
- `--follow` - Enable RPC sync mode. The node fetches blocks from trusted RPC endpoints instead of participating in consensus (requires `--follow.endpoint`)
- `--follow.endpoint <ENDPOINT>` - RPC endpoint to fetch blocks from in sync mode. Can be repeated. Format: `http://host:port[,ws=port]` (requires `--follow`)
- `--runtime.flavor` - Tokio runtime flavor: "single-threaded" or "multi-threaded" (default: "multi-threaded")
- `--runtime.worker-threads <COUNT>` - Number of worker threads for the multi-threaded runtime (default: number of CPU cores; ignored with single-threaded)
- `--private-key <PATH>` - Path to private validator key file. Used for P2P identity and (when not using `--signing.remote`) consensus signing. Default: `{home}/config/priv_validator_key.json`
- `--db.skip-upgrade` - Skip database schema upgrade on startup
- `--signing.remote` - Use remote signing with specified endpoint URL (if not provided, uses local signing)
- `--signing.tls-cert-path` - Path to TLS certificate file for remote signing; auto-enables TLS (requires `--signing.remote`)

#### Remote Signing

For validator nodes that use a remote signing service instead of local private keys:

```bash
arc-node-consensus start \
   --home=~/.arc/consensus \
   --moniker=validator-1 \
   --eth-socket=/tmp/reth.ipc \
   --execution-socket=/tmp/auth.ipc \
   --minimal \
   --signing.remote=http://validator-signer-proxy:10340 \
   --signing.tls-cert-path=/path/to/ca_cert.pem
```

Note: The remote signer timeout is hardcoded to 30 seconds.

### Download

Download a consensus layer snapshot and extract it into the home directory.

The snapshot archive uses bare paths — files are extracted directly into `--home` without any prefix stripping. For example, a `store.db` entry in the archive lands at `~/.arc/consensus/store.db`.

```bash
arc-node-consensus download \
  --home=~/.arc/consensus \
  --url <cl-snapshot-url>
```

If `--url` is omitted, the latest pruned snapshot for the selected `--chain` is fetched automatically from the snapshot API.

```bash
# Devnet — latest snapshot (recommended)
arc-node-consensus download \
  --home=~/.arc/consensus \
  --chain arc-devnet
```

> For a full node restore (EL + CL), use the `arc-snapshots` tool instead — it downloads both archives in one command. See [`crates/snapshots/README.md`](../snapshots/README.md).

### Key

Display the public key and address derived from the private validator key:

```bash
arc-node-consensus key --home=~/.arc/consensus
```

Optionally pass a key file path directly:

```bash
arc-node-consensus key /path/to/priv_validator_key.json
```

### DB

The `db` command provides database maintenance operations for the consensus layer database.

#### Migrate (Upgrade)

Migrate the database schema to the latest version (also available as `db upgrade`). This is useful when upgrading to a new version of the software that includes database schema changes.

Normally, database migrations are applied automatically each time the node starts. Running this command manually is only necessary if the automatic migration fails during startup.

```bash
arc-node-consensus db migrate --home=~/.arc/consensus
```

Use `--dry-run` to check what migrations would be applied without executing them:

```bash
arc-node-consensus db migrate --home=~/.arc/consensus --dry-run
```

#### Compact

Compact the database to reclaim disk space. This operation rewrites the database file to remove fragmentation and reclaim space from deleted records.

**Important:** The node must be stopped before running the compact command.

```bash
arc-node-consensus db compact --home=~/.arc/consensus
```

## Environment Variables

The following environment variables can be used to modify behavior:

- `ARC_HALT_AT_BLOCK_HEIGHT` - If set to a non-zero value, the node will gracefully shut down after reaching this block height. Used for automated testing.

## REST API

The consensus layer exposes a REST API for monitoring and querying consensus state when `--rpc.addr` is set (e.g., `--rpc.addr=0.0.0.0:26658`).

### API Versioning

The REST API uses **header-based versioning** with custom Accept headers:

```bash
Accept: application/vnd.arc.v{N}+json
```

**Current Version:** `v1`

#### Making Versioned Requests

**Explicit Version (Recommended):**
```bash
curl -H "Accept: application/vnd.arc.v1+json" http://localhost:26658/status
```

**Backwards Compatible (defaults to v1):**
```bash
curl http://localhost:26658/status
# or
curl -H "Accept: application/json" http://localhost:26658/status
```

**Unsupported Version:**
```bash
curl -H "Accept: application/vnd.arc.v99+json" http://localhost:26658/status
# Returns: 406 Not Acceptable with error details
```

#### Response Headers

All responses include a `Content-Type` header indicating the API version used:

```
Content-Type: application/vnd.arc.v1+json
```

#### Version Negotiation Rules

1. **Explicit versioned Accept header** → Uses that version if supported, otherwise returns `406 Not Acceptable`
2. **`Accept: application/json`** → Defaults to current version (v1)
3. **Missing Accept header** → Defaults to current version (v1)
4. **Unrecognized format** → Defaults to current version (v1) for backwards compatibility

#### Available Endpoints

All endpoints support versioning:

- `GET /` - API documentation and versioning info
- `GET /status` - Application status
- `GET /health` - Health check
- `GET /version` - Version information (git, cargo)
- `GET /consensus-state` - Current consensus state
- `GET /commit?height=N` - Commit certificate for specific height
- `GET /network-state` - Network peer information

#### Example API Usage

**Get Status:**
```bash
curl -H "Accept: application/vnd.arc.v1+json" http://localhost:26658/status | jq
```

**Get Commit Certificate:**
```bash
curl -H "Accept: application/vnd.arc.v1+json" \
  "http://localhost:26658/commit?height=100" | jq
```

**Get Health:**
```bash
curl http://localhost:26658/health
```

**Get API Documentation:**
```bash
curl http://localhost:26658/
```

### Deprecation Policy

When breaking changes are introduced:

1. A new API version (e.g., v2) will be released
2. The previous version (v1) will remain available with a deprecation notice
3. After a deprecation period, the old version may be removed in a major release
4. Clients will be notified via response headers and documentation

## Metrics

See [METRICS.md](./METRICS.md).

[malachite]: https://github.com/circlefin/malachite/
[reth]: https://reth.rs/
[engine-api]: https://github.com/ethereum/execution-apis/blob/main/src/engine/README.md
