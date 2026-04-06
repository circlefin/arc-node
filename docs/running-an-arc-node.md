# Running an Arc Node

Arc is an open, EVM-compatible Layer-1 blockchain. Anyone can run an Arc node — no permission required. Running your own node gives you independent verification of the chain and direct API access to the network.

## What Your Node Does

- **Verifies every block** — Every block is cryptographically verified against the signatures of the validator set before it is accepted. Your node independently confirms that validators finalized each block
- **Executes every transaction** — Every transaction is re-executed locally through the EVM. Your node maintains its own copy of the complete blockchain state
- **Exposes a local RPC endpoint** — Your node provides a standard Ethereum JSON-RPC API (`http://localhost:8545`) for querying blocks, balances, and transactions, and for submitting calls directly against your own verified state

## Quick Start

An Arc node runs two processes: the Execution Layer (EL) and the Consensus Layer (CL). The EL executes transactions and maintains blockchain state. The CL fetches blocks from the network, verifies their cryptographic signatures, and passes them to the EL for execution.

See [installation](installation.md) for instructions on how to install the binaries on your machine.

**0. Create data directories** (one-time setup):

```sh
mkdir -p ~/.arc/execution ~/.arc/consensus
sudo install -d -o $USER /run/arc
```

> **macOS:** `/run` does not exist on macOS. Use a user-local directory instead (e.g. `mkdir -p ~/.arc/run`) and adjust the `--ipcpath`, `--auth-ipc.path`, `--eth-socket`, and `--execution-socket` flags in the commands below accordingly.

When running as a systemd service, `RuntimeDirectory=arc` creates `/run/arc` automatically — skip the second command.

**1. Download snapshots** (required). Syncing from genesis is not currently supported -- a snapshot is needed to bootstrap the node.

```sh
arc-snapshots download --chain=arc-testnet
```

This command fetches the latest snapshot URLs from https://snapshots.arc.network, downloads the snapshots, and extracts them into `~/.arc/execution` and `~/.arc/consensus` respectively.

**2. Start the Execution Layer:**

```sh
arc-node-execution node \
  --chain arc-testnet \
  --datadir ~/.arc/execution \
  --disable-discovery \
  --ipcpath /run/arc/reth.ipc \
  --auth-ipc \
  --auth-ipc.path /run/arc/auth.ipc \
  --http \
  --http.addr 127.0.0.1 \
  --http.port 8545 \
  --http.api eth,net,web3,txpool,trace,debug \
  --metrics 127.0.0.1:9001 \
  --full \
  --enable-arc-rpc \
  --rpc.forwarder https://rpc.quicknode.testnet.arc.network/
```

> `--chain arc-testnet` uses the genesis configuration bundled in the binary. Replace with `--chain /path/to/genesis.json` if you have a custom genesis file.

> `--http` / `--http.port` expose the JSON-RPC API on localhost. `--rpc.forwarder` routes transactions to an RPC node.

See [reth node](https://reth.rs/cli/reth/node/) for additional flags.

**3. Initialize the Consensus Layer** (one-time setup):

```sh
arc-node-consensus init --home ~/.arc/consensus
```

This generates a private key file used for P2P network identity.

**4. Start the Consensus Layer** (in a separate terminal):

```sh
arc-node-consensus start \
  --home ~/.arc/consensus \
  --eth-socket /run/arc/reth.ipc \
  --execution-socket /run/arc/auth.ipc \
  --rpc.addr 127.0.0.1:31000 \
  --full \
  --follow \
  --follow.endpoint https://rpc.drpc.testnet.arc.network,wss=rpc.drpc.testnet.arc.network \
  --follow.endpoint https://rpc.quicknode.testnet.arc.network,wss=rpc.quicknode.testnet.arc.network \
  --follow.endpoint https://rpc.blockdaemon.testnet.arc.network,wss=rpc.blockdaemon.testnet.arc.network \
  --metrics 127.0.0.1:29000
```

> **Note:** Start the Execution Layer first. The Consensus Layer connects to it on startup and will fail if the EL is not running.

> **Note:** The Blockdaemon endpoint does not currently support WebSocket connections. The node will log retry warnings for this endpoint but still syncs correctly via the other two endpoints. HTTP block fetching from Blockdaemon works normally.

**5. Verify the node is syncing:**

Wait about 30 seconds, then check the block height:

```sh
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'
```

The `result` field should be a hex block number that increases over time. If it stays at `0x0`, check the Consensus Layer logs for connection errors.

### EL ↔ CL Communication

The Quick Start above uses IPC sockets, which require EL and CL to run on the same host. If they are on separate hosts, use RPC instead.

**Generate a JWT secret** (one-time setup). The EL and CL use this to authenticate with each other:

```sh
openssl rand -hex 32 | tr -d "\n" > ~/.arc/jwtsecret
chmod 600 ~/.arc/jwtsecret
```


**EL flags (RPC):**

Remove the IPC flags and add:

```sh
--authrpc.addr 0.0.0.0 \
--authrpc.port 8551 \
--authrpc.jwtsecret ~/.arc/jwtsecret
```

> **Security:** When using `--authrpc.addr 0.0.0.0`, restrict access to the Engine API port (8551) using firewall rules or a private network. The Engine API controls block production — do not expose it to the public internet.

**CL flags (RPC):**

Remove `--eth-socket` and `--execution-socket`, and add:

```sh
--eth-rpc-endpoint http://<EL_HOST>:8545 \
--execution-endpoint http://<EL_HOST>:8551 \
--execution-jwt ~/.arc/jwtsecret
```

> IPC and RPC are mutually exclusive. Both processes must have read/write access to the IPC socket directory when using IPC.

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
  --disable-discovery \
  --ipcpath /run/arc/reth.ipc \
  --auth-ipc \
  --auth-ipc.path /run/arc/auth.ipc \
  --http \
  --http.addr 127.0.0.1 \
  --http.port 8545 \
  --http.api eth,net,web3,txpool,trace,debug \
  --metrics 127.0.0.1:9001 \
  --full \
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
  --eth-socket /run/arc/reth.ipc \
  --execution-socket /run/arc/auth.ipc \
  --rpc.addr 127.0.0.1:31000 \
  --full \
  --follow \
  --follow.endpoint https://rpc.drpc.testnet.arc.network,wss=rpc.drpc.testnet.arc.network \
  --follow.endpoint https://rpc.quicknode.testnet.arc.network,wss=rpc.quicknode.testnet.arc.network \
  --follow.endpoint https://rpc.blockdaemon.testnet.arc.network,wss=rpc.blockdaemon.testnet.arc.network \
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
