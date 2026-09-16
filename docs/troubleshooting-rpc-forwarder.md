# Troubleshooting RPC transaction forwarding

Follower nodes use `--rpc.forwarder` to submit transactions that cannot be propagated locally. A healthy follower can serve read-only JSON-RPC calls while transaction submission still fails if the configured forwarder cannot be reached by the execution-layer process.

This guide focuses on failures such as:

```text
error code -32603: error sending request for url (...)
```

and log messages around `eth_sendRawTransaction` forwarding.

## 1. Confirm the forwarder URL used by the node

The standard testnet configuration uses:

```sh
--rpc.forwarder https://rpc.quicknode.testnet.arc.network/
```

Check the actual process arguments rather than assuming the service file was reloaded:

```sh
ps -ef | grep '[a]rc-node-execution'
```

For systemd installations, also inspect the effective unit:

```sh
systemctl cat arc-execution
systemctl show arc-execution -p ExecStart
```

After changing a unit file, run `systemctl daemon-reload` before restarting the service.

## 2. Test JSON-RPC from the same host

A successful TCP or TLS connection alone does not prove that the upstream accepts JSON-RPC. Send a real request from the node host:

```sh
curl -sS -X POST https://rpc.quicknode.testnet.arc.network/ \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

For Arc Testnet the response should report chain ID `5042002` (`0x4cef52`).

If this request fails, fix DNS, routing, TLS, proxy, or firewall policy before debugging `arc-node-execution`.

## 3. Test from the execution process environment

Environment differences are common when `curl` works in an interactive shell but the systemd service fails. Inspect the service environment and proxy variables:

```sh
systemctl show arc-execution -p Environment
systemctl show arc-execution -p User -p Group
```

Pay particular attention to `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, `SSL_CERT_FILE`, and `SSL_CERT_DIR`. A service can have a different certificate store or proxy configuration than the login shell.

To reproduce the request under the same service account, run:

```sh
sudo -u "$(systemctl show -p User --value arc-execution)" \
  curl -sS -X POST https://rpc.quicknode.testnet.arc.network/ \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

If the unit does not set `User=`, run the check as the account that starts the node.

## 4. Enable targeted debug logs

Run the execution layer with debug logging for the RPC client while reproducing a single transaction submission:

```sh
RUST_LOG='info,rpc::eth=debug,alloy_rpc_client=debug' arc-node-execution node ...
```

With systemd, temporarily add the same `RUST_LOG` value to the service environment and restart it. Avoid enabling broad trace logging on a busy public RPC endpoint because it can produce a large amount of output.

Then submit one transaction and capture the log lines immediately before and after the forwarding failure:

```sh
journalctl -u arc-execution --since '2 minutes ago' --no-pager
```

## 5. Separate local RPC health from forwarding health

These checks answer different questions:

```sh
# Local execution-layer RPC is alive
curl -sS -X POST http://127.0.0.1:8545 \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# Upstream forwarder is reachable and speaks the expected chain
curl -sS -X POST https://rpc.quicknode.testnet.arc.network/ \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

A follower can pass the first check while failing the second. In that state, reads from the local verified chain can work even though `eth_sendRawTransaction` cannot be forwarded.

## 6. Check for idle connection-pool failures

If the reachability, TLS, DNS, proxy, and service-environment checks above all pass, test whether the failure correlates with an idle gap before the forwarded write.

One useful comparison is:

1. Poll a read-only RPC such as `eth_blockNumber` every few seconds, then submit a transaction immediately.
2. Repeat the same test, but leave the node idle for roughly 30–90 seconds before submitting the transaction.

If the immediate write succeeds while the post-idle write fails, the symptom is more consistent with reuse of a stale pooled connection than with basic upstream reachability. This behavior is being investigated in [#59](https://github.com/circlefin/arc-node/issues/59).

When testing the upstream directly, also compare the default curl behavior with an explicit HTTP/1.1 request:

```sh
# Default protocol negotiation
curl -sS -X POST https://rpc.quicknode.testnet.arc.network/ \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'

# Force HTTP/1.1 as a negative control
curl --http1.1 -sS -X POST https://rpc.quicknode.testnet.arc.network/ \
  -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

If both variants succeed while the follower still fails only after an idle period, capture the timing and debug logs and include them in the issue report. That evidence helps distinguish protocol-negotiation problems from connection-pool staleness.

## 7. Information to include in a bug report

When the generic forwarding error remains after the checks above, include:

- `arc-node-execution --version` and the exact commit or release tag;
- operating system and architecture;
- the `--rpc.forwarder` URL with credentials or API keys removed;
- whether the JSON-RPC `eth_chainId` curl succeeds under the service account;
- whether the failure changes after a 30–90 second idle gap;
- whether default curl and `curl --http1.1` behave differently;
- the relevant `RUST_LOG` lines from `rpc::eth` and `alloy_rpc_client`;
- whether the node is started interactively, through systemd, or in Docker;
- proxy and custom CA usage, without including secrets.

Do not post private keys, bearer tokens, API keys, JWT secrets, or complete environment dumps containing credentials.

## Related documentation

See [Running an Arc Node](running-an-arc-node.md) for the standard follower-node configuration and the current testnet forwarder example.

For the live investigation into idle-time forwarding failures and connection-pool behavior, see [issue #59](https://github.com/circlefin/arc-node/issues/59).
