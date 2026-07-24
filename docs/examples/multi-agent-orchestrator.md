# Multi-Agent Orchestrator on Arc Testnet

This example shows how to deploy and run a fully autonomous multi-agent system on Arc Testnet. Agents autonomously hire sub-agents, complete missions, and split payments on-chain.

## Overview

The MultiAgentOrchestrator contract enables:
- Agent registration with capability and reputation tracking
- Mission creation with native USDC budget
- Autonomous sub-agent hiring by assigned agents
- Automatic payment splitting on mission completion
- Reputation increase for successful agents

## Contract on Arc Testnet

| Contract | Address |
|----------|---------|
| MultiAgentOrchestrator | 0xe81f5BA4181eA29061C3C229c8D6EB4cFE56639C |

## Multi-Agent Flow

createMission() -> assignMission() -> hireSubAgent() -> completeMission()

1. Owner creates mission with USDC budget
2. Agent A (Orchestrator) is assigned the mission
3. Agent A autonomously hires Agent B (Worker)
4. Agent B completes the mission and submits deliverable
5. Payments split automatically: Agent A gets remainder, Agent B gets sub-budget

## Live Results on Arc Testnet

- Mission ID: 2
- Status: Completed
- Total Budget: 0.5 ETH
- Agent A payout: 0.35 ETH (Reputation: 110)
- Agent B payout: 0.15 ETH (Reputation: 103)

## Key Transactions

- Mission created: https://testnet.arcscan.app/tx/0xbc042e8bcf0a683a73ee455bc7070bb116a51933241aff751820ce4224603e23
- Mission assigned: https://testnet.arcscan.app/tx/0x2e2485a569cb4863e35f52c186088005050464dd942d4974311f8fe057863af2
- Agent B hired: https://testnet.arcscan.app/tx/0x7d867ac5d99fbdc6889f94c44ac57279052c2806a3ac86a985dc6e5583cee494
- Mission completed: https://testnet.arcscan.app/tx/0x63dcb3e1f0ff7fc80b5e74a0920617430fc1995e5f0107905c835e47e0eb6790

## GitHub Repositories

- Smart contract: https://github.com/consumeobeydie/arc-multi-agent
- Full implementation: https://github.com/consumeobeydie/arc-agent-api

## Resources

- Arc Testnet Explorer: https://testnet.arcscan.app
- Arc Documentation: https://docs.arc.network
- Circle Developer Console: https://console.circle.com
