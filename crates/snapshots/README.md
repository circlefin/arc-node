# arc-snapshots

Download and restore Arc node snapshots.

## Overview

An Arc node stores two data layers, execution (EL) and consensus (CL), and
`arc-snapshots` restores both.

Published snapshots use reth's storage v2 format. Rather than shipping the
execution layer as one compressed archive, it publishes a manifest listing each
database component on its own, which is what lets `--el-profile` fetch part of a
snapshot instead of all of it. The consensus layer is still a single `.tar.lz4`
archive. The rest of this document simply calls these snapshots.

Automatic resolution always selects an execution manifest and its matching
consensus archive. Explicit URLs can use either of the restore styles below. The
execution artifact chooses the style:

- **Manifest** — when the execution snapshot is a reth manifest (a URL whose
  last path segment is `manifest.json`), the execution layer is downloaded by
  handing off to `arc-node-execution download`, and the consensus layer is
  restored from a `.tar.lz4` archive. This needs the `arc-node-execution`
  binary (see [Execution binary](#execution-binary)).
- **Archive** — when the execution snapshot is a single `.tar.lz4` archive,
  both layers are restored by arc-snapshots itself, with no dependency on
  `arc-node-execution`.

When URLs are resolved automatically, the tool selects the newest published
entry that carries both layers, regardless of retention. This skips a newer
entry whose consensus upload has not finished in favor of an older usable one.
The chosen block is logged.

The native `.tar.lz4` archives contain:

| Archive | Contents |
|---------|----------|
| Execution (explicit `--execution-url` only) | `db/` and `static_files/` |
| Consensus (`consensus.tar.lz4`) | `store.db` |

## `arc-snapshots` CLI

Restore both layers, resolving the latest snapshot for the chain automatically:

```bash
arc-snapshots download --chain arc-testnet --el-profile full
```

Supported chains: `arc-testnet`, `arc-devnet`, `arc-mainnet`.

Automatic resolution requires the API to publish a storage v2 listing. A
deployment that omits it fails with an error naming the missing `v2Snapshots`
field. An empty listing, or one with no complete entry for the selected chain,
also fails. There is no fallback to any other listing the API serves.

To point at specific snapshots instead of resolving them, pass both URLs. The
flags are all-or-nothing because both snapshots must be from the same block. A
consensus snapshot from a different block leaves the node unable to hand off
between the layers, which appears as slow syncing rather than an error.

```bash
arc-snapshots download \
  --chain arc-testnet \
  --el-profile full \
  --execution-url <el-snapshot-url> \
  --consensus-url <cl-snapshot-url>
```

### Execution profiles (manifest downloads)

For a manifest download, `--el-profile` chooses how much execution-layer
history to fetch:

- `minimal`: state, all headers, and a small recent window. Suits validators
  and sentries.
- `full`: adds full transaction, receipt, and changeset history. Suits follow
  nodes.
- `archive`: every component, including transaction senders and rocksdb
  indices.

The flag defaults to `minimal`, including when an explicit `--execution-url`
names a manifest. Pass the profile that matches the pruning preset the node will
run with: `--el-profile full` for a node started with `--full`. An archive node
must pass `--el-profile archive`; omitting it produces a minimal restore.

A single-archive execution snapshot ignores `--el-profile`.

Automatic manifest URLs use the API's query-free
`{base}/download/{manifestKey}` form. Do not supply a presigned manifest URL by
hand. Reth derives every component URL from the manifest URL and preserves its
query string, so the manifest's signature would be attached to each component
request and those requests would return 404.

### Execution binary

For a manifest download, `arc-snapshots` runs `arc-node-execution` to download
the execution layer. It looks for `arc-node-execution` on `PATH` by default; set
`ARC_EXECUTION_BINARY` to use a different name or an absolute path. An archive
restore needs no execution binary.

Before deleting anything, a manifest restore checks that the binary can perform
the download it is about to be asked for. A binary that is missing, too old, or
otherwise unusable fails the restore with the existing data still in place.

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `--el-profile` | Execution profile for a manifest download: `minimal`, `full`, or `archive` | `minimal` |
| `--chain` | Network: `arc-testnet`, `arc-devnet`, `arc-mainnet` | none |
| `--execution-url` | Explicit execution snapshot URL: `manifest.json` selects the manifest restore, anything else selects the archive restore | supply both URLs or resolve a pair from `--chain` |
| `--consensus-url` | Consensus `.tar.lz4` archive URL | supply both URLs or resolve both from `--chain` |
| `--execution-path` | Directory for EL data | `~/.arc/execution` |
| `--consensus-path` | Directory for CL data | `~/.arc/consensus` |
| `--force` | Replace both layers if their current data would otherwise block the restore | disabled |

`--chain` is required whenever a snapshot URL is omitted, since it resolves the
latest complete entry from the API. It is also required whenever the
execution snapshot is a manifest, since it is passed to
`arc-node-execution` to select the chainspec.

Listing tests use wiremock rather than the live service. The live response is
edge cached for five minutes and rate limited at roughly thirty requests in two
minutes, so it cannot provide deterministic fixtures or a reliable test loop.

### Restore behavior

Each layer records which snapshot it holds in a `.snapshot-url` marker file,
written only once the restore has finished. That marker, not the contents of the
directory, decides what a run does:

| Target | Result |
|--------|--------|
| Empty | restore |
| Marker names the requested snapshot | nothing to do |
| Marker names a different snapshot | restore |
| Data present, no marker | **error** — pass `--force` to replace it |

`--force` restores both layers whatever their markers say. It does not change
which snapshot the API resolver selects.

The last row is an error rather than a skip or a restore because two states look
identical there, and they need opposite treatment. It may be a node that synced
from genesis or a validator that has been signing since `arc-node-consensus init`
— deleting that unasked is not something a snapshot can undo, and rewinding a
consensus store below the heights it has already voted at is worse than expensive.
Or it may be a restore that died before writing its marker, in which case the
files are part of a snapshot and skipping would report success over them. Nothing
on disk separates the two, so the run stops and the operator decides with
`--force`.

Rows three and four differ for the same reason. A marker means the tool wrote
what is there and knows exactly what it is, so replacing one snapshot with
another costs only the download.

Two details about what the marker records.

A URL's signature is not part of it, so a re-signed pre-signed URL
does not read as a new snapshot. Every other query parameter is kept, and sorted
so their order does not matter. That is deliberate: a parameter like
`?network=arc-devnet` is part of what the URL addresses, and dropping it would
give two chains' snapshots one identity, so restoring either would report the
other as up to date. An unrecognised parameter is kept for the same reason — a
needless re-download is recoverable, a datadir holding the wrong chain is not.

And for a manifest restore the execution marker records the `--el-profile`
alongside the URL, so re-running with a different profile counts as a different
snapshot and fetches the new component set.

#### What a restore leaves behind

Within one layer, a restore does not leave files from an older snapshot beside
the new one. The layers are restored in sequence, though, so an interrupted run
can leave them at different stages. Markers make that detectable rather than
self-healing: a layer left holding data with no marker is not overwritten by a restore
unless the operator passes `--force`.

How a layer avoids mixing differs by layer, because their snapshots differ in
shape:

- **Execution** — the directory is removed first. Extraction writes the files the
  snapshot names and deletes nothing else, so anything the incoming snapshot does
  not name survives: `static_files/` jars covering block ranges the restored
  database has no checkpoints for, a `rocksdb/` left by an earlier
  `--el-profile archive` restore, or a stale `reth.toml`. The last of those is the
  worst, since it carries the pruning configuration and reth will not overwrite a
  `reth.toml` that is already there.
- **Consensus** — the directory is not removed. Its snapshot is the single file
  `store.db`, which extraction replaces outright, so there is nothing to clean
  up. It is also the consensus node's home directory: removing it would take
  `config/` and the validator's private key, which no snapshot restores. It would
  also take `wal/consensus.wal`, and that file is wanted. Malachite wipes the WAL
  whenever the height recorded in it differs from the height the node starts at,
  which is what normally happens after a restore. When the two match — the node
  had already started that height and died partway through it — the log is
  replayed instead, and replaying it is what makes the node re-cast the vote it
  cast before rather than a different one. The WAL is not a record of signatures
  already sent; it is every message the node took in at that height, and feeding
  those back rebuilds the state that produced the vote.

That covers a crash, not a rewind. Restoring a validator to a snapshot below a
height it has already voted at gets no protection from the WAL, since a log
recorded at a *higher* height is discarded just as quietly as a stale one. The
error on unmarked data is what stops that restore: a node that has been running
has no marker, so the run refuses to touch it until an operator passes `--force`
and accepts the consequence.

Because that directory survives, its marker is deleted before extraction starts
rather than left to be overwritten at the end. Otherwise a restore that failed
partway through `store.db` would leave a marker claiming the store is intact — and
when the snapshot being restored is the one already named there, as on a `--force`
retry, the next run would read a truncated store as up to date.

The asymmetry also decides what a failed download costs. An archive is downloaded
in full before anything is touched, so a failure changes nothing. A manifest
restore has no staging step — `arc-node-execution` downloads straight into the
datadir — so once it starts, the previous execution data is gone whether or not it
finishes. A run interrupted there leaves data and no marker, which is the error
row above: the next run stops and asks for `--force`.
