# Running an Arc Node

Arc is an open, EVM-compatible Layer-1 blockchain. Anyone can run an Arc node — no permission required. Running your own node gives you independent verification of the chain and direct API access to the network.

## What Your Node Does

- **Verifies every block** — Every block is cryptographically verified against the signatures of the validator set before it is accepted. Your node independently confirms that validators finalized each block;
- **Executes every transaction** — Every transaction is re-executed locally through the EVM. Your node maintains its own copy of the complete blockchain state;
- **Exposes a local RPC endpoint** — Your node provides a standard Ethereum JSON-RPC API (`http://localhost:8545`) for querying blocks, balances, and transactions, and for submitting calls directly against your own verified state.

## Quick Start

An Arc node is composed of two processes:

- **Execution Layer (EL)**: executes finalized transactions and maintains the state of the blockchain;
- **Consensus Layer (CL)**: fetches finalized blocks, verifies their cryptographic signatures, and passes them to the EL for execution.

Refer to the [installation](installation.md) instructions to install
`arc-node-execution` (EL) and `arc-node-consensus` (CL).

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

# Linux runtime directory:
ARC_RUN="/run/arc"

# Mac OS runtime directory:
#ARC_RUN="$ARC_HOME/run"

ARC_EXECUTION=$ARC_HOME/execution
ARC_CONSENSUS=$ARC_HOME/consensus
EOF
```

Source it to load these variables into your current shell session:

```sh
source ~/.arc_env
```

Or using the POSIX shorthand: `. ~/.arc_env`

### Setup directories

The standard installation sets up `$ARC_HOME=~/.arc` as base directory.
Create the **data directories** for the execution and consensus layers:

```sh
mkdir -p $ARC_EXECUTION $ARC_CONSENSUS
```

To set up the **runtime directory** in a **Linux** environment:

```sh
sudo install -d -o $USER "$ARC_RUN"
```

> When running Arc as a systemd service, `RuntimeDirectory=arc`
> sets up `/run/arc` automatically — the last command is not needed.

To set up the **runtime directory** in a **MacOS** environment,
uncomment the `ARC_RUN="$ARC_HOME/run"` line above and run:

```sh
mkdir -p "$ARC_RUN"
```

### Download snapshots

Syncing a new Arc node from genesis is currently not supported.
A **snapshot** is needed to bootstrap the node:

```sh
arc-snapshots download \
  --chain=arc-testnet \
  --execution-path "$ARC_EXECUTION" \
  --consensus-path "$ARC_CONSENSUS"
```

The `arc-snapshots` binary is part of the Arc node installation.
The command above fetches the latest snapshots for `arc-testnet` chain from
https://snapshots.arc.network and extracts them into the
`$ARC_CONSENSUS` and `$ARC_EXECUTION` data directories.

> **Download sizes:** At the time of writing, the most recent snapshot sizes
> (tagged `20260408`) are: **~68 GB** for EL and **~16 GB** for CL.
> These are the sizes of the downloaded compressed snapshots; when extracted,
> the sizes are ~103 GB for EL and ~36 GB for CL.
>
> On a fast connection (~100 Mbps) the download takes roughly 10-15 minutes;
> on slower or metered connections it can take hours.

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
  --rpc.forwarder https://rpc.quicknode.testnet.arc.network/ \
  --metrics 127.0.0.1:9001 \
  --disable-discovery \
  --enable-arc-rpc
```

> **Note on `--full` and snapshots:** The `--full` flag is required on the
> first start when bootstrapping from a pruned snapshot. It reconciles internal
> database tables that would otherwise fail a consistency check. After the
> initial startup completes, you may restart without `--full` if you prefer to
> run without pruning.

The `--chain` parameter configures the genesis file.
By using `--chain arc-testnet`, the genesis configuration bundled in the binary is adopted.
Replace with `--chain /path/to/genesis.json` if you have a custom genesis file.

The `--http`, `--http.addr`, and `--http.port` parameters expose a standard Ethereum
[JSON-RPC API](https://reth.rs/jsonrpc/intro).
The `--http.api` parameter defines the available RPC endpoints.
The `--rpc.forwarder` parameter routes requests not served locally to an existing RPC node.

The `arc-node-execution` binary accepts all parameters of a `reth` node.
Refer to its [documentation](https://reth.rs/cli/reth/node/) for details.

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
  --follow.endpoint https://rpc.drpc.testnet.arc.network,wss=rpc.drpc.testnet.arc.network \
  --follow.endpoint https://rpc.quicknode.testnet.arc.network,wss=rpc.quicknode.testnet.arc.network \
  --follow.endpoint https://rpc.blockdaemon.testnet.arc.network,wss=rpc.blockdaemon.testnet.arc.network/websocket \
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

For real-time logs, run these commands in separate terminals:

```sh
# Consensus layer logs (most useful for sync issues)
sudo journalctl -u arc-consensus -f

# Execution layer logs
sudo journalctl -u arc-execution -f

> Notice that this command queries the execution layer's HTTP server offering
> a local JSON-RPC API.
> If the address and port of the HTTP endpoint are configured differently than
> the above example, adapt the command accordingly.

> **Docker:** For running with Docker Compose instead of binaries, see
> [Running an Arc Node with Docker](running-an-arc-node-docker.md).

## Separated hosts

The [Quick Start](#quick-start) section describes the setup of the execution
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

From the [Start execution layer](#start-execution-layer) instructions, two changes are required:

1. Remove all flags related to IPC communication: `--ipcpath`, `--auth-ipc`, `--auth-ipc.path`;
2. Add the following parameters to configure the RPC interaction:
```sh
  --authrpc.addr 0.0.0.0 \
  --authrpc.port 8551 \
  --authrpc.jwtsecret "$ARC_HOME/jwtsecret"
```

**Important:** with this setup, port 8551 is exposed via all network
interfaces (`0.0.0.0`).
Make sure to configure the firewall to restrict the access to this port to the
consensus layer's host.
The Engine API controls block production — do not expose it to the public internet.

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

To enable this, the `reth_` namespace should enabled on the **execution layer**:

```sh
--http.api eth,net,web3,txpool,trace,debug,reth
```

And on the **consensus layer** backpressure must be activated:

```sh
--execution-persistence-backpressure \
--execution-persistence-backpressure-threshold=10
```

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
  --rpc.forwarder https://rpc.quicknode.testnet.arc.network/

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
  --follow.endpoint https://rpc.drpc.testnet.arc.network,wss=rpc.drpc.testnet.arc.network \
  --follow.endpoint https://rpc.quicknode.testnet.arc.network,wss=rpc.quicknode.testnet.arc.network \
  --follow.endpoint https://rpc.blockdaemon.testnet.arc.network,wss=rpc.blockdaemon.testnet.arc.network/websocket \
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

The `--full` flag is accepted by both the CL and EL and will enable pruning.
When bootstrapping from a pruned snapshot, `--full` is **required** on the
first EL start to reconcile the database (see the note in
[Start execution layer](#start-execution-layer)). After that initial run you
can restart without `--full`.

> **Caution:** EL pruning increases memory usage and may cause out-of-memory
> issues on constrained machines. If you encounter memory pressure, enable
> backpressure (see [System Requirements](#system-requirements) section) and remove
> `--full` after the first successful start.
