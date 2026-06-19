# Breaking Changes

Records breaking changes between tagged public releases of [`arc-node`](https://github.com/circlefin/arc-node).

Each bullet is prefixed with a flag identifying the kind of breaking change:

- `[CLI]` -- CLI flag added, renamed, removed, or made required.
- `[Config]` -- default value, environment variable, or manifest field change.
- `[Format]` -- log, metric label, or serialized output format change that breaks parsers.

Entries are split by audience. A change appears under `### For Validators` when validator-mode operation must change; otherwise it appears under `### For Node Operators`. A change requiring both audiences to act appears in both sections (rare).

Compare and release-notes links resolve once the corresponding tag is published at [`circlefin/arc-node`](https://github.com/circlefin/arc-node).

## [Unreleased]

### For Node Operators

- **[Format] JSON-RPC error text on insufficient-balance `eth_call` / `eth_estimateGas` changed with the reth 2.2 / revm 38 upgrade.**
  - Value exceeds balance: the error previously read `insufficient funds for gas * price + value`; it now reflects revm 38's `OutOfFunds` variant.
  - Simple (EOA-to-EOA) transfer with insufficient balance: reth 2.2 runs these RPC paths with `disable_fee_charge`, so the basic-transfer shortcut no longer applies the caller gas-allowance cap. The surfaced error shifted from `Missing or invalid parameters` to `gas required exceeds allowance`.
  - Neither string is a stable API contract, but tooling that matches JSON-RPC error text on these paths must update its patterns. No consensus-affecting behavior changed; only the RPC error surface.

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
