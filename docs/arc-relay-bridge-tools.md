# Arc Relay Bridge — Infrastructure Tools Reference

A cross-chain USDC relay bridge built on Circle's CCTP V2, deployable to Arc Testnet. This document describes the six infrastructure tools built alongside the bridge, their design decisions, and key Arc Testnet quirks discovered during development.

**Repository:** [osr21/arc-relay-bridge](https://github.com/osr21/arc-relay-bridge)

---

## Tool A — USDC Transfer Indexer + REST API

A background worker inside the API server indexes all native USDC transfer events on Arc Testnet in real time.

### How it works

Listens to EIP-7708 logs emitted by the native USDC emitter (`0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFE`) — the single authoritative source for USDC balance changes on Arc. Resumes from DB on restart; caps startup catch-up at 500 blocks; polls every 2 s (~4 Arc blocks per cycle given ~0.48 s block time).

**Key decision:** index only native emitter logs (not the ERC-20 mirror) — Arc USDC emitter fires two logs per transfer (native + ERC-20 mirror); indexing both causes double-counting.

### REST endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/indexer/transfers` | Paginated USDC transfer list. Filters: `from`, `to`, `type` (NATIVE_SEND/MINT/BURN), `txHash` |
| GET | `/api/indexer/stats` | Aggregate stats: last indexed block, totals, CCTP domain breakdown |

### Database schema

Table `usdc_transfers`: `id` (txHash-logIndex), `blockNumber`, `blockTime`, `txHash`, `logIndex`, `fromAddress`, `toAddress`, `amountNative` (18-decimal string), `amountUsdc` (6-decimal numeric), `transferType`, `isNativeLog`

---

## Tool B — Transaction Memo Indexer

Indexes `Memo` events from Arc's on-chain memo contract (`0x5294E9927c3306DcBaDb03fe70b92e01cCede505`). Each memo event carries a sender, target, callDataHash, memoId, and raw memo bytes. Attempts UTF-8 decoding of the payload and stores the human-readable text if valid.

### REST endpoint

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/indexer/memos` | Paginated memo list. Filters: `sender`, `target`, `memoId`, `txHash` |

### Database schema

Table `memo_events`: `id`, `blockNumber`, `blockTime`, `txHash`, `sender`, `target`, `callDataHash`, `memoId`, `memoData` (hex), `memoText` (decoded UTF-8 or null), `memoIndex`

---

## Tool C — The Graph Subgraph

A Graph Protocol subgraph (`scripts/src/graph/`) indexing the same Arc Testnet events via The Graph's decentralised infrastructure — an alternative / complement to the centralised indexer.

### Files

```
scripts/src/graph/
  schema.graphql          — UsdcTransfer, MemoEvent, CctpBurn, CctpMint entity types
  subgraph.yaml           — data sources: NativeUsdcEmitter, MemoContract, TokenMessenger, MessageTransmitter
  src/
    usdc.ts               — AssemblyScript mapping for Transfer events
    memo.ts               — AssemblyScript mapping for Memo events
    cctp.ts               — AssemblyScript mapping for DepositForBurn + MintAndWithdraw
  abis/
    ERC20.json, Memo.json, TokenMessenger.json, MessageTransmitter.json
```

### Deployment

```bash
cd scripts/src/graph
npm install
graph auth --studio <deploy-key>
graph codegen && graph build
graph deploy --studio arc-relay-bridge
```

---

## Tool D — Contract Compatibility Linter

A CLI linter (`scripts/src/lint-arc-contract.ts`) that detects Arc Testnet compatibility issues in Solidity contracts before deployment. Fetches on-chain bytecode and runs 9 static checks.

### Checks

| Code | Check |
|------|-------|
| ARC-001 | PUSH0 opcode detected — contract requires `evmVersion: paris` |
| ARC-002 | 2-immutable constructor init pattern (`0x60c0`) silently reverts — use `constant` for second immutable |
| ARC-003 | `nonReentrant` on `validatePaymasterUserOp` — violates ERC-7562, Pimlico rejects |
| ARC-004 | V1 CCTP 4-param `depositForBurn` selector — must use V2 7-param version |
| ARC-005 | Missing `onlyEntryPoint` guard on paymaster functions |
| ARC-006 | Constructor arity mismatch for known paymaster pattern |
| ARC-007 | Stale gas price usage — requires ≥30% premium on Arc |
| ARC-008 | `eth_estimateGas` reliance for complex calls — use hardcoded gas limits |
| ARC-009 | `wallet_switchEthereumChain` race condition pattern |

### Usage

```bash
pnpm --filter @workspace/scripts run lint-contract <contract-address>
```

---

## Tool E — CCTP Bridge Analytics

### Indexer component

The same background worker also indexes CCTP V2 events:

- **Burns:** `DepositForBurn` from TokenMessenger (`0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA`)
- **Mints:** `MintAndWithdraw` from TokenMessenger (co-emitted with `MessageReceived` from MessageTransmitter `0xE737e5cEBEEBa77EFE34D4aa090756590b1CE275`)

**CCTP V2 ABI facts confirmed from on-chain decoding:**

- `DepositForBurn` topic: `0x0c8c1cbdc5190613ebd485511d4e2812cfa45eecb79d845893331fedad5130a5`
  - `burnToken` is the **1st indexed param** (not `nonce` as in the Circle standard)
  - Includes additional `uint32 minFinalityThreshold` and `bytes hookData` params
  - Full signature: `DepositForBurn(address,uint256,address,bytes32,uint32,bytes32,bytes32,uint256,uint32,bytes)`
- `MintAndWithdraw` replaces `MessageReceived` for mint indexing
  - topic: `0x50c55e915134d457debfa58eb6f4342956f8b0616d51a89a3659360178e1ab63`
  - Source domain extracted from raw `MessageReceived` V2 data word 0 (first 32 bytes = `uint32 sourceDomain`)
- `minFinalityThreshold` per chain: Arc=2000 (finalized), Ethereum Sepolia/Base Sepolia/Avalanche Fuji=0 (fast testnet)

**REST endpoints:**

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/indexer/cctp/burns` | CCTP burn events. Filters: `depositor`, `destDomain`, `status` (PENDING/COMPLETE/STUCK) |
| GET | `/api/indexer/cctp/mints` | CCTP mint events. Filters: `recipient`, `remoteDomain` |

### Dashboard component

Real-time dark-themed analytics dashboard (`artifacts/arc-analytics/`) built with React 19 + Vite + Recharts + shadcn/ui, served at `/arc-analytics/`.

| Route | Purpose |
|-------|---------|
| `/` | Overview — last indexed block, event totals, CCTP domain breakdown bar charts (auto-refresh every 10 s) |
| `/transfers` | Searchable/filterable USDC transfer explorer with pagination |
| `/memos` | Memo event explorer — decoded text + raw hex tooltip |
| `/bridge` | CCTP Burns tab (attestation status badges) + Mints tab |

---

## Tool G — APS (Arc Privacy Sector) SDK

TypeScript stubs providing typed interfaces and client stubs for the Arc Privacy Sector (`lib/arc-privacy-sdk/`) — built ahead of APS mainnet deployment to enable downstream integration.

```typescript
import { ArcPrivacyClient } from "@workspace/arc-privacy-sdk";
const aps = new ArcPrivacyClient({ rpcUrl: "https://rpc.testnet.arc.network", chainId: 5042002 });
const note = await aps.shield(signer, usdcAmount);
```

---

## Arc Testnet gotchas discovered during development

| Issue | Detail |
|-------|--------|
| **Primary RPC rate-limits getLogs** | `rpc.testnet.arc.network` returns error −32011 for `eth_getLogs`. Use `https://arc-testnet.drpc.org` for indexer log fetching. |
| **drpc.org free-tier batch limit** | drpc.org rejects JSON-RPC batches of >3 requests (error code 31). Set `batchMaxCount: 1` on ethers.js provider. |
| **drpc.org getLogs range limit** | Free tier rejects `eth_getLogs` ranges >10,000 blocks (error code 35). Keep history window ≤9,000 blocks. |
| **Double logs on USDC transfer** | The native USDC emitter fires two logs per ERC-20 Transfer (native + ERC-20 mirror). Index only native logs. |
| **CCTP V2 ABI is a superset** | Arc's CCTP V2 `depositForBurn` has 7 params; V1 4-param selector (`0x6fd3504e`) silently reverts. Always use the V2 selector (`0x8e0250ee`). |
| **Arc USDC: 18-decimal native, 6-decimal ERC-20** | Arc native USDC uses 18 decimals for gas accounting, but the ERC-20 interface uses 6 decimals. Always use the ERC-20 interface for amounts. |
| **Block time ~0.48 s** | Arc produces ~4 blocks per 2-second poll cycle. Use `blockNumber` (not `timestamp`) as the stable ordering key for indexers — no reorgs. |
| **PUSH0 opcode** | Arc Testnet rejects contracts compiled with `evmVersion >= shanghai`. Always compile with `evmVersion: "paris"`. |
| **2-immutable constructor** | Contracts with 2 immutable variables use `0x60c0` init bytecode which silently reverts on Arc. Keep the second variable as a `constant`. |
| **Arc explorer** | Use `https://testnet.arcscan.app` — previous URLs (`explorer.testnet.arc.network`, `explorer.arc.io`) are dead. |

---

## Contract addresses

| Contract | Address (all chains) |
|----------|---------------------|
| TokenMessenger (CCTP V2) | `0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA` |
| MessageTransmitter (CCTP V2) | `0xE737e5cEBEEBa77EFE34D4aa090756590b1CE275` |
| Arc Testnet USDC | `0x3600000000000000000000000000000000000000` |
| Native USDC Emitter | `0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFE` |
| Memo Contract | `0x5294E9927c3306DcBaDb03fe70b92e01cCede505` |

**Supported chains:** Arc Testnet (domain 26, chain ID 5042002), Ethereum Sepolia (domain 0), Base Sepolia (domain 6), Avalanche Fuji (domain 1)
