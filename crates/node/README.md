# arc-node-execution

This is a [Reth][reth]-based execution layer (EL) implementation customized for the Arc Network.

It serves as the execution client that processes transactions, executes smart contracts, and maintains the blockchain state. Communication with the consensus layer (CL) is handled via the [Engine API][engine-api].

## Table of Contents

- [Usage](#usage)
  - [a) Full Node](#a-full-node)
  - [b) Unsafe RPC Node](#b-unsafe-rpc-node)
  - [c) RPC Node (with consensus verification)](#c-rpc-node-with-consensus-verification)
  - [CLI Flags](#cli-flags)
  - [Custom flags](#custom-flags)
  - [Init](#init)
  - [Database Commands](#database-commands)
- [Invalid Transaction List](#invalid-transaction-list)
- [Pending Txs Filter](#pending-txs-filter)
- [Architecture](#architecture)
- [Metrics](#metrics)
- [Development](#development)
  - [Running Without Consensus (Mock Mode)](#running-without-consensus-mock-mode)
- [Further Reading](#further-reading)

## Usage

### a) Full Node

**Minimal example with IPC** (recommended for colocated execution and consensus):

```bash
arc-node-execution node \
  --chain=assets/localdev/genesis.json \
  --http --http.port=8545 \
  --ipcpath=/tmp/reth.ipc \
  --full
```

**Full example with detailed configuration**:

```bash
arc-node-execution node \
  --chain=assets/localdev/genesis.json \
  --datadir=/var/lib/arc-execution \
  --http --http.addr=0.0.0.0 --http.port=8545 \
  --http.api=eth,net,web3,txpool,trace,debug \
  --http.corsdomain="*" \
  --ws --ws.addr=0.0.0.0 --ws.port=8546 \
  --authrpc.addr=0.0.0.0 --authrpc.port=8551 --authrpc.jwtsecret=jwtsecret \
  --ipcpath=/tmp/reth.ipc \
  --metrics=0.0.0.0:9001 \
  --enable-arc-rpc \
  --full
```

Note: to generate a JWT (JSON web token), use the following command:

```bash
openssl rand -hex 32 | tr -d "\n" > "jwtsecret"
```

### b) Unsafe RPC Node

Use RPC nodes to interact with the Arc network via API. While they don't participate in consensus, they stay fully synced with the latest state.

In this mode, consensus data is not verified. It is essential to only follow and synchronize with a trusted node.

```bash
# Download snapshot (this will help you sync much faster)
arc-node-execution download

arc-node-execution node \
  --unsafe-follow \
  --http --http.port=8545 \
  --http.api eth,net,web3,txpool,trace \
  --enable-arc-rpc \
  --minimal
```

> **Note:** When running a node in RPC mode (as shown above), you do **not** need to run or download the consensus binary. The execution node will follow the network without participating in consensus.

### c) RPC Node (with consensus verification)

For an RPC node with full consensus verification, run both the execution and consensus
layers together. The `--follow` and `--follow.endpoint` flags are configured on
`arc-node-consensus`, not this binary. See the [Consensus Layer README](../malachite-app/README.md)
for details on setting up a verified full node.

---

#### CLI Flags

For a complete list of available flags, see the [Reth CLI reference](https://reth.rs/cli/reth/node/) or run:

```bash
arc-node-execution node --help
```

#### Custom flags

In addition to standard Reth flags, `arc-node-execution` provides the following custom flags:

| Flag | Default | Environment Variable | Description |
|------|---------|---------------------|-------------|
| `--enable-arc-rpc` | `false` | - | Enable custom ARC RPC namespace (certificates, etc.) |
| `--arc-rpc-upstream-url <URL>` | - | `ARC_RPC_UPSTREAM_URL` | Upstream malachite-app base URL for ARC RPC (e.g., `http://127.0.0.1:31000`). Only read if `--enable-arc-rpc` is set |
| `--unsafe-follow [URL]` | - | `ARC_UNSAFE_FOLLOW_URL` | Run an RPC node (unsafe - no verification). Use without value for auto-config or specify WebSocket URL (e.g., `ws://trusted-node:8546`) |
| `--invalid-tx-list-enable` | `false` | - | Enable the invalid transaction list feature |
| `--invalid-tx-list-cap <CAPACITY>` | `100000` | - | Maximum capacity of the invalid tx list LRU cache. Only read if `--invalid-tx-list-enable` is set |
| `--full` | - | - | Full-node pruning preset. Fully prunes sender recovery; keeps the last 237,600 blocks for all other segments. Also sets `--prune.block-interval=5000`. Mutually exclusive with `--minimal`. |
| `--minimal` | - | - | Minimal-storage pruning preset. Fully prunes sender recovery; keeps transaction lookup for 64 blocks, receipts for 64 blocks, account/storage history for 10,064 blocks, and block bodies for 237,600 blocks. Also sets `--prune.block-interval=5000`. Mutually exclusive with `--full`. |
| `--arc.hide-pending-txs` | `false` | - | Hide pending-tx RPCs. When set, a filter blocks pending-tx subscriptions, filters, and pending block queries (see [Pending Txs Filter](#pending-txs-filter)). |

**Examples:**

Enable ARC RPC namespace:

```bash
arc-node-execution node \
  --enable-arc-rpc \
  --arc-rpc-upstream-url http://localhost:31000 \
  --chain genesis.json
```

Enable invalid transaction list with custom capacity:

```bash
arc-node-execution node \
  --invalid-tx-list-enable \
  --invalid-tx-list-cap 50000 \
  --chain genesis.json
```

### Init

Initialize the database from a genesis file:

```bash
arc-node-execution init --chain=assets/localdev/genesis.json
```

This creates the genesis block and initializes the state database.

### Database Commands

The `db` command provides database maintenance and debugging utilities.

For available database operations, run:
```bash
arc-node-execution db --help
```

## Invalid Transaction List

The node includes an optional in-memory invalid transaction list (LRU) used to proactively reject known-bad transaction hashes and to add all currently pending transactions to the list in the event the payload builder panics.

**Configuration:**

Use the `--invalid-tx-list-enable` and `--invalid-tx-list-cap` flags (see Custom flags section above).

**Behavior when enabled:**
- On payload builder panic, all pending transactions are added to the invalid tx list and removed from the mempool
- O(1) hash membership check during transaction validation
- Metrics exposed: `arc_invalid_tx_list_size`, `arc_invalid_tx_list_hits_total`, `arc_invalid_tx_list_inserts_total`, `arc_invalid_tx_list_batch_inserts_total`

**Behavior when disabled (default):**
- No invalid tx list is created
- No metrics are exposed
- On payload builder panic, no action is taken

**Example:**
```bash
arc-node-execution node \
  --chain genesis.json \
  --invalid-tx-list-enable \
  --invalid-tx-list-cap 10000
```

**Operational Notes:**
- Enable only on validator nodes
- Setting `--invalid-tx-list-cap 0` keeps the invalid tx list logically enabled (metrics + panic handling) but stores no hashes

## Pending Txs Filter

By default, the node allows all pending-tx RPCs. Pass `--arc.hide-pending-txs` to enable the filter that blocks them (for externally-exposed nodes).

**When the filter is enabled (`--arc.hide-pending-txs`):**

| Method or call | Behavior |
|----------------|----------|
| `eth_subscribe("newPendingTransactions")` | Error -32001 |
| `eth_newPendingTransactionFilter` | Error -32001 |
| `eth_getBlockByNumber("pending")` | Returns `null` (success) |

## Architecture

The execution layer is built on top of [Reth][reth], extending it with Arc-specific functionality:

- **Custom Precompiles** - Native implementations for Arc-specific operations (native coin control, post-quantum signatures, system accounting)
- **Custom EVM Configuration** - Specialized gas calculations and execution logic
- **Transaction Pool Enhancements** - Denylist support and custom validation
- **Block Executor** - Optimized block execution with Arc-specific features

For architectural details, see the [Architecture Guide](../../docs/ARCHITECTURE.md).

## Metrics

The execution layer exposes Prometheus metrics on the configured metrics endpoint (e.g., `http://localhost:9001/metrics`).

Key metric prefixes:
- `reth_*` - Core Reth metrics (block processing, sync, etc.)
- `arc_*` - Arc-specific metrics (precompiles, invalid tx list, etc.)

## Development

### Running Without Consensus (Mock Mode)

For execution-layer-only development and testing:

```bash
./scripts/localdev.mjs start
```

This runs the execution layer with a mock consensus layer for rapid iteration.

## Further Reading

- [Main README](../../README.md) - Getting started and development workflow
- [Architecture Guide](../../docs/ARCHITECTURE.md) - System design and component interactions
- [Consensus Layer README](../malachite-app/README.md) - Consensus layer documentation
- [Reth Book][reth-book] - Upstream Reth documentation

[reth]: https://github.com/paradigmxyz/reth
[reth-book]: https://paradigmxyz.github.io/reth/
[engine-api]: https://github.com/ethereum/execution-apis/blob/main/src/engine/README.md
