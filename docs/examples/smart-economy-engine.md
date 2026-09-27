# Arc Smart Economy Engine Example

This example shows how to build a self-sustaining autonomous agent economy on Arc Testnet that uses an X402-gated market oracle to make dynamic vault strategy decisions.

## Overview

The Smart Economy Engine combines three Arc Testnet primitives into one autonomous loop:

1. MultiAgentOrchestrator — agents hire each other and complete missions
2. ArcUSDCVault (ERC-4626) — mission earnings automatically enter a yield-bearing vault
3. X402 micropayments — the engine pays 0.001 USDC per cycle to purchase a market signal from an oracle

Each cycle, the engine buys a BULLISH/NEUTRAL/BEARISH signal and adjusts its behavior:
- BULLISH: larger mission budget (1.5x), distribute vault yield
- NEUTRAL: normal mission budget (1.0x)
- BEARISH: smaller mission budget (0.5x), conserve capital

## Architecture

X402 Oracle (local server)
    |
    | pays 0.001 USDC per request
    v
Smart Economy Engine (Node.js)
    |-- reads signal: BULLISH / NEUTRAL / BEARISH
    |-- opens mission with dynamic budget
    |-- Agent A assigns, hires Agent B
    |-- Agent B completes mission
    |-- routes payout to ERC-4626 vault
    |-- if BULLISH + threshold: distributes yield

## Key Contracts on Arc Testnet

| Contract | Address |
|----------|---------|
| MultiAgentOrchestrator | 0xe81f5BA4181eA29061C3C229c8D6EB4cFE56639C |
| ArcUSDCVault (ERC-4626) | 0x6C13dA317B65474299F6fDee02daDd6626Eb2BFe |
| Memo precompile | 0x5294E9927c3306DcBaDb03fe70b92e01cCede505 |

## Live Results (3 cycles)

Cycle 1: BEARISH (score 1)  -> 1.5 USDC mission, vault: 13.00 -> 13.25 USDC
Cycle 2: BEARISH (score 39) -> 1.5 USDC mission, vault: 13.25 -> 13.50 USDC
Cycle 3: BULLISH (score 89) -> 4.5 USDC mission + yield distributed, vault: 13.50 -> 17.75 USDC

Final state:
- Vault Assets: 17.75 USDC (+36% from start)
- Agent B Shares: 10,750,000 avUSDC (+34%)
- Agent A Reputation: 145
- Agent B Reputation: 124
- Total Missions: 11

## X402 Oracle Flow

The oracle server exposes a paywall endpoint. Each cycle the engine:
1. Sends GET /premium/market -> receives 402 Payment Required
2. Generates a payment proof (keccak256 of payment intent)
3. Resends with X-PAYMENT header -> receives market signal JSON
4. Uses signal.action and signal.mission_multiplier for dynamic decisions

## Known Limitation: callWithMemo + gas estimation

The Memo precompile (callWithMemo) reverts during eth_estimateGas / eth_call simulation but executes successfully as a direct signed transaction. This affects any SDK that estimates gas before sending (viem writeContract, Circle Developer Controlled Wallets). See issue #189 for details.

Workaround: use cast send with raw calldata for Memo transactions.

## GitHub Repositories

- Smart Economy Engine: https://github.com/consumeobeydie/arc-agent-api/blob/main/src/smart-economy-engine.js
- X402 Oracle Server: https://github.com/consumeobeydie/hermes-arc-x402
- ERC-4626 Vault: https://github.com/consumeobeydie/arc-vault
- Multi-Agent Orchestrator: https://github.com/consumeobeydie/arc-multi-agent

## Resources

- Arc Testnet Explorer: https://testnet.arcscan.app
- Arc Docs: https://docs.arc.io
- ERC-4626 Standard: https://eips.ethereum.org/EIPS/eip-4626
