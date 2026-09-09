# Arc Testnet RPC Endpoints

Circle publishes the endpoint list under
[Connect to Arc](https://docs.arc.io/arc/references/connect-to-arc) and the
provider list under
[Node providers](https://docs.arc.io/arc/tools/node-providers). Neither page
covers what a client sees when a public endpoint throttles it, so this page
records that: the error shape, the limit behind it and the fallback setup for
viem and ethers.

Every value below was measured against the live endpoints on 2026-08-02 from a
single client IP. `arc_getVersion` reported node version v0.7.3 behind three of
them and v0.7.3-rc1 behind Blockdaemon. The limits are not published and can
change. Treat the numbers as one observation rather than a guarantee.

## Endpoints

| HTTP | WebSocket | Operator |
|------|-----------|----------|
| `https://rpc.testnet.arc.io` | `wss://rpc.testnet.arc.io` | Circle |
| `https://rpc.drpc.testnet.arc.io` | `wss://rpc.drpc.testnet.arc.io` | dRPC |
| `https://rpc.quicknode.testnet.arc.io` | `wss://rpc.quicknode.testnet.arc.io` | QuickNode |
| `https://rpc.blockdaemon.testnet.arc.io` | `wss://rpc.blockdaemon.testnet.arc.io/websocket` | Blockdaemon |

All four answered `eth_chainId` with `0x4cef52` (5042002) and reported an
advancing block height. `arc_getVersion` and `arc_getCertificate` answer on all
four. dRPC serves the same chain on its own hostname,
`https://arc-testnet.drpc.org`, which also returned `0x4cef52`; the `arc.io`
name is the documented one.

The Blockdaemon WebSocket needs the `/websocket` path. Without it the upgrade is
refused.

The `--rpc.forwarder` and `--follow.endpoint` examples in
[Running an Arc Node](./running-an-arc-node.md) still use the older
`rpc.*.testnet.arc.network` hostnames. Those answered the same chain id when
this page was written.

## The request limit

Over HTTP a rejected request comes back as HTTP 429 carrying a well formed
JSON-RPC error:

```text
HTTP/1.1 429
content-type: application/json
x-ratelimit-limit: 1, 1;w=1
x-ratelimit-remaining: 0
x-ratelimit-reset: 1

{"jsonrpc":"2.0","id":0,"error":{"code":-32011,"message":"request limit reached"}}
```

Inside a JSON-RPC batch the HTTP call succeeds with 200 and individual items
carry the same error, so a caller that checks only the transport status sees
nothing wrong. There is no `Retry-After` header. `x-ratelimit-reset: 1` is the
only hint, and it appears on rejections only.

What the limit is, measured on `https://rpc.testnet.arc.io`:

| Traffic pattern | Result |
|-----------------|--------|
| 10 requests queued back to back on one keep-alive connection, 2.5 s total | 10 answered |
| 20 requests the same way, 5.3 s total | 20 answered |
| 2 requests sent at the same moment | 1 answered, 1 rejected |
| 4 requests sent at the same moment | 1 answered, 3 rejected |
| 20 requests sent at the same moment | 12 answered, 8 rejected |
| one batch of 3 | 3 answered |
| one batch of 5 | 1 answered, 4 rejected |
| one batch of 20 | 1 answered, 19 rejected |

Rate is not what trips it. Overlap is. Serialized traffic at about four requests
a second was never rejected, while two requests in flight at once lost one. The
`x-ratelimit-limit: 1, 1;w=1` header points the same way: the allowance is one
request, not a bucket that can be spent in parallel.

What that means for a client:

- Reuse one connection and let requests queue on it.
- Do not fan reads out with `Promise.all` against the primary endpoint.
- Do not enable JSON-RPC batching against the primary endpoint.
- Read -32011 as "try again in a second", not as a failed transaction.

`rpc.quicknode.testnet.arc.io` behaves the same way (1 of 4 simultaneous
answered, 1 of a 5 item batch). The dRPC and Blockdaemon hostnames answered 4 of
4 and a 5 item batch in full, so concurrent load belongs there. dRPC's public
tier has its own limit with its own shape, hit once while probing: HTTP 429, code
`15`, message "You reached Public endpoint rate limit, please upgrade to paid
plan". That code is dRPC's, not Arc's.

The node does not produce -32011. The operator guide says the node "does not
enforce per-client request limits" and asks operators to run a proxy in front of
it (see
[Firewall and network](./running-an-arc-node.md#firewall-and-network)), so the
rejection comes from that layer. Everything sharing your source address shares
the allowance:
[issue #207](https://github.com/circlefin/arc-node/issues/207) reports a block
explorer tab counting against the same bucket as a local dApp, which this page
did not measure.

## What each endpoint serves

| Call | Primary | dRPC | QuickNode | Blockdaemon |
|------|---------|------|-----------|-------------|
| `eth_*`, `net_*`, `web3_*` | yes | yes | yes | yes |
| `eth_getLogs` over a 10k block range | yes | yes | yes | yes |
| `arc_getVersion`, `arc_getCertificate` | yes | yes | yes | yes |
| `rpc_modules` | yes | `-32601` | yes | `-32003` |
| `debug_getRawHeader` | yes | paid plan (`35`) | yes | `-32003` |
| `txpool_status` | `-32604` | `-32601` | `-32604` | `-32601` |
| WebSocket handshake plus a JSON-RPC call | yes | yes | yes | yes, on `/websocket` |

`eth_subscribe` over HTTP fails as it should (`-32603` on the primary). Use the
WebSocket endpoint instead. A `newHeads` subscription on
`wss://rpc.testnet.arc.io` returned a subscription id and delivered
notifications, so WebSocket streaming is available on the public endpoint. The
other three sockets were checked with an `eth_chainId` call rather than a
subscription.

Do not build on the `debug` and `trace` reachability in that table. The operator
guide tells public RPC nodes to expose `eth,net,web3,rpc` only, so treat any
other namespace as incidental and withdrawable.

## Fallback and retry solve different failures

A host that is down, unreachable or on the wrong chain is fixed by trying
another host. A limit you are over is not: the next endpoint is being asked for
the same volume by the same client. Failover buys headroom, back-off buys time
and a rate limit usually needs both.

- Dead host: failover. viem `fallback`, ethers `FallbackProvider`.
- Throttled host: failover spreads the overflow, but when the whole list is
  throttled the call still fails, so it also needs a delayed retry.

## viem

Verified against viem 2.55.10.

```ts
import { createPublicClient, fallback, http } from "viem";
import { arcTestnet } from "viem/chains";

const endpoints = [
  "https://rpc.testnet.arc.io",
  "https://rpc.drpc.testnet.arc.io",
  "https://rpc.blockdaemon.testnet.arc.io",
  "https://rpc.quicknode.testnet.arc.io",
];

export const publicClient = createPublicClient({
  chain: arcTestnet,
  transport: fallback(endpoints.map((url) => http(url, { batch: false }))),
  pollingInterval: 4_000,
});
```

`viem/chains` exports `arcTestnet`, so the chain does not need writing by hand.
Its `rpcUrls` still hold the `arc.network` hostnames, which is why the transport
list is passed explicitly.

Three things are easy to get wrong here.

**`http()` on its own does not retry -32011.** viem retries three times by
default, but only for `-1`, `-32005`, `-32603` or an `HttpRequestError` with a
retryable status (`utils/buildRequest`). Arc's rejection is a valid JSON-RPC
error body, so `utils/rpc/http` returns the body instead of raising the 429 as an
`HttpRequestError`, and the retry check never sees a status. Against a local
server reproducing Arc's exact response, one call made one request and threw
`RpcRequestError` with `code: -32011`. The same server answering 429 with a plain
text body made four requests, which is the retry path Arc's shape walks past.

**`fallback()` does cover -32011.** Its `shouldThrow` stops only on a rejected or
reverted transaction, so every other error advances to the next transport. Same
local setup with the throttled transport first and a healthy one second: one
request each and the call resolved. Keeping the primary first is fine, since a
burst that overflows it spills onto the next endpoint instead of failing.

**When every transport is throttled the call still throws** the last `-32011`,
one request per transport and no retry. Calls that must not give up need their
own back-off:

```ts
import {
  BaseError,
  RpcRequestError,
  type Hash,
  type PublicClient,
  type TransactionReceipt,
} from "viem";

const REQUEST_LIMIT_REACHED = -32011;

export async function waitForReceipt(
  client: PublicClient,
  hash: Hash,
): Promise<TransactionReceipt> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await client.waitForTransactionReceipt({ hash, timeout: 60_000 });
    } catch (error) {
      const rpcError =
        error instanceof BaseError
          ? error.walk((e) => e instanceof RpcRequestError)
          : null;
      const throttled =
        rpcError instanceof RpcRequestError &&
        rpcError.code === REQUEST_LIMIT_REACHED;
      if (!throttled || attempt >= 4) throw error;
      await new Promise((resolve) => setTimeout(resolve, 1_000 * 2 ** attempt));
    }
  }
}
```

`waitForTransactionReceipt` is the call that needs this most, and it fails two
different ways. Its own `retryCount` guards the replacement-detection lookup
only. The receipt lookup inside the poll has no such guard, so a -32011 there
rejects the wait on the first poll even when the transaction is already
confirmed. Retrying is safe, because the receipt lookup is idempotent. The other
way is quieter: the opening receipt lookup and the block-number poll both
swallow their errors, so a client throttled on every call waits for `timeout` to
expire and then raises `WaitForTransactionReceiptTimeoutError`, 180 seconds by
default (`actions/public/waitForTransactionReceipt`). Both paths were reproduced
against a local server. Throttling only the receipt lookup rejected on the first
poll. Throttling everything timed out.

`rank: true` reorders transports by latency and stability. It pings
`net_listening` on every transport every `pollingInterval`
(`clients/transports/fallback`), which all four endpoints answer, at the cost of
a standing background request against each one. Weigh that against a limit
measured at one request in flight.

## ethers

Verified against ethers 6.17.0.

```ts
import { FallbackProvider, JsonRpcProvider, Network } from "ethers";

const arcTestnet = new Network("arc-testnet", 5042002);

const endpoints = [
  "https://rpc.testnet.arc.io",
  "https://rpc.drpc.testnet.arc.io",
  "https://rpc.blockdaemon.testnet.arc.io",
  "https://rpc.quicknode.testnet.arc.io",
];

export const provider = new FallbackProvider(
  endpoints.map((url, index) => ({
    provider: new JsonRpcProvider(url, arcTestnet, {
      staticNetwork: arcTestnet,
      batchMaxCount: 1,
    }),
    priority: index + 1,
    weight: 1,
    stallTimeout: 1_500,
  })),
  arcTestnet,
  { quorum: 1 },
);
```

**`batchMaxCount: 1` is not optional.** `JsonRpcProvider` batches by default, up
to 100 calls per 10 ms window (`batchMaxCount`, `batchStallTime`). Against the
primary endpoint a batch of 5 loses 4 items.

**`quorum: 1` is not optional either.** The default is
`ceil(sum of weights / 2)`, so two providers give quorum 1 but three or four give
quorum 2. Every call then goes to at least two endpoints and has to agree, which
doubles the volume you send. Checked by construction: 2 providers give 1, 3 give
2, 4 give 2.

**ethers already retries a 429 for you.** `FetchRequest` retries on 429 up to 12
attempts with a randomized 250 ms slot back-off, honoring `Retry-After` when
present, which Arc does not send. A throttled single call is therefore retried at
the transport level and -32011 never surfaces. What surfaces once the attempts
run out is `code: "SERVER_ERROR"` with message `exceeded maximum retry limit`.
With batching left on, a throttled item never reaches that retry, because the
HTTP status is 200. It arrives as `code: "UNKNOWN_ERROR"` and
`shortMessage: "could not coalesce error"`, carrying the JSON-RPC error on
`error.error`. Match both shapes if you branch on the rate limit.

`broadcastTransaction` is sent to every provider in a `FallbackProvider` by
design, so a fallback list multiplies write traffic.

## What only Circle can change

- **A published endpoint list.** [Connect to Arc](https://docs.arc.io/arc/references/connect-to-arc)
  now carries one. This page is a live check of that list, not a second source of
  truth: endpoints added or retired later will not appear here on their own.
- **A published limit.** Without a documented quota a client cannot size a
  request budget, and with no `Retry-After` the back-off is guesswork.
- **Headroom for builders.** Keeper jobs, load tests and metered sessions do not
  fit inside one request in flight. Nothing on the client side substitutes for a
  higher limit or a keyed tier.
- **The wallet-facing error.** A throttled `eth_estimateGas` or
  `eth_sendRawTransaction` reads to a user as a failed contract call. Wallets map
  that text, and this repo does not.
- **A status signal.** There is no status page or health endpoint that separates
  a throttled client from a degraded network.
