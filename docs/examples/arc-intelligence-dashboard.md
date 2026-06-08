# Arc Intelligence Dashboard

A real-time Next.js dashboard for Arc Testnet that visualizes X402 payments, ERC-8004 agent identity, ERC-8183 jobs, and deployed contracts.

## Live Demo

https://arc-intelligence-dashboard.vercel.app

## Features

- Real-time USDC balance (refreshed every 10 seconds)
- Live Arc Testnet block number
- Transaction count for main agent address
- Agentic Stack status (X402, ERC-8004, ERC-8183)
- 15 deployed Solidity contracts with explorer links
- circlefin/arc-node contribution history

## Tech Stack

- Next.js 16 with TypeScript
- Tailwind CSS
- viem for Arc Testnet RPC
- Deployed on Vercel

## Quick Start

npm install
npm run dev

## Arc Testnet Configuration

const arcTestnet = {
  id: 5042002,
  name: "Arc Testnet",
  rpcUrls: { default: { http: ["https://rpc.testnet.arc.network"] } },
  blockExplorers: { default: { name: "Arcscan", url: "https://testnet.arcscan.app" } },
};

## Data Fetching

The dashboard uses viem createPublicClient to fetch:
- ERC-20 USDC balance via balanceOf()
- Latest block number via getBlockNumber()
- Transaction count via getTransactionCount()

All three queries run concurrently with Promise.all and refresh every 10 seconds.

## GitHub Repository

https://github.com/consumeobeydie/arc-intelligence-dashboard

## Resources

- Arc Docs: https://docs.arc.network
- Arc Testnet Explorer: https://testnet.arcscan.app
- viem: https://viem.sh
