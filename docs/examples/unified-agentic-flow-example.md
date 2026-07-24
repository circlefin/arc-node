# Unified Agentic Flow on Arc Testnet

This example combines X402 payments, ERC-8004 identity, and ERC-8183 jobs into a single automated agent flow on Arc Testnet.

## Overview

A single agent wallet runs three protocols in sequence:

- PHASE 1: X402 micropayment to access Arc Testnet data
- PHASE 2: ERC-8004 onchain identity registration  
- PHASE 3: ERC-8183 job creation, funding, and completion

## Architecture

Main Agent (0x54b4B44749a95070560509B6Ec0be501665CcF63)
- Pays for API access with USDC (X402)
- Registers onchain identity (ERC-8004)
- Creates and completes a job (ERC-8183)

## Contracts Used

| Contract | Address |
|----------|---------|
| IdentityRegistry (ERC-8004) | 0x8004A818BFB912233c491871b3d84c89A494BD9e |
| ReputationRegistry (ERC-8004) | 0x8004B663056A597Dffe9eCcC1965A193B7388713 |
| ValidationRegistry (ERC-8004) | 0x8004Cb1BF31DAf7788923b405b754f57acEB4272 |
| AgenticCommerce (ERC-8183) | 0x0747EEf0706327138c69792bF28Cd525089e4583 |

## Live Results on Arc Testnet

Main Agent: 0x54b4B44749a95070560509B6Ec0be501665CcF63
Agent ID: 69828
Job ID: 110935
Job Status: Completed
Budget: 1 USDC

## Flow Summary

PHASE 1 - X402 Payment:
- Agent sends request to payment-gated API
- Receives HTTP 402 Payment Required
- Signs USDC payment automatically
- Receives Arc Testnet data

PHASE 2 - ERC-8004 Identity:
- Registers identity on IdentityRegistry
- Records reputation score (95)
- Completes validation flow

PHASE 3 - ERC-8183 Job:
- Creates job on AgenticCommerce contract
- Funds 1 USDC into escrow
- Provider submits deliverable hash
- Evaluator completes job, USDC released

## Full Example Repository

https://github.com/consumeobeydie/arc-agent-api

## Resources

- X402 Protocol: https://x402.org
- Arc ERC-8004 Docs: https://docs.arc.network/arc/tutorials/register-your-first-ai-agent
- Arc ERC-8183 Docs: https://docs.arc.network/arc/tutorials/create-your-first-erc-8183-job
- Arc Testnet Explorer: https://testnet.arcscan.app
