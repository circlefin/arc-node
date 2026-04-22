# Arc Engine Bench

Inspired by [`reth-bench`](https://github.com/paradigmxyz/reth/tree/main/bin/reth-bench) from the
upstream Reth project.

`arc-engine-bench` replays historical blocks into an Arc execution node and measures Engine API import
latency. The current benchmark mode, `new-payload-fcu`, submits each block with
`engine_newPayloadV4`, follows it with `engine_forkchoiceUpdatedV3`, and writes CSV artifacts for
per-block latency and aggregate throughput.

## CLI

| Command | Description |
| --- | --- |
| `arc-engine-bench prepare-payload` | Fetches a contiguous source block range and writes a local payload fixture directory with `genesis.json`, `metadata.json`, and `payloads.jsonl`. |
| `arc-engine-bench new-payload-fcu` | Replays a prepared payload fixture into a target execution node with `engine_newPayloadV4` followed by `engine_forkchoiceUpdatedV3`. |

## What You Need

- A running `arc-node-execution` instance to benchmark. This is the **target** node.
- A payload fixture directory containing `genesis.json`, `metadata.json`, and `payloads.jsonl` for
  the block range you want to replay.
- A source RPC endpoint with the historical blocks you want to replay. This is only needed when you
  run `prepare-payload` to create or refresh a fixture.
- The target must already be at block `FROM_BLOCK - 1`. The benchmark verifies that the target head
  matches the fixture metadata before replay starts and exits if it does not.
- The target Engine API must be reachable via **IPC** or **authenticated RPC**. Pass either
  `--engine-ipc <PATH>` or `--engine-rpc-url <URL> --jwt-secret <PATH>` (mutually exclusive).

## Example Environment

Copy this block, adjust it for your setup, then `source` it before running the commands below:

`BENCH_DATADIR` is the directory where the target node snapshot lives.

```bash
BENCH_DATADIR=datadir/bench-target
TARGET_ETH_RPC_URL=http://127.0.0.1:7545
SOURCE_RPC_URL=http://127.0.0.1:8545
HTTP_PORT=7545
METRICS_PORT=19001
FROM_BLOCK=1
TO_BLOCK=3000
PAYLOAD_DIR=target/engine-bench/payload-fixture
CHAIN=arc-localdev
# ipc
ENGINE_IPC="$BENCH_DATADIR/reth.ipc"
# rpc
ENGINE_RPC_URL=http://127.0.0.1:7551
AUTHRPC_PORT=7551
```

## Prepare the Target Node

The target node must start at the parent of the first replayed block.

- If `FROM_BLOCK=1`, you can start from a fresh datadir.
- If `FROM_BLOCK>1`, you need the target node at block `FROM_BLOCK - 1` before replay. In
  practice, that means either:
  - prepare a snapshot at the desired height, or
  - sync the node past that height and unwind it back to `FROM_BLOCK - 1`.

### 1. Create a datadir and JWT secret

```bash
mkdir -p "$BENCH_DATADIR"
# Only needed for RPC transport:
openssl rand -hex 32 | tr -d '\n' > "$BENCH_DATADIR/jwt.hex"
chmod 600 "$BENCH_DATADIR/jwt.hex"
```

### 2. Unwind the target to the replay parent block

Skip this step when `FROM_BLOCK=1`. If you synced the node past the replay start, stop it before running the unwind command:

```bash
arc-node-execution stage unwind \
  --chain "$CHAIN" \
  --datadir "$BENCH_DATADIR" \
  to-block "$((FROM_BLOCK - 1))"
```

### 3. Start the target node

**IPC transport:**

```bash
arc-node-execution node \
  --chain "$CHAIN" \
  --datadir "$BENCH_DATADIR" \
  --dev \
  --disable-discovery \
  --http \
  --http.api=eth \
  --http.port "$HTTP_PORT" \
  --metrics 127.0.0.1:"$METRICS_PORT" \
  --auth-ipc \
  --auth-ipc.path "$ENGINE_IPC" \
  --arc.denylist.enabled
```

**RPC transport:**

```bash
arc-node-execution node \
  --chain "$CHAIN" \
  --datadir "$BENCH_DATADIR" \
  --dev \
  --disable-discovery \
  --http \
  --http.api=eth \
  --http.port "$HTTP_PORT" \
  --metrics 127.0.0.1:"$METRICS_PORT" \
  --authrpc.addr=127.0.0.1 \
  --authrpc.port="$AUTHRPC_PORT" \
  --authrpc.jwtsecret="$BENCH_DATADIR/jwt.hex" \
  --arc.denylist.enabled
```

## Prepare the Payload Fixture

Fetch source blocks `FROM_BLOCK..=TO_BLOCK` once and write them to a local fixture directory:

```bash
arc-engine-bench prepare-payload \
  --chain "$CHAIN" \
  --source-rpc-url "$SOURCE_RPC_URL" \
  --from "$FROM_BLOCK" \
  --to "$TO_BLOCK" \
  --output-dir "$PAYLOAD_DIR"
```

Other flags:

- `--chain <NAME_OR_PATH>` sets the chain spec used to record genesis config. Accepts built-in
  names (`arc-localdev`, `arc-devnet`, `arc-testnet`) or a path to a genesis JSON file. The default
  is `arc-localdev`.
- `--eth-rpc-timeout-ms <MILLISECONDS>` sets the timeout for source Ethereum RPC requests. The
  default is `10000` ms. Batch requests use the larger of this value or 30 seconds.
- `--batch-size <N>` controls source RPC fetch batching. The default is `20`.

The fixture directory contains:

| File | Content |
| --- | --- |
| `genesis.json` | Chain genesis configuration (chain ID, hardfork activations, initial state). |
| `metadata.json` | Replay metadata including `from_block`, `to_block`, `payload_count`, and the expected parent block. |
| `payloads.jsonl` | One `ExecutionPayloadV3` JSON document per line, ordered by block number. |

## Run `new-payload-fcu`

Replay the prepared fixture into the target node:

**IPC transport:**

```bash
arc-engine-bench new-payload-fcu \
  --engine-ipc "$ENGINE_IPC" \
  --target-eth-rpc-url "$TARGET_ETH_RPC_URL" \
  --payload "$PAYLOAD_DIR"
```

**RPC transport:**

```bash
arc-engine-bench new-payload-fcu \
  --engine-rpc-url "$ENGINE_RPC_URL" \
  --jwt-secret "$BENCH_DATADIR/jwt.hex" \
  --target-eth-rpc-url "$TARGET_ETH_RPC_URL" \
  --payload "$PAYLOAD_DIR"
```

Other flags:

- `--output <DIR>` writes artifacts to an explicit directory. By default, output goes to
  `target/engine-bench/new-payload-fcu-<timestamp>/`.
- `--eth-rpc-timeout-ms <MILLISECONDS>` sets the timeout for target Ethereum RPC requests. The
  default is `10000` ms.

## Live Metrics

From the repo root, start the monitoring stack:

```bash
docker compose -f deployments/monitoring.yaml up -d
```

The bundled Prometheus config includes an `arc_engine_bench_target` scrape job that reads the
benchmark target from `host.docker.internal:19001`.

Open Grafana at `http://127.0.0.1:3000`, then open the provisioned `Reth` dashboard and select the
`arc_engine_bench_target` instance.

## Output Artifacts

Each run writes to `target/engine-bench/<mode>-<YYYYMMDDTHHMMSSZ>/` unless you pass `--output`.

| File | Content |
| --- | --- |
| `combined_latency.csv` | One row per replayed block with block metadata, `new_payload_ms`, `fcu_ms`, `total_ms`, per-block throughput, and cumulative throughput. |
| `summary.csv` | One-row summary with sample count, total gas and txs, wall-clock time, average throughput, and latency percentiles. |
