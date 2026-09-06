# Scan historical logs with a resumable client

Wide `eth_getLogs` queries can exceed a provider's range, response-size, or query-time limits. [Issue #83](https://github.com/circlefin/arc-node/issues/83) describes this problem when indexing Arc Testnet. A successful smaller request does not establish a universal provider limit: the appropriate range also depends on the filter and event density.

[`scripts/scan_logs.py`](../scripts/scan_logs.py) is a Python 3.10+ standard-library reference client. It scans one address in sequential, bounded ranges and stores raw logs and progress in SQLite. No node installation, wallet, private key, or third-party Python package is required. This is a client-side workaround; it does not change the RPC server's limits or error handling.

## Run a bounded scan

Use a known deployment block as the start of a historical scan. Avoid starting at genesis unless you actually need the entire history. For a first test, choose a short interval that already exists on your network.

```sh
export ARC_RPC_URL="https://rpc.testnet.arc.network"

# Example historical testnet interval; choose the range you need.
python3 scripts/scan_logs.py \
  --address 0xfffffffffffffffffffffffffffffffffffffffe \
  --topics '["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"]' \
  --from-block 60700000 \
  --to-block 60700049 \
  --db /tmp/arc-native-transfers.sqlite
```

The example filters native USDC `Transfer` logs. It retains the RPC's raw values; it does not decode amounts, reconcile balances, or combine native and ERC-20-interface events. See [Index Arc Events](https://docs.arc.io/integrate/infrastructure/indexing-events) for the emitter, precision, and historical event-format distinctions.

Use `--rpc-url` to override `ARC_RPC_URL`. Prefer the environment variable when a provider URL contains credentials. The client does not persist the URL in the database or include it in error messages.

`--topics` accepts the JSON-RPC positional topic filter: a topic hash, `null` wildcard, or an array of alternative hashes at each position. Omitting it requests all events for the selected address. Block arguments are decimal integers, and both ends of the range are inclusive.

## Resume after interruption

Run the same command with the same database. Each successful batch commits its logs and the next block together. If the process stops before a commit, the batch is fetched again; identical log identities are stored only once. Empty successful batches advance progress, while failed requests never do.

Omitting `--to-block`, or passing `--to-block latest`, freezes the end block on the first run. A resumed scan continues toward that stored end; it does not chase a moving chain head. A completed scan is a no-op when resumed with the same range. Use a new database for a different range or filter.

The database binds the scan to its chain ID, address, topics, starting block, ending block, and ending block hash. Initialization requires the ending block to exist. Resume checks that the provider still returns the same ending block hash, so another provider can serve the same scan but a lagging provider or a different chain cannot silently turn missing blocks into empty results.

Only one scanner should use a database at a time. If another process has already advanced the cursor, the stale writer fails without committing its batch. Keep the database on a local filesystem with working SQLite locking.

## Limits and failure behavior

- `--chunk-size` defaults to 10,000 blocks, an initial batch size inspired by the issue's reported workaround, not an official Arc RPC limit. Lower it for dense event streams.
- Explicit range or response-size limits cause the client to reduce the batch size and retry from the same block. The reduced size is retained for subsequent batches in that run.
- `--timeout` defaults to 15 seconds. `--retries` defaults to three retries with bounded backoff for transient failures. Rate limits and ordinary transport failures retry the same range; they do not cause an unbounded fan-out of smaller requests.
- A query timeout may require a smaller range. Authentication failures, invalid requests, malformed responses, and invalid logs stop the scan. A single-block query that still exceeds a limit also stops rather than skipping that block.
- Unavailable pruned history is not a range-size problem. For example, a provider may return `4444: pruned history unavailable`; use a provider that retains the requested history. Splitting that request cannot recover missing data.
- Responses are bounded in memory. Logs are stored batch by batch, rather than collecting the complete history in memory.

Progress goes to stderr; a successful run prints a JSON summary to stdout. On failure, address the reported cause and rerun with the same database. Tuning the chunk size, timeout, or retry budget does not change the stored filter or range.

## Read the stored logs

The `logs` table contains `block_number`, `transaction_index`, `log_index`, `block_hash`, `transaction_hash`, and `data` (the normalized JSON log). Order by block, transaction, and log index; sub-second blocks can share a timestamp.

For example, export the example database as JSON Lines without opening a writable connection:

```sh
python3 - <<'PY'
import sqlite3

with sqlite3.connect("file:/tmp/arc-native-transfers.sqlite?mode=ro", uri=True) as db:
    for (log,) in db.execute(
        "SELECT data FROM logs ORDER BY block_number, transaction_index, log_index"
    ):
        print(log)
PY
```

Deduplication uses `(blockHash, transactionHash, logIndex)`, not just transaction hash. Multiple events in the same transaction remain distinct. This is event-level deduplication, not business-level deduplication of multiple events that represent the same USDC movement.

## Scope

This example assumes Arc's finalized history. It is not a general-purpose reorg-aware Ethereum indexer, a live subscription service, or a cryptographic verifier. The end-block check detects a changed anchor; it does not prove the completeness of a provider's successful response. Use a provider with the historical data you require, and independently reconcile results before relying on them for accounting.

The SQLite commit guarantees apply only to this local store. Delivering rows to another system needs its own durable cursor and idempotency handling. There is no automatic database migration or network-reset recovery; retain the old database and start a new scan when those assumptions change.

## Run the tests

The tests use deterministic fixtures and a local HTTP server; they do not call a public RPC endpoint.

```sh
python3 -m unittest discover -s scripts -p 'test_scan_logs.py' -v
```
