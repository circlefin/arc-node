# Arc Testnet public RPC

Reference for application developers using the Arc Testnet JSON-RPC endpoints.
For node-operator follow/forwarder configuration, see [Running an Arc Node](running-an-arc-node.md).

## Endpoints

Arc Testnet (chain ID `5042002`) exposes multiple public HTTPS endpoints.
Spread traffic across providers and serialize concurrent requests per endpoint
(see [Concurrency limiting](#concurrency-limiting)).

| URL | Notes |
| --- | --- |
| `https://rpc.testnet.arc.network` | Primary public endpoint |
| `https://rpc.testnet.arc.io` | Alternate public endpoint; `wss://` supports `eth_subscribe` |
| `https://rpc.drpc.testnet.arc.network` | DRPC provider |
| `https://rpc.drpc.testnet.arc.io` | DRPC provider (alternate host) |
| `https://rpc.blockdaemon.testnet.arc.network` | Blockdaemon provider |
| `https://rpc.blockdaemon.testnet.arc.io` | Blockdaemon provider (alternate host) |
| `https://rpc.quicknode.testnet.arc.network` | QuickNode provider |
| `https://rpc.quicknode.testnet.arc.io` | QuickNode provider (alternate host) |
| `https://arc-testnet.drpc.org` | Third-party DRPC endpoint |

The `.arc.io` hosts overlap with the provider set documented for node follow mode in
[Running an Arc Node](running-an-arc-node.md) (primary, DRPC, Blockdaemon); QuickNode
answers on `.arc.io` but is not part of that documented follow set. The `.arc.network`
hosts are the URLs most sample apps and wallets default to.

## Gas estimation limits

### EIP-7825 per-transaction cap (protocol)

Arc Testnet activates EIP-7825 (Osaka) at the Osaka hardfork (activated alongside
Zero5). The **per-transaction gas limit is 16,777,216 (2²⁴)** on every node —
public RPC, self-hosted, and third-party infrastructure alike.

The effective ceiling for a single `eth_estimateGas` / `eth_call` is therefore:

```text
min(--rpc.gascap, 16_777_216)
```

Self-hosted `arc-node-execution` defaults to `--rpc.gascap=30000000` (30M), but
post-Osaka the protocol cap always undercuts it for a single transaction. Raising
`--rpc.gascap` alone cannot make a transaction above 2²⁴ gas valid.

The block gas limit on Arc Testnet is 30M (`eth_getBlockByNumber("latest").gasLimit`).
That budget is shared across **multiple transactions per block**, not one
large transaction.

### Error shapes

Cap and budget failures surface under several JSON-RPC error texts. Tooling
should pattern-match both families rather than a single string:

| Shape | Typical code | When |
| --- | --- | --- |
| `out of gas: gas required exceeds: 16777216` | `-32003` | Gas budget exceeds EIP-7825 cap (common on cap probes) |
| `gas required exceeds allowance (<N>)` | `-32000` | Allowance clamp (balance-derived `N`, or cap-valued `N` on some paths) |
| Other `out of gas` / halt variants | varies | Cap/limit failures on older reth lineages |

An explicit `"gas": 30000000` in the request is silently clamped to 2²⁴ when the
protocol cap applies.

**Do not treat an `eth_estimateGas` failure above 2²⁴ as proof that a higher gas
limit would succeed on-chain** — it would not. For failures below the protocol
cap, compare the parenthesized limit against `--rpc.gascap` and the sender's
balance-derived allowance. See [BREAKING_CHANGES.md](../BREAKING_CHANGES.md#v072).

## Concurrency limiting

Public endpoints enforce a **per-connection concurrency limit**, not a requests-per-second
rate. Observed behavior:

- Serialized requests (~4/s on one connection) are not rejected.
- Two requests in flight on the same connection: one may receive `-32011`.
- JSON-RPC batches of *N* entries may return `-32011` on *N−1* items behind HTTP 200.
- Response header: `x-ratelimit-limit: 1;w=1` (one request in flight).

```json
{"code":-32011,"message":"request limit reached"}
```

Depending on the client layer, this surfaces as:

- viem `RpcRequestError` on receipt polls (`shortMessage: "RPC Request failed."`)
- `ContractFunctionRevertedError` on some estimate/call paths
- MetaMask toasts that look like a contract revert (`"Request is being rate limited"`)

**Actionable guidance:** serialize RPC calls per endpoint (avoid `Promise.all`
fan-out on one URL), prefer keep-alive on a single connection, and use
`fallback()` across independent providers — each provider's concurrency budget
is separate.

`viem`'s `fallback()` advances past `-32011` on most call paths (its `shouldThrow`
only halts on rejected/reverted transactions). Detection code should walk the
error `cause` chain for code `-32011` or the message rather than relying on a
single `instanceof`.

```ts
import { createPublicClient, fallback, http } from "viem";
import { arcTestnet } from "viem/chains";

const RPC_URLS = [
  "https://rpc.testnet.arc.network",
  "https://rpc.drpc.testnet.arc.network",
  "https://rpc.blockdaemon.testnet.arc.network",
  "https://rpc.quicknode.testnet.arc.network",
];

export const publicClient = createPublicClient({
  chain: arcTestnet,
  transport: fallback(RPC_URLS.map((url) => http(url))),
});
```

`waitForTransactionReceipt` still aborts on the first `-32011` poll unless wrapped
with application-level retry — a rate-limited receipt poll does not mean the
transaction failed on-chain.

For production workloads, run your own node or use a dedicated RPC provider rather
than relying on the shared public endpoints alone.
