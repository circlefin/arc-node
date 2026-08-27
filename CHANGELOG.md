# Changelog

All notable changes to arc-node are documented in this file.

## [v0.8.0]

**Changes:** [v0.7.3...v0.8.0](https://github.com/circlefin/arc-node/compare/v0.7.3...v0.8.0) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.8.0)

*Note: testnet node operators must use a version supporting Zero8 before timestamp `1788447600` (2026-09-03 15:00:00 UTC), when Zero8 activates on testnet. Earlier versions are not supported.*

*Note: mainnet node operators must use a version supporting Zero7/Zero8 before timestamp `1789052400` (2026-09-10 15:00:00 UTC), when Zero7/Zero8 activate on mainnet. Earlier versions are not supported.*

### For Node Operators

- **[Format] JSON-RPC error text for insufficient-balance `eth_call` and `eth_estimateGas` calls changed with the reth 2.2 / revm 38 upgrade.** Transactions whose value or gas cost exceeds the sender balance now surface revm 38's `OutOfFunds` text (previously `insufficient funds for gas * price + value`); simple EOA-to-EOA transfers with insufficient balance now surface `gas required exceeds allowance` (previously `Missing or invalid parameters`). Tooling that matches on these strings must update its patterns. No consensus-affecting behavior changed; only the RPC error surface. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v080) for migration details.
- **[CLI] `arc-node-consensus` admin RPC routes are disabled by default.** The new `--rpc.admin` flag enables the unauthenticated `POST` and `DELETE /persistent-peers` routes; read-only RPC routes remain available without it. Enable admin routes only on a trusted interface. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v080) for migration details.
- **[Config] Explicit invalid CL environment values now fail startup.** Eleven value-sync, consensus-queue, discovery, and remote-signing tunables can now be overridden through `ARC_*` environment variables. Unset or empty variables retain their defaults; malformed values and zero values for settings that must be positive abort startup. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v080) for migration details.
- **[CLI] Arc denylist checks are mandatory and their contract configuration is chain-derived.** Remove `--arc.denylist.enabled`, `--arc.denylist.address`, and `--arc.denylist.storage-slot`; `--arc.denylist.addresses-exclusions` remains available. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v080) for migration details.
- **[CLI] `arc-snapshots download` no longer defaults `--chain` to testnet and requires `--el-profile` for execution manifest restores.** Supply the chain for automatic URL resolution and supply both options for a manifest URL. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v080) for migration details.
- **[Config] Execution-layer pruning presets now inject `--prune.block-interval=128` instead of `5000`.** This applies to the `node` subcommand when `--full` or `--minimal` is used without an explicit interval. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v080) for migration details.

### Features

- [EL] Upgrade to reth 2.2.0 and revm 38; brings EVM improvements and reth Storage V2 support. Nodes running existing V1 data may optionally migrate in place with `arc-node-execution db migrate-v2` (offline, one-way); the v0.8.0 binary runs V1 data without migration
- [EL] Add ordered transaction relay failover for follow nodes through `--arc.tx.relays` and `--arc.tx.relays.timeout`
- [EL] Add reth V2 modular execution snapshot restores with explicit minimal, full, and archive profiles
- [CL] Add `arc-node-consensus db migrate` command (alias: `upgrade`) to upgrade the consensus database schema to the latest version; migrations run automatically at node startup, so the manual command is a recovery tool for failed auto-migrations. Pass `--dry-run` to preview changes without committing
- [CL] Add forward height-range queries to the `/commit`, `/misbehavior-evidence`, `/proposal-monitor`, and `/invalid-payloads` endpoints, with negotiated response compression
- [CL] Add gzip compression negotiation to the CL `/metrics` endpoint
- [CL] Deprecate the RPC/HTTP transport between CL and EL in favor of IPC; RPC configurations now emit a startup warning ahead of removal in v0.9.0
- [CL] Close completed proposal-part stream keys so resurfaced duplicate parts cannot consume streaming capacity
- [CL] Add `arc_malachite_app_transient_validation_errors_count` metrics for transient proposal-validation failures
- [CL] Record proposal-monitor latency and outcome data when proposal parts arrive before round 0 starts

### Fixes

- [CL] Exit with non-zero status when the EL IPC connection closes unexpectedly, enabling container orchestrators to restart the node automatically
- [CL] Generate a fresh JWT token per Engine API request to prevent token staleness during long-running consensus operations
- [CL] Reject proposal stream messages carrying an invalid Fin sequence number
- [CL] Retry EL chain-identity resolution for up to 30 seconds during startup, then exit non-zero for orchestrator restart instead of hanging indefinitely
- [CL] Refuse to restream blocks without their original proposal signature
- [CL] Reject payloads with non-canonical block hashes before storing them as undecided blocks
- [CL] Let a proposer decline the round when `getPayload` exceeds the proposal deadline instead of crashing the consensus application
- [CL] Fail fast when multiple locally built blocks exist for the same height and round
- [CL] Bound CL RPC request bodies, execution time, and concurrency to prevent slow requests from exhausting node resources
- [CL] Treat invalid-payload forensic persistence failures as diagnostic errors instead of aborting consensus-critical processing
- [CL] Stop the Malachite Node actor before application teardown during SIGTERM shutdown to avoid spurious consensus errors
- [CL] Bind each execution payload to its consensus height and the block finalized at the previous height
- [CL] Key pending proposal parts by proposer, proof-of-lock round, and signature so distinct proposal streams cannot alias in storage
- [CL] Select the expected proposer from the proposal parts' round when validating streamed proposals
- [CL] Treat `engine_newPayload` internal JSON-RPC errors as no-verdict outcomes instead of permanently recording a payload as invalid
- [EL] Gate corrected EIP-161 deletion of synthetic empty accounts behind Zero8, preserving historical pre-Zero8 state roots
- [EL] Reject Engine API payloads containing withdrawals because Arc does not support withdrawals
- [EL] Evict permanently un-includable transactions from the transaction pool after payload construction rejects them
- [EL] Preserve transaction-warm status when evaluating `SELFDESTRUCT` targets
- [EL] Validate transaction size before recovering EIP-7702 authorities for denylist checks
- [EL] Keep the sparse-trie state hook active through post-block writes so committed state roots include those writes
- [EL] Reject a block beneficiary that was self-destructed during the same block
- [EL] Apply pending-transaction subscription filtering when `eth_subscribe` uses object-form parameters
- [EL] Classify beneficiary blocklist read failures as internal execution errors instead of deterministic transaction failures
- [Shared] Update h2 to reject unbounded empty DATA-frame streams and remove the related denial-of-service vulnerability
- [Shared] Update ruint to correct overflow flags and shift-amount handling used by EVM big-integer operations

## [v0.7.3]

**Changes:** [v0.7.2...v0.7.3](https://github.com/circlefin/arc-node/compare/v0.7.2...v0.7.3) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.7.3)

### Features

- [CL] Add consensus and RPC Prometheus metrics on the `/metrics` endpoint: `arc_malachite_app_consensus_round_missed` (counter, labeled by the missed round's proposer), `arc_malachite_app_rpc_request_time`, and app-request channel queue-time, process-time, and full-channel-rejection metrics
- [CL] Process application and RPC requests concurrently with consensus messages so slow database-backed reads do not delay consensus progress
- [EL] Add the Zero8 hardfork (dormant). Once active on a chain, a rejected delegatecall into a stateful Arc precompile is charged the uniform 200-gas early-revert penalty, matching the penalty already applied to other precompile authorization and validation reverts. Zero8 is not scheduled on any public chain
- [EL] `arc_getCertificate` now accepts a hex-encoded quantity string (e.g. `"0x7"`) for the block-height parameter in addition to a plain decimal number, aligning with the EVM hex-quantity convention used by `eth_getBlockByNumber`

### Fixes

- [CL] Avoid querying the EL for a validator set when a stored certificate already includes proposer metadata
- [EL] Complete the pending-block RPC filter so permissionless callers can no longer read the proposed pre-finalization block through case-variant `pending` tags, previously-uncovered block-content methods, or object-style block parameters
- [Shared] Remediate cargo audit advisories via dependency bumps

### Docs

Full documentation tree at this release: [`arc-node` v0.7.3 docs](https://github.com/circlefin/arc-node/tree/v0.7.3/docs). New or updated topics in this release:

- Revise the base-fee validation ADR to match the implemented code: rename `elasticity_multiplier` to `inverse_elasticity_multiplier`, drop the unimplemented proportional check, and clarify the `k_rate` description

## [v0.7.2]

**Changes:** [v0.7.1...v0.7.2](https://github.com/circlefin/arc-node/compare/v0.7.1...v0.7.2) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.7.2)

*Note: testnet node operators must use v0.7.2 before timestamp `1781791200` (2026-06-18 14:00:00 UTC), when Zero7 activates on testnet. Earlier versions are not supported.*

### For Node Operators

- **[Config] EL JSON-RPC gas cap default lowered to 30M.** `--rpc.gascap` now defaults to `30000000` (previously `50000000`). `eth_call` and `eth_estimateGas` requests that need more than 30M gas now fail unless the cap is raised. Operators who never set `--rpc.gascap` and do not rely on calls above 30M gas are unaffected; pass `--rpc.gascap <N>` to restore a larger budget. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v072) for migration details.
- **[CLI] Replay-unprotected (pre-EIP-155) transactions are rejected over JSON-RPC by default.** The new `--arc.rpc.allow-unprotected-txs` flag defaults to `false`; raw transaction submission returns "only replay-protected (EIP-155) transactions allowed over RPC", matching Geth. Operators that must accept legacy unprotected transactions over RPC pass `--arc.rpc.allow-unprotected-txs`. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v072) for migration details.
- **[Config] JSON-RPC batch requests are capped at 100 entries by default.** The new `--arc.rpc.max-batch-entries` flag defaults to `100`; batches with more entries are rejected with JSON-RPC error `-32600` before any per-entry handler runs, and `0` is rejected so the cap cannot be silently disabled. Operators whose tooling sends larger batches raise `--arc.rpc.max-batch-entries <COUNT>`. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v072) for migration details.
- **[Config] The invalid-transaction list is enabled by default.** `--invalid-tx-list-enable` now defaults to `true` (previously `false`). On a payload-builder panic, all pending transactions are added to the list and removed from the mempool; resubmit them after investigating. Opt out with `--invalid-tx-list-enable=false`. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v072) for migration details.

### Fixes

- [CL] Stop stale proposal streams from blocking live proposals
- [EL] Preserve cold account state when evaluating the SELFDESTRUCT beneficiary check
- [EL] Charge gas before performing storage I/O in precompile helpers
- [EL] Fail closed in the SELFDESTRUCT handler when a blocklist read fails

### Docs

Full documentation tree at this release: [`arc-node` v0.7.2 docs](https://github.com/circlefin/arc-node/tree/v0.7.2/docs). New or updated topics in this release:

- Add an RPC provider node section to the public node-operation guide

## [v0.7.1]

**Changes:** [v0.7.0...v0.7.1](https://github.com/circlefin/arc-node/compare/v0.7.0...v0.7.1) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.7.1)

*Note: testnet node operators must use v0.7.1 before timestamp `1779894517` (2026-05-27 15:08:37 UTC), when Zero5/Zero6 activate on testnet. Earlier versions are not supported.*

### For Node Operators

- **[Config] EL RPC connection defaults tightened.** `--rpc.max-connections` default lowered from `500` to `250`; `--rpc.max-subscriptions-per-connection` default lowered from `1024` to `32`. Operators running tooling that opens many concurrent WebSocket connections, or that subscribes more than 32 times on a single connection, must raise these explicitly on the `arc-node-execution` command line. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v071) for migration details.

### Features

- [Shared] Enable global keccak cache and asm-backed keccak
- [Spammer] Expose per-run telemetry from the spammer

### Fixes

- [EL] Avoid double-hashing initCode on CREATE2 with non-zero value
- [EL] Activate testnet Zero5/Zero6 by timestamp instead of block height to preserve fork-id compatibility across mixed-version peers

## [v0.7.0]

**Changes:** [v0.6.0...v0.7.0](https://github.com/circlefin/arc-node/compare/v0.6.0...v0.7.0) -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.7.0)

### For Node Operators

*Note: mainnet node operators must use v0.7.0. Earlier versions are not supported.*

- **[Config] Pending transactions are hidden from RPC by default.** Renamed `--arc.hide-pending-txs` (opt-in to hide) to `--arc.expose-pending-txs` (opt-in to expose) and flipped the default. Added `--public-api`, a convenience flag for externally-exposed nodes that forces hiding and warns if `--http.api` / `--ws.api` expose namespaces outside `{eth, net, web3, rpc}`.
- **[Config] CL default log level changed from `debug` to `info`.** Pass `--log-level debug` explicitly if your monitoring depends on debug-level output.
- **[Config] `--follow` no longer requires `--follow.endpoint` for standard chains.** The CL resolves a default RPC endpoint from the chain id at startup; run `arc-node-consensus start --help` for the per-chain defaults. Explicit `--follow.endpoint` still takes precedence.
- **[CLI] New `--txpool.rebroadcast-interval` flag (EL).** Periodic re-announcement of pending transactions to peers (default `60` seconds, `0` to disable). Recovers from missed gossip announcements.
- **[CLI] New `--pprof.heap-prof` flag (EL and CL).** Enables jemalloc heap profiling on demand when built with `--features pprof`. Heap profiling is now inactive by default.
- **[Config] `--execution-persistence-backpressure-threshold` must be greater than zero** and triggers when the gap *reaches* the threshold (previously *exceeds*). Default is `16` and is unchanged; operators who never set this flag explicitly are unaffected. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v070) for migration details.
- **[API] New `/ready` readiness probe** and `sync_state` field on the CL `/status` endpoint.
- **[CLI] New `arc-node-consensus db rollback` command** (alias: `unwind`) for operator-driven rollback. Dry-run by default; pass `--execute` to commit. `--num-heights` and `--to-height` are mutually exclusive.
- **[Config] Arc mainnet is a named chainspec** (`--chain arc-mainnet`, chain id `5042`).

### For Validators

- **[CLI] `--validator` flag is required** for a CL to participate in block signing and voting. Without it, the node runs as a non-voting full node. This flag did not exist in `v0.6.0`. See [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v070).
- **[CLI] `--suggested-fee-recipient` is required when `--validator` is set.** It is required that this address be set / non-zero.

  ```
  arc-node-consensus start \
    --validator \
    --suggested-fee-recipient 0xYOUR_ADDRESS \
    ...
  ```

- **[Format] Equivocation evidence log levels raised**: persistence failures promoted from `warn` to `error`, successful persistence from `info` to `warn`. Both include validator addresses for forensics.
- **[API] Validator public key exposed in the CL `/status` endpoint.**
- **[Format] Address and public-key rendering uniformly switched to `0x`-prefixed lowercase hex.** Logs, metrics, and JSON-RPC responses use this single canonical format (signatures continue to use Base64). Tooling that parsed EIP-55 checksummed addresses or non-prefixed hex must be updated.

### Features

- [CL] Add `--validator` configuration flag
- [CL] Require `--suggested-fee-recipient` when `--validator` is set
- [CL] Resolve default follow endpoint from chain id
- [CL] Add `/ready` readiness probe and `sync_state` to `/status`
- [CL] Add `db rollback` command (alias: `unwind`) for operator-driven rollback
- [CL] Raise equivocation evidence log levels
- [CL] Add versioned wire encoding for consensus network messages
- [CL] Harden validator-set decoding against malformed public keys
- [CL] Count and log invalid payloads across all storage paths
- [CL] Model consensus fork history; narrow `ForkCondition` to height-only
- [CL] Use Arc-branded libp2p protocol names on mainnet; see [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v070) for cross-version peering implications
- [CL] Detect EL crashes over IPC and log a diagnostic instead of silently stalling
- [EL] Implement **Zero7** hardfork: `CallFrom` subcall precompile, `Multicall3From`, `Memo`
- [EL] Apply EIP-2929 warm/cold pricing to precompile account loads; see [BREAKING_CHANGES.md](./BREAKING_CHANGES.md#v070) for the gas-estimation impact
- [EL] Add periodic transaction rebroadcast to recover from missed gossip
- [EL] Unconditionally use validator-provided beneficiary addresses
- [EL] Apply `0xef` non-deployable prefix (EIP-3541) to Arc precompile addresses in genesis, preventing EOAs or contracts from being deployed at those addresses
- [EL] EEST fixture runner for EVM spec test validation
- [EL] Register `arc-mainnet` as a named chainspec (Zero3-Zero6 active at block `0`)
- [EL] Finalize mainnet genesis with USDC admin roles, denylist, prefunded ops wallet
- [Shared] Add `--pprof.heap-prof` flag for on-demand heap profiling
- [Shared] Uniformize address and key rendering to `0x`-prefixed lowercase hex
- [Contracts] ProtocolConfig upgrade scripts; remove `rewardBeneficiary` field (proposer-provided fee recipient is authoritative)
- [Contracts] Deploy denylist contract on testnet (mainnet ships it pre-deployed in genesis)
- [Quake] Testnet orchestrator improvements: web topology viewer, node-group support in `load` / `spam`, mesh/health/performance/sanity test runner with report generation, manifest fields for EL/CL CPU and memory limits and `block_gas_limit`
- [Bench] `arc-engine-bench` with IPC and RPC engine transports
- [Bench] Nightly engine bench workflow
- [Spammer] Cache gas estimates for ERC-20 and Guzzler transactions
- [Spammer] Reuse with parallel nonce resync and reduced request timeout
- [Spammer] Improved send-stall visibility

### Fixes

- [CL] Prevent stream eviction by colluding validators
- [CL] Propagate `pol_round` as `valid_round` in assembled blocks
- [CL] On restream, look up block by hash and preserve `round` / `valid_round`
- [CL] Fetch validator set at `certificate_height - 1` in `get_certificate_info`
- [CL] Align `RemoteSigningProvider` Ed25519 verification with Malachite
- [CL] Bound repeated proto fields to prevent unbounded allocation
- [CL] Use checked arithmetic in `total_voting_power()`
- [CL] Account for EL earliest block in `GetHistoryMinHeight`
- [CL] Skip persistence wait during sync when block is already present or height decided
- [CL] Acknowledge `AppMsg::Decided` so sync advertises a new tip
- [CL] Mark undecided block `Invalid` on engine validation errors; persist verdict
- [CL] Surface duplicate Init/Fin proposal parts as `InsertResult::Invalid`
- [CL] Improve EL/CL height-mismatch error with actionable guidance
- [CL] Update backpressure semantics
- [EL] Suppress pool-based pending-tx leaks in RPC middleware
- [EL] Charge EIP-2929 cold account access cost in `CallFrom` subcalls
- [EL] Blocklist SLOADs are unmetered on native value transfers
- [EL] Consume all gas for subcall in static context
- [EL] Charge gas for subcall completion phase
- [EL] Strictly decode ABI parameters in precompiles
- [EL] Implement EIP-2200 sentry for `SSTORE`
- [EL] Apply new-account surcharge via precompiles
- [EL] Extend early-revert penalty to auth reverts in Zero6
- [EL] Drop redundant `SLOAD` charge in `storeGasValuesCall` under Zero6
- [EL] Align `totalSupply` input validation with other precompiles
- [EL] Revert child state when subcall precompile rejects
- [EL] Resolve EIP-7702 delegation when loading subcall target bytecode
- [EL] Check EIP-7702 authorization-list authorities against the denylist
- [EL] `DenylistedAddressError` should not penalize peers
- [EL] Include base fee in payload builder fee totals
- [EL] Use checked arithmetic for cumulative gas accounting in payload builder
- [EL] Panic on missing subcall continuation instead of reverting
- [Shared] Remediate cargo audit advisories
- [Contracts] Capture `Multicall3From` precompile reverts instead of propagating
- [Quake] Validate manifest flags against consensus binary CLI struct; decouple monitoring lifecycle from `clean` and `restart`
- [Spammer] Lift gas-fee caps above testnet base-fee ceiling
- [Spammer] Fix raw tx encoding, TCP backpressure drain, zero-latency warning

### Docs

Full documentation tree at this release: [`arc-node` v0.7.0 docs](https://github.com/circlefin/arc-node/tree/v0.7.0/docs). New or updated topics in this release:

- Add Docker instructions for running an Arc node
- Add single-host monitoring guide for Arc EL + CL

## [v0.6.0]

**Released:** 2026-04-08 -- [release notes](https://github.com/circlefin/arc-node/releases/tag/v0.6.0)

Initial public open-source release of `arc-node`. Baseline for subsequent changelog entries.
