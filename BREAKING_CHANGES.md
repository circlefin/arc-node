# Breaking Changes

Records breaking changes between tagged public releases of [`arc-node`](https://github.com/circlefin/arc-node).

Each bullet is prefixed with a flag identifying the kind of breaking change:

- `[CLI]` -- CLI flag added, renamed, removed, or made required.
- `[Config]` -- default value, environment variable, or manifest field change.
- `[Format]` -- log, metric label, or serialized output format change that breaks parsers.

Entries are split by audience. A change appears under `### For Validators` when validator-mode operation must change; otherwise it appears under `### For Node Operators`. A change requiring both audiences to act appears in both sections (rare).

Compare and release-notes links resolve once the corresponding tag is published at [`circlefin/arc-node`](https://github.com/circlefin/arc-node).

## [v0.8.5]

**Changes:** [v0.8.4...v0.8.5](https://github.com/circlefin/arc-node/compare/v0.8.4...v0.8.5) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.8.5)

No breaking changes in this release.

## [v0.8.4]

**Changes:** [v0.8.3...v0.8.4](https://github.com/circlefin/arc-node/compare/v0.8.3...v0.8.4) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.8.4)

No breaking changes in this release.

## [v0.8.3]

**Changes:** [v0.8.2...v0.8.3](https://github.com/circlefin/arc-node/compare/v0.8.2...v0.8.3) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.8.3)

No breaking changes in this release.

## [v0.8.2]

**Changes:** [v0.8.1...v0.8.2](https://github.com/circlefin/arc-node/compare/v0.8.1...v0.8.2) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.8.2)

This release has no hardfork activation and no database migration, and v0.8.2 nodes run alongside v0.8.0 and v0.8.1 nodes on the same network. Node operators can upgrade the binary on their own schedule.

### For Node Operators

- **[Format] `arc-node-consensus` height-range RPC responses are now a paged object instead of a JSON array.**
  - Applies to `/commit`, `/misbehavior-evidence`, `/proposal-monitor`, and `/invalid-payloads` when `count` is greater than `1`. Single-height requests (`count` omitted or `1`) still return the per-height object unchanged.
  - Old: `[ { "height": 100, ... }, { "height": 101, ... } ]`. New: `{ "items": [ { "height": 100, ... }, { "height": 101, ... } ], "next": { "height": 102 } }`.
  - A page can cover fewer than `count` heights. It stops before the first height whose stored records would push the page past a 4 MiB budget (a single height larger than the budget is still returned on its own), and `next` holds the first height it did not cover. `next` is absent once the requested range is exhausted.
  - To read a whole range, repeat the request with `height` set to `next.height` and `count` covering the heights still wanted, until `next` is absent.
  - A range that extends past the latest decided height can now return `200` with `next` set, when the budget stops the page before the head. Continuing from `next` returns the `400` `above_current_head` error.
  - The `count` cap rises from `1000` to `10000`. Requests above the cap still return `400` with reason `over_limit`.
  - The response shape does not depend on the API version negotiated through the `Accept` header, so no media type returns the previous array. Clients that parse range responses as an array must read `items` and follow `next`.
  - A client that also queries nodes on an earlier release can branch on the response type: an array is the previous form, an object with `items` is the new one. Keep `count` at `1000` or below for those nodes, since they reject larger values with `over_limit`.
- **[CLI] `arc-node-execution` limits `eth_callMany` work per JSON-RPC request.**
  - `--arc.rpc.max-call-many-calls` (default `10`) caps the calls in one JSON-RPC request. The budget covers every bundle and, in a batch, every entry, so batching cannot multiply it. Each `eth_callMany` costs at least one call even when it runs nothing, so at the default one batch holds at most 10 `eth_callMany` entries.
  - `--arc.rpc.max-call-many-state-overrides` (default `100`) caps the accounts a single `eth_callMany` may override. It applies to each `eth_callMany` on its own, not to a batch total.
  - A request above either limit is rejected with JSON-RPC error `-32602` before any call runs. A batch is rejected as a whole.
  - Clients that send larger requests must split them, or the operator must raise the limits. Both values must be `>= 1`.
- **[CLI] `arc-node-execution` limits the calldata an RPC tracer records per transaction.**
  - `--arc.rpc.max-trace-bytes` (default `67108864`, 64 MiB) bounds the calldata a tracer copies while tracing one transaction, including the calldata of internal calls.
  - It applies to every RPC that runs with an inspector: `debug_trace*`, `trace_*`, `ots_*`, and `eth_createAccessList`.
  - A transaction past the limit fails with `trace byte limit of <BYTES> exceeded` instead of returning a partial trace. Block execution is unaffected.
  - Operators who serve heavy tracing workloads can raise the limit. The value must be `>= 1`.
- **[Config] `arc-node-execution` aborts database read transactions after 60 seconds.**
  - The `node` subcommand now defaults `--db.read-transaction-timeout` to `60` seconds. The previous default was reth's `300`.
  - A read transaction held longer is aborted, and the request that opened it fails. This is usually a slow `eth_call`, `eth_getLogs`, or trace over old state.
  - Archive or tracing nodes that need longer reads can set the flag explicitly. `0` removes the limit and is not recommended on public endpoints. Operators who already set the flag are unaffected.
- **[Config] `arc-node-consensus` value-sync defaults changed.**
  - `ARC_SYNC_BATCH_SIZE`: from `10` to `5` heights per request.
  - `ARC_SYNC_REQUEST_TIMEOUT`: from `1s` to `10s`.
  - `ARC_SYNC_MAX_RESPONSE_SIZE`: from `10 MiB` to `16 MiB`. The same limit now also bounds P2P RPC transport messages.
  - Operators who set these variables explicitly are unaffected.
  - Nodes on earlier releases keep syncing from v0.8.2 nodes. A request for more heights than the serving node's batch size is shortened to that size, not refused.
- **[Config] `arc-node-consensus` limits inbound sync requests from peers.**
  - `ARC_SYNC_INBOUND_RATE_LIMIT_WINDOW` (default `10s`): the window over which the node counts sync requests from each peer.
  - `ARC_SYNC_MAX_INBOUND_REQUESTS_PER_WINDOW` (default `1000`): sync requests the node accepts from one peer in each window.
  - `ARC_SYNC_MAX_CONCURRENT_INBOUND_REQUESTS` (default `1`): sync requests the node serves at once.
  - `ARC_SYNC_MAX_PENDING_INBOUND_REQUESTS` (default `128`): sync requests that wait for a free slot. `0` disables the queue.
  - The node answers requests over the per-peer rate, or beyond the pending queue, with an empty response. No action is needed to keep the defaults.
- **[Config] `ARC_CONSENSUS_WAL_REPLAY_DELAY` was removed.**
  - `arc-node-consensus` no longer reads this variable, and the setting no longer exists. A value that is still set is ignored.
  - Remove it from the consensus environment.

## [v0.8.1]

**Changes:** [v0.8.0...v0.8.1](https://github.com/circlefin/arc-node/compare/v0.8.0...v0.8.1) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.8.1)

No breaking changes in this release.

## [v0.8.0]

**Changes:** [v0.7.3...v0.8.0](https://github.com/circlefin/arc-node/compare/v0.7.3...v0.8.0) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.8.0)

### For Node Operators

- **[Format] JSON-RPC error text on insufficient-balance `eth_call` / `eth_estimateGas` changed with the reth 2.2 / revm 38 upgrade.**
  - Value exceeds balance: the error previously read `insufficient funds for gas * price + value`; it now reflects revm 38's `OutOfFunds` variant.
  - Simple (EOA-to-EOA) transfer with insufficient balance: reth 2.2 runs these RPC paths with `disable_fee_charge`, so the basic-transfer shortcut no longer applies the caller gas-allowance cap. The surfaced error shifted from `Missing or invalid parameters` to `gas required exceeds allowance`.
  - Neither string is a stable API contract, but tooling that matches JSON-RPC error text on these paths must update its patterns. No consensus-affecting behavior changed; only the RPC error surface.
- **[CLI] `arc-node-consensus` admin RPC routes are disabled by default.**
  - The unauthenticated `POST` and `DELETE /persistent-peers` routes are no longer mounted unless `--rpc.admin` is provided.
  - Read-only RPC routes are unaffected.
  - Operators using persistent-peer mutation routes must add `--rpc.admin` and restrict the RPC listener to a trusted interface.
- **[Config] Explicit invalid CL environment values now fail startup.**
  - Unset or empty `ARC_*` configuration variables continue to use their defaults.
  - Malformed values, and zero values for resource sizes, counts, timeouts, or thresholds that must be positive, now abort startup instead of silently falling back.
  - Correct or remove invalid environment values before upgrading.
- **[CLI] `arc-node-execution` denylist configuration flags were removed and denylist checks are mandatory.**
  - Remove `--arc.denylist.enabled`, `--arc.denylist.address`, and `--arc.denylist.storage-slot`. `--arc.denylist.addresses-exclusions` remains available.
- **[CLI] `arc-snapshots download` no longer defaults `--chain` to testnet and adds an execution manifest profile.**
  - Add `--chain <CHAIN>` for automatic URL resolution. For a manifest URL, also add `--el-profile <minimal|full|archive>`.
- **[Config] Execution-layer pruning presets now inject a 128-block pruning interval.**
  - `node --full` and `node --minimal` previously injected `5000`. Set `--prune.block-interval=5000` explicitly to retain that schedule.

## [v0.7.3]

**Changes:** [v0.7.2...v0.7.3](https://github.com/circlefin/arc-node/compare/v0.7.2...v0.7.3) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.7.3)

No breaking changes in this release.

## [v0.7.2]

**Changes:** [v0.7.1...v0.7.2](https://github.com/circlefin/arc-node/compare/v0.7.1...v0.7.2) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.7.2)

*Note: testnet node operators must use v0.7.2 before timestamp `1781791200` (2026-06-18 14:00:00 UTC), when Zero7 activates on testnet. Earlier versions are not supported.*

### For Node Operators

- **[Config] `arc-node-execution`: JSON-RPC gas cap default lowered.**
  - Old (`v0.7.1`): `--rpc.gascap` default `50000000` (Reth stock default).
  - New (`v0.7.2`): `--rpc.gascap` default `30000000`.
  - `eth_call` and `eth_estimateGas` requests that need more than 30M gas now fail with a gas-cap error. Pass `--rpc.gascap 50000000` (or higher) to restore the previous budget. Operators who never set the flag and do not rely on calls above 30M gas are unaffected.

- **[CLI] `arc-node-execution`: replay-unprotected (pre-EIP-155) transactions are rejected over JSON-RPC by default.**
  - Old (`v0.7.1`): pre-EIP-155 (replay-unprotected) transactions were accepted over JSON-RPC.
  - New (`v0.7.2`): they are rejected by default with "only replay-protected (EIP-155) transactions allowed over RPC".
  - Adds `--arc.rpc.allow-unprotected-txs` (default `false`); set it to accept legacy unprotected transactions over RPC.

- **[Config] `arc-node-execution`: JSON-RPC batch requests are capped.**
  - Old (`v0.7.1`): no limit on the number of entries in a JSON-RPC batch request.
  - New (`v0.7.2`): `--arc.rpc.max-batch-entries` defaults to `100`; oversized batches are rejected with JSON-RPC error `-32600` before any per-entry handler runs. A value of `0` is rejected so the cap cannot be silently disabled.
  - Operators whose tooling submits larger batches must raise `--arc.rpc.max-batch-entries <COUNT>`.

- **[Config] `arc-node-execution`: the invalid-transaction list is enabled by default.**
  - Old (`v0.7.1`): `--invalid-tx-list-enable` default `false`.
  - New (`v0.7.2`): default `true`. On a payload-builder panic, all pending transactions are added to the list and removed from the mempool; resubmit them after investigating the panic.
  - Opt out with `--invalid-tx-list-enable=false`.

## [v0.7.1]

**Changes:** [v0.7.0...v0.7.1](https://github.com/circlefin/arc-node/compare/v0.7.0...v0.7.1) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.7.1)

*Note: testnet node operators must use v0.7.1 before timestamp `1779894517` (2026-05-27 15:08:37 UTC), when Zero5/Zero6 activate on testnet. Earlier versions are not supported.*

### For Node Operators

- **[Config] `arc-node-execution`: EL RPC connection defaults tightened.**
  - `--rpc.max-connections` default: `500` -> `250`.
  - `--rpc.max-subscriptions-per-connection` default: `1024` -> `32`.
  - Both flags remain accepted on `arc-node-execution`; operators that need the previous behavior must pass them explicitly. The new defaults bound a WebSocket subscription fan-out memory pressure path; real-world clients typically multiplex around five subscriptions per socket and are unaffected.

## [v0.7.0]

**Changes:** [v0.6.0...v0.7.0](https://github.com/circlefin/arc-node/compare/v0.6.0...v0.7.0) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.7.0)

*Note: mainnet node operators must use v0.7.0. Earlier versions are not supported.*

### For Node Operators

- **[CLI] `arc-node-execution`: pending-tx flag rename and default flip.**
  - Old (`v0.6.0`): `--arc.hide-pending-txs` (opt-in to hide, default exposed).
  - New (`v0.7.0`): `--arc.expose-pending-txs` (opt-in to expose, default hidden).
  - Adds `--public-api`, a convenience flag for externally-exposed nodes that forces hiding and warns if `--http.api` / `--ws.api` expose namespaces outside `{eth, net, web3, rpc}`.
  - Nodes that relied on the default exposure must now pass `--arc.expose-pending-txs` or adopt the new secure-by-default behavior.

- **[Config] `arc-node-consensus`: `--execution-persistence-backpressure-threshold=0` is rejected at startup.**
  - Old (`v0.6.0`): `0` was accepted and caused indefinite stalling.
  - New (`v0.7.0`): the value must be `> 0`; the CL refuses to start otherwise.
  - Backpressure trigger semantics also changed: the gap now triggers when it *reaches* the threshold (previously *exceeds*).
  - The default (`16`) is unchanged. Only operators who set this flag explicitly to `0` (now rejected) or who monitor the exact threshold value need to act.

- **[Config] CL default `--log-level` changed from `debug` to `info`.**
  Not a config syntax change, but a behavior change that affects log volume and content. Pass `--log-level debug` explicitly if your tooling depends on debug-level output.

- **[Format] libp2p protocol identifiers on mainnet are Arc-branded.**
  - The CL on mainnet (chain id `5042`) advertises Arc-branded libp2p protocol IDs from v0.7.0. A pre-v0.7.0 CL **cannot** peer with a v0.7.0 CL on mainnet.
  - Operators must upgrade all mainnet nodes before or simultaneously with the v0.7.0 rollout; staged rollouts that leave a subset of nodes on `v0.6.x` will fragment the mainnet mesh.
  - Testnet (`5042002`) protocol IDs are unchanged in this release.

- **[Format] Address and public-key rendering uniformly switched to `0x`-prefixed lowercase hex.**
  - Logs, metrics, and JSON-RPC responses now use a single canonical format (signatures continue to use Base64). EIP-55 checksums are not used; Prometheus labels are case-sensitive.
  - Log parsers, alerting rules, and dashboards built against the previous mixed formats (EIP-55 checksummed, non-prefixed hex, etc.) must be updated.

### For Validators

- **[CLI] `arc-node-consensus`: `--validator` is required for block signing and voting.**
  - The CL now runs as a non-voting full node unless `--validator` is explicitly set.
  - The flag did not exist in `v0.6.0`. Validator operators upgrading from `v0.6.0` must add `--validator` to their startup command or they will stop participating in consensus.

- **[CLI] `arc-node-consensus`: `--suggested-fee-recipient` is required when `--validator` is set.**
  - Enforced at startup. Omitting the recipient with `--validator` set causes the binary to refuse to start.
  - **Important**: this address is where block rewards (tx fees, in USDC) collect after successful proposals are made.
  - Example:

    ```
    arc-node-consensus start \
      --validator \
      --suggested-fee-recipient 0xYOUR_ADDRESS \
      ...
    ```

## [v0.6.0]

Baseline -- initial public open-source release. Treat the [`v0.6.0`](https://github.com/circlefin/arc-node/releases/tag/v0.6.0) tag as the reference point for subsequent breaking-change notes. No breaking-change entries are recorded for this release.
