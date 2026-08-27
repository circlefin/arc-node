# Transaction forwarding and failover for follow nodes

Follow nodes (RPC read nodes) do not build blocks. They serve reads locally and
forward write transactions to an upstream that does build blocks.

Key points that hold for every method below:

- Only raw transaction submission is forwarded, `eth_sendRawTransaction` and
  `eth_sendRawTransactionSync`. Every other method is served from the follow
  node's local state.
- Each accepted transaction is retained in the local pool, so local RPC reads
  see it before it is mined and the block is imported.
- A single upstream is a single point of failure: submission breaks whenever
  that endpoint is down.

| Method | Failover | Health checks | Load balancing | Extra infra |
|--------|----------|---------------|----------------|-------------|
| `--rpc.forwarder` (single upstream) | No | No | No | None |
| `--rpc.forwarder` (external proxy) | Yes, your policy | Active | Yes | A proxy to operate |
| `--arc.tx.relays` (built in) | Yes, ordered | Per request | No | None |

## Method 1: single upstream

When one upstream is enough, Reth's built-in `--rpc.forwarder` forwards raw
transaction submission to a single URL with no failover.

```sh
arc-node-execution node \
  --chain arc-testnet \
  --datadir $ARC_EXECUTION \
  --http --http.addr 127.0.0.1 --http.port 8545 \
  --http.api eth,net,web3 \
  --rpc.forwarder https://rpc-a.example/
```

This is the simplest option and matches the setup in
[Running an Arc node](./running-an-arc-node.md). It has no redundancy: if the
upstream is unreachable, submission fails.

## Method 2: external proxy

If you already operate a layer-7 proxy such as nginx, HAProxy, or Envoy, point
`--rpc.forwarder` at it and let the proxy select upstreams. The proxy owns the
forwarding policy, so you can configure active health checks, weighting, load
balancing, and TLS termination however you want.

The proxy must listen on a different address than the node's own RPC port,
otherwise the forwarder loops back to the node. Here the node serves on 8545 and
the proxy on 8600:

```sh
arc-node-execution node \
  --chain arc-testnet \
  --datadir $ARC_EXECUTION \
  --http --http.addr 127.0.0.1 --http.port 8545 \
  --http.api eth,net,web3 \
  --rpc.forwarder http://127.0.0.1:8600/
```

Example nginx upstream with an ordered failover, primary plus backup:

```nginx
upstream arc_rpc {
    server rpc-a.example:443 max_fails=3 fail_timeout=10s;
    server rpc-b.example:443 backup;
}

server {
    listen 8600;
    location / {
        proxy_pass https://arc_rpc;
        proxy_next_upstream error timeout http_502 http_503 http_504 http_429;
        proxy_connect_timeout 5s;
    }
}
```

Choose this when you already run such a proxy, or need active health probing or
weighted load balancing. The trade-off is another component to deploy and operate.

## Method 3: built-in multi-relay failover

`--arc.tx.relays` takes a comma-separated list of upstream URLs in priority order
and fails over across them with no additional infrastructure.

```sh
arc-node-execution node \
  --chain arc-testnet \
  --datadir $ARC_EXECUTION \
  --http --http.addr 127.0.0.1 --http.port 8545 \
  --http.api eth,net,web3 \
  --arc.tx.relays https://rpc-a.example/,https://rpc-b.example/,https://rpc-c.example/
```

The list may also be supplied through the `ARC_TX_RELAYS` environment variable.
`--arc.tx.relays.timeout` (env `ARC_TX_RELAYS_TIMEOUT`, default `10s`) bounds each
relay attempt, connection plus response; when it elapses the relay advances to
the next upstream. It accepts `10s`, `500ms`, or a bare number of seconds. The
default suits Arc's sub-second finality; raise it if `eth_sendRawTransactionSync`
submissions need longer to be mined.

Behavior:

- Relays `eth_sendRawTransaction` and `eth_sendRawTransactionSync` to the current
  upstream, sticky to the last good one.
- Advances to the next upstream, wrapping past the end of the list, on a
  transport failure (connection refused, DNS or TLS error, timeout) or an HTTP
  5xx or 429 response. One full pass is attempted before giving up.
- Returns a JSON-RPC error from a reachable upstream verbatim, without failover.
  A rejected transaction is a decision, not an outage.
- Retains each accepted transaction in the local pool so local RPC reads see it
  before it is mined.
- Returns a relay error when every upstream fails a full pass, and does not add
  the transaction locally.

Conflicts with `--rpc.forwarder`; set one or the other, not both.

Metrics:

- `arc_tx_relay_failovers_total` increments on each advance to the next upstream.
- `arc_tx_relay_exhausted_total` increments when a full pass finds no reachable
  upstream.
