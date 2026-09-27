# Arc Pay — Reference DApp: Gasless Stablecoin Transfers

  **Repository:** https://github.com/osr21/arc-pay  
  **Live App:** https://245e1045-f370-4cc1-8fc0-1ba2bc475ce3-00-3pv56lyfqrmzj.picard.replit.dev/  
  **Live RPC Gateway:** https://245e1045-f370-4cc1-8fc0-1ba2bc475ce3-00-3pv56lyfqrmzj.picard.replit.dev/rpc  
  **Network:** Arc Testnet (Chain ID: 5042002)

  ---

  ## Overview

  Arc Pay is a peer-to-peer stablecoin transfer DApp built on Arc Network Testnet. It demonstrates how to build a production-quality DApp using Arc's unique architecture — specifically the fact that USDC is the native gas token, which enables completely gasless UX for end users.

  Users send USDC or EURC to any address with near-zero fees (~$0.01) using a gasless relay. The signing experience requires no ETH, no separate gas token, and no approval transaction — just a single off-chain signature.

  ---

  ## What This DApp Demonstrates

  ### 1. Gasless Transfers via EIP-2612 Permit

  Arc's defining feature is USDC as the native gas token. Arc Pay exploits this to build a fully gasless transfer flow:

  1. User signs an EIP-2612 `permit` message off-chain (zero gas, zero cost)
  2. The permit authorises a relayer to spend the user's USDC
  3. The relayer submits `transferFrom` on-chain, paying gas in USDC from its own wallet
  4. The recipient receives USDC minus a 0.1% protocol fee; the relayer is reimbursed for gas

  This is possible on Arc because the relayer only needs USDC — no ETH or other gas token. The entire arc of gasless UX is enabled by Arc's native token design.

  ```
  User (signs permit off-chain, free)
      │
      ▼
  Relayer server receives permit signature
      │
      ▼
  Relayer calls USDC.transferFrom(from, to, amount) on-chain
      │
      ▼
  Recipient receives USDC — user never paid gas
  ```

  **Contracts used:**
  - USDC: `0x3600000000000000000000000000000000000000` (ERC-20, 6 decimals, EIP-2612 permit supported)
  - EURC: `0x89B50855Aa3bE2F677cD6303Cec089B5F319D72a` (ERC-20, 6 decimals)

  No custom contracts were deployed — the entire gasless flow runs on top of the existing USDC contract's permit functionality.

  ### 2. Arc RPC Gateway

  A public, load-balanced JSON-RPC proxy for Arc Testnet:

  ```
  POST https://245e1045-f370-4cc1-8fc0-1ba2bc475ce3-00-3pv56lyfqrmzj.picard.replit.dev/rpc
  ```

  Drop-in replacement for `https://rpc.testnet.arc.network` with:

  - **Load balancing** across `arc-public` and `drpc` with latency-weighted selection
  - **Automatic failover** — unhealthy providers excluded after 3 consecutive errors, re-enabled after 30s
  - **Response caching** — `eth_chainId` / `net_version` (1 hour), `eth_blockNumber` (2s), gas prices (5s)
  - **Method allowlist** — 30 safe methods allowed; `admin_*`, `debug_*`, `miner_*`, `personal_*` blocked
  - **API key auth** for `eth_sendRawTransaction` only; all read methods are open
  - **Batch support** (max 100 per batch)
  - **Security hardening** — timing-safe API key comparison, JSON-RPC error format for all failures

  **Wallet / MetaMask setup:**

  | Field | Value |
  |---|---|
  | Network name | Arc Testnet |
  | RPC URL | `https://245e1045-f370-4cc1-8fc0-1ba2bc475ce3-00-3pv56lyfqrmzj.picard.replit.dev/rpc` |
  | Chain ID | `5042002` |
  | Currency symbol | `USDC` |
  | Block explorer | `https://testnet.arcscan.app` |

  **Monitoring endpoints:**

  | Path | Description |
  |---|---|
  | `/rpc/healthz` | Liveness probe — `{ "status": "ok" }` |
  | `/rpc/health` | Per-provider health, error counts, last error |
  | `/rpc/metrics` | Latency averages, success rates, request counts |
  | `/rpc/info` | Self-documenting: network config, methods, usage |

  ### 3. On-Chain Event Indexing

  Arc Pay indexes `Transfer` events from the last 50,000 blocks on wallet connect, reconciling on-chain state with a local Postgres database. This gives users complete transfer history that persists across sessions.

  ### 4. Direct Wallet Integration

  Wallet connection uses `window.ethereum` directly — no wagmi, RainbowKit, or Web3Modal dependency. The implementation is lean (~150 lines) and serves as a reference for minimal wallet integration on Arc.

  ---

  ## Stack

  | Layer | Technology |
  |---|---|
  | Frontend | React, Vite, TypeScript |
  | Backend | Node.js, Express 5 |
  | Database | PostgreSQL, Drizzle ORM |
  | Blockchain | viem (Arc Testnet client) |
  | API contract | OpenAPI 3.1, Orval codegen |
  | Monorepo | pnpm workspaces |

  ---

  ## Key Developer Findings

  The following issues were discovered during development and filed as GitHub issues on this repository:

  | Issue | Description |
  |---|---|
  | [#90](https://github.com/circlefin/arc-node/issues/90) | `rpc.testnet.arc.network` missing CORS headers — browser DApps cannot call it directly |
  | [#91](https://github.com/circlefin/arc-node/issues/91) | USDC decimal ambiguity: 18 decimals (native gas) vs 6 decimals (ERC-20) — undocumented |
  | [#92](https://github.com/circlefin/arc-node/issues/92) | No documented fallback RPC endpoints — single point of failure for all testnet developers |
  | [#93](https://github.com/circlefin/arc-node/issues/93) | EIP-2612 permit support on USDC not documented — critical capability for gasless DApps |
  | [#94](https://github.com/circlefin/arc-node/issues/94) | Chain ID inconsistency — some resources reference 1516, actual testnet chain ID is 5042002 |

  ---

  ## Critical: USDC Decimal Handling

  The most common source of silent bugs when building on Arc:

  | Context | Decimals | 1 USDC in wei |
  |---|---|---|
  | Native gas token (`eth_getBalance`, `gasPrice`, receipts) | **18** | `1000000000000000000` |
  | ERC-20 contract (`balanceOf`, `transfer`, `transferFrom`) | **6** | `1000000` |

  When displaying gas costs to users, always convert: `nativeGasCost / 10^12 = displayValue`.  
  When calling ERC-20 methods, always use 6 decimals.

  ---

  ## Architecture

  ```
  Browser (Arc Pay)
        │
        ├──► GET  /api/balance        ─► viem ──► Arc Testnet RPC
        ├──► GET  /api/transfers      ─► PostgreSQL
        ├──► POST /api/transfers      ─► PostgreSQL
        ├──► POST /api/transfers/sync ─► viem ──► eth_getLogs ──► PostgreSQL
        ├──► GET  /api/stats          ─► PostgreSQL
        └──► POST /api/relay          ─► viem ──► USDC.transferFrom (permit)

  Arc RPC Gateway (/rpc)
        │
        ├──► arc-public (https://rpc.testnet.arc.network)
        └──► drpc       (https://arc-testnet.drpc.org)
  ```

  ---

  ## Getting Started with Arc Testnet

  1. **Get testnet USDC** at [faucet.circle.com](https://faucet.circle.com) — select Arc Testnet + USDC
  2. **Add Arc Testnet to MetaMask** using the RPC gateway URL above (Chain ID: 5042002)
  3. **Connect wallet** at the Arc Pay app and start sending

  The relayer wallet must hold USDC for gasless relay to function. The relayer address is `0xf4a14B84108885AF2f18843DD18761706e47d5F6`.

  ---

  ## Source Code

  Full source at [github.com/osr21/arc-pay](https://github.com/osr21/arc-pay). Key files:

  | File | Description |
  |---|---|
  | `artifacts/arc-gateway/src/routes/rpc.ts` | RPC gateway core — routing, auth, caching, batch |
  | `artifacts/api-server/src/routes/relay.ts` | EIP-2612 permit relay implementation |
  | `artifacts/arc-pay/src/pages/send.tsx` | Gasless transfer UI |
  | `docs/RELAY.md` | Full gasless relay technical documentation |
  | `docs/RPC_GATEWAY.md` | RPC gateway complete API reference |
  | `docs/ARCHITECTURE.md` | Full system architecture |
  