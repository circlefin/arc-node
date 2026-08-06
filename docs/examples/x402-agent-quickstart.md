# X402 Payment-Gated API on Arc Testnet

This example shows how to build a payment-gated API on Arc Testnet using the [x402 protocol](https://x402.org) and Circle USDC. AI agents can autonomously pay for API access without human intervention.

## Overview
## Prerequisites

- Node.js v22+
- A testnet wallet with USDC on Base Sepolia
- Arc Testnet RPC: `https://rpc.testnet.arc.network`

Get testnet USDC from [faucet.circle.com](https://faucet.circle.com).

## Installation

```bash
mkdir arc-x402-api && cd arc-x402-api
npm init -y
npm install express x402-express x402-fetch viem dotenv
```

## Environment Setup

```bash
# .env
ARC_RPC_URL=https://rpc.testnet.arc.network
ARC_CHAIN_ID=5042002
SELLER_PRIVATE_KEY=your_testnet_private_key
PORT=3000
```

## Server (Seller)

```javascript
const express = require("express");
const { paymentMiddleware } = require("x402-express");
const dotenv = require("dotenv");

dotenv.config();

const app = express();
const SELLER_ADDRESS = "0xYOUR_SELLER_ADDRESS";

// Protect endpoints with X402
app.use(paymentMiddleware(
  SELLER_ADDRESS,
  {
    "/api/arc-data": {
      price: "$0.001",
      network: "base-sepolia",
      config: { description: "Arc Testnet network data" },
    },
  },
  { facilitatorUrl: "https://facilitator.circle.com" }
));

// Free endpoint
app.get("/health", (req, res) => {
  res.json({ status: "ok", network: "Arc Testnet", chainId: 5042002 });
});

// Paid endpoint
app.get("/api/arc-data", (req, res) => {
  res.json({
    network: "Arc Testnet",
    chainId: 5042002,
    gasToken: "USDC",
    finality: "sub-second deterministic",
    contracts: {
      USDC: "0x3600000000000000000000000000000000000000",
      EURC: "0x3600000000000000000000000000000000000001",
    },
  });
});

app.listen(3000, () => console.log("Server running on port 3000"));
```

## Agent (Buyer)

```javascript
const { createWalletClient, http } = require("viem");
const { baseSepolia } = require("viem/chains");
const { privateKeyToAccount } = require("viem/accounts");
const { wrapFetchWithPayment } = require("x402-fetch");
const dotenv = require("dotenv");

dotenv.config();

async function runAgent() {
  const account = privateKeyToAccount(`0x${process.env.SELLER_PRIVATE_KEY}`);

  const walletClient = createWalletClient({
    account,
    chain: baseSepolia,
    transport: http(),
  });

  const { default: nodeFetch } = await import("node-fetch");
  const fetchWithPayment = wrapFetchWithPayment(nodeFetch, walletClient);

  // Agent automatically pays and fetches data
  const response = await fetchWithPayment("http://localhost:3000/api/arc-data");
  const data = await response.json();
  console.log("Data received:", data);
}

runAgent().catch(console.error);
```

## Running the Example

Start the server:
```bash
node server.js
```

Run the agent:
```bash
node agent.js
```

Expected output:
## How X402 Works on Arc

1. Agent sends a request to the paid endpoint
2. Server responds with **HTTP 402 Payment Required**
3. Agent reads the payment requirements (price, asset, payTo address)
4. Agent signs a USDC transfer authorization using **EIP-3009**
5. Agent resends the request with the `X-PAYMENT` header
6. Circle's facilitator verifies and settles the payment
7. Server returns the requested data

## Arc Testnet Contract Addresses

| Contract | Address |
|----------|---------|
| USDC | `0x3600000000000000000000000000000000000000` |
| EURC | `0x3600000000000000000000000000000000000001` |
| Gateway Wallet | `0x0077777d7EBA4688BDeF3E311b846F25870A19B9` |

## Full Example Repository

[github.com/consumeobeydie/arc-agent-api](https://github.com/consumeobeydie/arc-agent-api)

## Resources

- [Arc Documentation](https://docs.arc.io)
- [X402 Protocol](https://x402.org)
- [Circle Developer Console](https://console.circle.com)
- [Arc Testnet Explorer](https://testnet.arcscan.app)
- [Circle Faucet](https://faucet.circle.com)
