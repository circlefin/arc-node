# arc-snapshots

Download and extract Arc node snapshots.

## Overview

Arc node snapshots are separate `.tar.lz4` archives for the execution layer (EL) and consensus layer (CL):

| Archive | Contents |
|---------|----------|
| Execution (`*-execution-*.tar.lz4`) | `db/`, `db/mdbx.dat`, `db/mdbx.lck`, `db/database.version` |
| Consensus (`*-consensus-*.tar.lz4`) | `store.db` |


## `arc-snapshots` CLI

Download both EL and CL snapshots and extract them to their respective data directories.
The latest snapshot URLs are fetched automatically from the API when `--chain` is provided:

```bash
# Testnet
arc-snapshots download --chain arc-testnet

# Devnet
arc-snapshots download --chain arc-devnet
```

To use specific snapshot URLs instead of auto-fetching, provide both explicitly. `--chain` is then optional:

```bash
arc-snapshots download \
  --execution-url <el-snapshot-url> \
  --consensus-url <cl-snapshot-url>
```

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `--execution-url` | URL of the EL snapshot archive | auto-fetched from snapshot API when `--chain` is set |
| `--consensus-url` | URL of the CL snapshot archive | auto-fetched from snapshot API when `--chain` is set |
| `--chain` | Network: `arc-testnet`, `arc-devnet` | none (required only when `--execution-url`/`--consensus-url` are omitted) |
| `--execution-path` | Directory for EL data | `~/.arc/execution` |
| `--consensus-path` | Directory for CL data | `~/.arc/consensus` |
