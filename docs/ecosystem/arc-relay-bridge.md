# Arc Relay Bridge

  A cross-chain USDC bridge built on [Circle's CCTP V2](https://developers.circle.com/stablecoins/cctp-getting-started), deployable to Arc Testnet. Burn USDC on a source chain and natively mint it on the destination — no wrapped tokens, no liquidity pools.

  > **Live testnet app:** deployed on Replit  
  > **Protocol:** Circle CCTP V2 (all chains)  
  > **Status:** Testnet — supports Arc Testnet ↔ Ethereum Sepolia, Base Sepolia, Avalanche Fuji

  ---

  ## Overview

  Arc Relay Bridge is a frontend-only dapp that implements the full CCTP V2 burn-and-mint flow client-side via ethers.js and MetaMask. Users connect their wallet, choose source and destination chains, enter a USDC amount, and execute a 4-step bridge:

  1. **Approve** — ERC-20 approval of the fee router (or TokenMessenger directly if no fee)
  2. **Burn** — `depositForBurn` on the source chain TokenMessenger
  3. **Attest** — Poll Circle Iris API until attestation is ready
  4. **Mint** — `receiveMessage` on the destination chain MessageTransmitter

  The UI shows real-time step progress and links each transaction to the appropriate block explorer.

  ---

  ## Supported Chains (Testnet)

  | Chain | CCTP Domain | Chain ID | Explorer |
  |---|---|---|---|
  | Arc Testnet | 26 | 5042002 | [testnet.arcscan.app](https://testnet.arcscan.app) |
  | Ethereum Sepolia | 0 | 11155111 | [sepolia.etherscan.io](https://sepolia.etherscan.io) |
  | Base Sepolia | 6 | 84532 | [sepolia.basescan.org](https://sepolia.basescan.org) |
  | Avalanche Fuji | 1 | 43113 | [testnet.snowtrace.io](https://testnet.snowtrace.io) |

  ---

  ## Contract Addresses

  All chains use the same CCTP V2 contract addresses:

  | Contract | Address |
  |---|---|
  | TokenMessenger (CCTP V2) | `0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA` |
  | MessageTransmitter (CCTP V2) | `0xE737e5cEBEEBa77EFE34D4aa090756590b1CE275` |

  USDC addresses per chain:

  | Chain | USDC Address |
  |---|---|
  | Arc Testnet | `0x3600000000000000000000000000000000000000` |
  | Ethereum Sepolia | `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` |
  | Base Sepolia | `0x036CbD53842c5426634e7929541eC2318f3dCF7e` |
  | Avalanche Fuji | `0x5425890298aed601595a70AB815c96711a31Bc65` |

  ---

  ## Technical Notes

  ### CCTP V2 on Arc

  Arc implements CCTP V2 with a few important characteristics:

  - **All calls use the 7-parameter `depositForBurn`** (selector `0x8e0250ee`). The V1 4-parameter variant will revert.
  - **Nonce encoding:** Arc's MessageTransmitter encodes the nonce as a 32-byte field (always `0x00…00` in practice). Replay protection uses `keccak256(messageBytes)` as the `usedNonces` key — not `keccak256(sourceDomain, nonce)` as in CCTP V1.
  - **Message length:** 376 bytes (140-byte header + 236-byte body).
  - **`depositForBurn` parameters:** `destinationCaller = bytes32(0)` (permissionless relay), `maxFee = 0`, `minFinalityThreshold` varies by chain (Arc: 2000 finalized; others: 1000 safe).
  - **USDC decimals:** Arc's native USDC at `0x360…0` uses 18 decimals for gas accounting but exposes a standard 6-decimal ERC-20 interface — always use the ERC-20 interface.

  ### Attestation

  The bridge polls [Circle Iris API (sandbox)](https://iris-api-sandbox.circle.com) every 5 seconds for up to 20 minutes. Attestation timing:

  - Arc → other chains: ~1–3 minutes (finalized finality)
  - Other chains → Arc: ~2–5 minutes (safe finality)

  ---

  ## Stack

  - **Frontend:** React 19 + Vite + Tailwind CSS (shadcn/ui)
  - **Web3:** ethers.js v6 (BrowserProvider + Contract)
  - **Protocol:** Circle CCTP V2
  - **Attestation:** Circle Iris API (sandbox endpoint)
  - **No backend:** all bridge logic runs client-side

  ---

  ## Fee Router

  The bridge includes an optional protocol fee (0.3%) collected via an immutable FeeRouter contract deployed on each chain. Security properties:

  - Immutable `usdc` and `tokenMessenger` addresses (no caller-supplied addresses)
  - Reentrancy lock
  - Zero `mintRecipient` check
  - `rescueTokens` return-value check

  | Chain | FeeRouter Address |
  |---|---|
  | Arc Testnet | `0x8256a1e1f8971448b49dA0F55b8A1BB6557eA8FC` |
  | Ethereum Sepolia | `0x5B1F511ed4dF76f369671BF1c4aCF0dD84CC0804` |
  | Base Sepolia | `0x8d4B57eD464df10414Dde3ADC2E403a01ebc50d8` |
  | Avalanche Fuji | `0x64D160b7E91e78e52dFc0e8829640E32A919164C` |

  ---

  ## Getting Started

  1. Get testnet USDC from the [Circle Faucet](https://faucet.circle.com)
  2. Connect MetaMask to the source chain
  3. Select source and destination chains
  4. Enter an amount and click **Bridge**
  5. Approve and sign each step as prompted
  6. Monitor real-time progress — the bridge will auto-switch your wallet to the destination chain for the final mint

  ---

  ## Links

  - [Arc Developer Docs](https://docs.arc.network)
  - [CCTP Documentation](https://developers.circle.com/stablecoins/cctp-getting-started)
  - [Circle Faucet](https://faucet.circle.com)
  - [Arc Testnet Explorer](https://testnet.arcscan.app)
  