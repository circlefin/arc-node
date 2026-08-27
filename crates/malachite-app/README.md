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
  - [Height Ranges](#height-ranges)
  - [Response Compression](#response-compression)
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
   --validator \
   --suggested-fee-recipient=0xYourAddressHere \
   --eth-socket=/tmp/reth.ipc \
   --execution-socket=/tmp/auth.ipc \
   --minimal
```

**Full example with IPC** (recommended for colocated reth and malachite):

```bash
arc-node-consensus start \
   --home=~/.arc/consensus \
   --moniker=validator-1 \
   --validator \
   --suggested-fee-recipient=0xYourAddressHere \
   --p2p.addr=/ip4/172.19.0.5/tcp/27000 \
   --p2p.persistent-peers=/ip4/172.19.0.6/tcp/27000,/ip4/172.19.0.7/tcp/27000 \
   --metrics=172.19.0.5:29000 \
   --rpc.addr=127.0.0.1:31000 \
   --eth-socket=/tmp/reth.ipc \
   --execution-socket=/tmp/auth.ipc \
   --minimal
```

> [!WARNING]
> **Deprecated.** The RPC transport is deprecated and will be removed in
> v0.9.0. Prefer the IPC example above. See
> [running-an-arc-node.md](../../docs/running-an-arc-node.md).

**Full example with RPC** (for remote deployments):

```bash
arc-node-consensus start \
   --home=~/.arc/consensus \
   --moniker=validator-1 \
   --validator \
   --suggested-fee-recipient=0xYourAddressHere \
   --p2p.addr=/ip4/172.19.0.5/tcp/27000 \
   --p2p.persistent-peers=/ip4/172.19.0.6/tcp/27000,/ip4/172.19.0.7/tcp/27000 \
   --metrics=0.0.0.0:29000 \
   --rpc.addr=172.19.0.5:31000 \
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
   --full
```

To run as a sync-only node that does not subscribe to consensus gossip topics, pass `--no-consensus`.

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
- `--validator` - Run as a validator: load the consensus signing key, sign the validator proof, and advertise a validator identity. Without this flag the node runs as a full node (no signing, ephemeral consensus key). Mutually exclusive with `--no-consensus` and `--follow`. Requires `--suggested-fee-recipient`.
- `--no-consensus` - Run as a sync-only node that does not subscribe to consensus gossip topics. Mutually exclusive with `--validator`.
- `--discovery` - Enable peer discovery (default: false)
- `--discovery.num-outbound-peers` - Number of outbound peers (default: 20)
- `--discovery.num-inbound-peers` - Number of inbound peers (default: 20)
- `--value-sync` - Enable value sync (default: true)
- `--metrics` - Enable metrics and set listen address (e.g., "0.0.0.0:29000")
- `--rpc.addr` - Enable RPC and set listen address (e.g., "127.0.0.1:31000")
- `--rpc.admin` - Enable the admin RPC routes (state-mutating persistent-peer add/remove). Disabled by default. These routes have no authentication, so they stay unreachable unless explicitly enabled, and **should only be exposed on an internal, trusted interface**.
- `--full` - Arc full-node pruning preset; sets `--prune.certificates.distance 237600`; mutually exclusive with `--minimal` and the individual `--prune.certificates.*` flags
- `--minimal` - Arc minimal-storage pruning preset; sets `--prune.certificates.distance 237600`; mutually exclusive with `--full` and the individual `--prune.certificates.*` flags
- `--prune.certificates.distance` - Keep certificates for the last N heights (default: 0, disabled/archive node); mutually exclusive with `--prune.certificates.before` and `--full/--minimal` presets
- `--prune.certificates.before` - Prune all certificates below this height (default: 0, disabled); mutually exclusive with `--prune.certificates.distance` and `--full/--minimal` presets
- `--log-level` - Log level: "trace", "debug", "info", "warn", "error" (default: "debug")
- `--log-format` - Log format: "plaintext" or "json" (default: "plaintext")
- `--pprof.addr` - Profiling server bind address (default: "0.0.0.0:6060")
- `--suggested-fee-recipient <ADDRESS>` - 20-byte address to receive tips and rewards. Required when `--validator` is set.
- `--follow` - Enable RPC sync mode. The node fetches blocks from trusted RPC endpoints instead of participating in consensus (requires `--follow.endpoint`)
- `--follow.endpoint <ENDPOINT>` - RPC endpoint to fetch blocks from in sync mode. Can be repeated. Format: `http://host:port[,ws=port]` (requires `--follow`)
- `--runtime.flavor` - Tokio runtime flavor: "single-threaded" or "multi-threaded" (default: "multi-threaded")
- `--runtime.worker-threads <COUNT>` - Number of worker threads for the multi-threaded runtime (default: number of CPU cores; ignored with single-threaded)
- `--private-key <PATH>` - Path to private validator key file. Used for P2P identity and (when not using `--signing.remote`) consensus signing. Default: `{home}/config/priv_validator_key.json`
- `--db.skip-upgrade` - Skip database schema upgrade on startup
- `--signing.remote` - Use remote signing with specified endpoint URL (if not provided, uses local signing). Requires `--validator`.
- `--signing.tls-cert-path` - Path to TLS certificate file for remote signing; auto-enables TLS (requires `--signing.remote`)

#### Remote Signing

For validator nodes that use a remote signing service instead of local private keys:

```bash
arc-node-consensus start \
   --home=~/.arc/consensus \
   --moniker=validator-1 \
   --validator \
   --suggested-fee-recipient=0xYourAddressHere \
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

If `--url` is omitted, the newest storage v2 entry that carries both layers is
selected for `--chain`, regardless of retention. The consensus archive from
that entry is fetched without falling back to the v1 listing. The current
selection is an archive snapshot measured at 42.29 GB, up from 15.37 GB for the
previous v1 pruned selection.

```bash
# Testnet latest snapshot
arc-node-consensus download \
  --home=~/.arc/consensus \
  --chain arc-testnet
```

> For a paired node restore, use the `arc-snapshots` tool. Automatic resolution
> restores a storage v2 execution manifest and its consensus archive from one
> listing entry. Explicit URLs can still select the native archive restore. See
> [`crates/snapshots/README.md`](../snapshots/README.md).

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

The consensus layer exposes a REST API for monitoring and querying consensus state when `--rpc.addr` is set (e.g., `--rpc.addr=127.0.0.1:26658`).

The read-only endpoints below are always available when the RPC server is enabled. The state-mutating admin routes (`POST`/`DELETE /persistent-peers`) are gated behind `--rpc.admin` and disabled by default: they are not served and not listed in the API index unless that flag is set. They have no authentication, so **only enable them on an internal, trusted interface**.

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
- `GET /ready` - Readiness probe (200 in sync, 503 catching up)
- `GET /version` - Version information (git, cargo)
- `GET /consensus-state` - Current consensus state
- `GET /network-state` - Network peer information
- `GET /commit?height=N[&count=C]` - Commit certificate(s) for a height or range
- `GET /misbehavior-evidence?height=N[&count=C]` - Misbehavior evidence for a height or range
- `GET /proposal-monitor?height=N[&count=C]` - Round-0 proposal monitoring data for a height or range
- `GET /invalid-payloads?height=N[&count=C]` - Invalid payloads for a height or range

The four observability endpoints (`/commit`, `/misbehavior-evidence`,
`/proposal-monitor`, `/invalid-payloads`) accept an optional `count` for range
queries — see [Height Ranges](#height-ranges).

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

### Height Ranges

The four observability endpoints — `/commit`, `/misbehavior-evidence`,
`/proposal-monitor`, and `/invalid-payloads` — accept an optional `count` query
parameter to fetch a contiguous forward range of heights in one request:

- `count` is the total number of heights **including** `height`. It defaults to
  `1`. `count=1` (or omitting `count`) is identical to the single-height
  behavior and returns a single JSON object.
- `count > 1` returns an ordered JSON **array** of the per-height object, for
  heights `height, height+1, …, height+count-1`. An explicit `height` is
  required; `count > 1` without `height` is a `400 Bad Request`.
- `count` is capped at **1000**. Requests above the cap are rejected with a
  `400 Bad Request` (see below). The limit is fixed, not configurable.

```bash
# A single height (unchanged):
curl "http://localhost:26658/commit?height=100" | jq

# 50 contiguous heights (100..=149) as a JSON array:
curl "http://localhost:26658/commit?height=100&count=50" | jq
```

#### Range Errors

When a range cannot be fully served, the endpoint returns `400 Bad Request`
with a structured body identifying the failing heights, uniform across all four
endpoints, e.g., for a request that exceeds the current head:

```json
{
  "error": "partial range unavailable",
  "requested": { "from": 100, "to": 149 },
  "failed_heights": [147, 148, 149],
  "reason": "above_current_head"
}
```

- `requested` — the inclusive `from..=to` range that was asked for.
- `failed_heights` — the exact heights that could not be served, so a client
  can retry only those. Omitted when empty.
- `reason` — one of:
  - `above_current_head` — the height is past the latest decided height.
  - `pruned` — the height is below the earliest retained height.
  - `not_recorded` — within range but never recorded (proposal monitor only; a
    permanent gap, not retryable).
  - `internal` — a record is missing or could not be decoded.
  - `over_limit` — `count` exceeds the 1000 cap. `failed_heights` is omitted
    (the range is never evaluated); `requested` still carries the bounds.

Plain argument errors (`count=0`, `count > 1` without `height`, or a range that
overflows `u64`) return a simpler `400 Bad Request` of the form `{"error": "..."}`.

When the store holds no decided heights yet (an empty table — e.g. a freshly
started node), a range request returns `404 Not Found` with a per-endpoint body
(e.g., `{"error": "Certificate not found"}` on `/commit`, and the analogous message on
the other three) rather than the structured `400` above.

### Response Compression

Responses are compressed when the client opts in via the standard
`Accept-Encoding` header; the server negotiates a codec the client offered and
sets `Content-Encoding` accordingly. The server supports `gzip`, `zstd`, `br`
(brotli), and `deflate`. Clients that send no `Accept-Encoding` receive
uncompressed responses unchanged.

```bash
# curl negotiates and transparently decompresses with --compressed:
curl --compressed "http://localhost:26658/commit?height=100&count=1000" | jq
```

Very small responses (under ~32 bytes, e.g. `/health`) are sent uncompressed
regardless.

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
