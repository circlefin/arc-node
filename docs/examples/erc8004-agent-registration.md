# ERC-8004 AI Agent Registration on Arc Testnet

This example shows how to register an AI agent with onchain identity, build reputation, and verify credentials using the ERC-8004 standard on Arc Testnet with Circle Developer-Controlled Wallets.

## Overview

ERC-8004 provides onchain identity and reputation for AI agents. Combined with x402 payments, it enables fully autonomous agents that can prove their identity, build trust, and transact on Arc Testnet.

## ERC-8004 Contracts on Arc Testnet

| Contract | Address |
|----------|---------|
| IdentityRegistry | `0x8004A818BFB912233c491871b3d84c89A494BD9e` |
| ReputationRegistry | `0x8004B663056A597Dffe9eCcC1965A193B7388713` |
| ValidationRegistry | `0x8004Cb1BF31DAf7788923b405b754f57acEB4272` |

## Prerequisites

- Node.js v22+
- Circle Developer Console account with API key
- Entity Secret registered in Circle Console

## Installation

```bash
mkdir arc-erc8004-agent && cd arc-erc8004-agent
npm init -y
npm install @circle-fin/developer-controlled-wallets viem dotenv
```

## Environment Setup

```bash
# .env
CIRCLE_API_KEY=your_circle_api_key
CIRCLE_ENTITY_SECRET=your_entity_secret
```

## Complete Implementation

```javascript
const { initiateDeveloperControlledWalletsClient } = require("@circle-fin/developer-controlled-wallets");
const { createPublicClient, http, parseAbiItem, keccak256, toHex } = require("viem");
require("dotenv").config();

const arcTestnet = {
  id: 5042002,
  name: "Arc Testnet",
  nativeCurrency: { name: "USDC", symbol: "USDC", decimals: 18 },
  rpcUrls: { default: { http: ["https://rpc.testnet.arc.network"] } },
  blockExplorers: { default: { name: "Arcscan", url: "https://testnet.arcscan.app" } },
  testnet: true,
};

const IDENTITY_REGISTRY = "0x8004A818BFB912233c491871b3d84c89A494BD9e";
const REPUTATION_REGISTRY = "0x8004B663056A597Dffe9eCcC1965A193B7388713";
const VALIDATION_REGISTRY = "0x8004Cb1BF31DAf7788923b405b754f57acEB4272";
const METADATA_URI = "ipfs://bafkreibdi6623n3xpf7ymk62ckb4bo75o3qemwkpfvp5i25j66itxvsoei";

const circleClient = initiateDeveloperControlledWalletsClient({
  apiKey: process.env.CIRCLE_API_KEY,
  entitySecret: process.env.CIRCLE_ENTITY_SECRET,
});

const publicClient = createPublicClient({
  chain: arcTestnet,
  transport: http(),
});

async function waitForTransaction(txId, label) {
  for (let i = 0; i < 30; i++) {
    await new Promise((r) => setTimeout(r, 2000));
    const { data } = await circleClient.getTransaction({ id: txId });
    if (data?.transaction?.state === "COMPLETE") return data.transaction.txHash;
    if (data?.transaction?.state === "FAILED") throw new Error(label + " failed");
  }
  throw new Error(label + " timed out");
}

async function main() {
  // Step 1: Create two wallets (owner + validator)
  const walletSet = await circleClient.createWalletSet({ name: "ERC8004 Agent Wallets" });
  const walletsResponse = await circleClient.createWallets({
    blockchains: ["ARC-TESTNET"],
    count: 2,
    walletSetId: walletSet.data?.walletSet?.id,
    accountType: "SCA",
  });

  const ownerWallet = walletsResponse.data?.wallets?.[0];
  const validatorWallet = walletsResponse.data?.wallets?.[1];
  console.log("Owner:    ", ownerWallet.address);
  console.log("Validator:", validatorWallet.address);

  // Step 2: Register agent identity
  const registerTx = await circleClient.createContractExecutionTransaction({
    walletAddress: ownerWallet.address,
    blockchain: "ARC-TESTNET",
    contractAddress: IDENTITY_REGISTRY,
    abiFunctionSignature: "register(string)",
    abiParameters: [METADATA_URI],
    fee: { type: "level", config: { feeLevel: "MEDIUM" } },
  });
  const registerHash = await waitForTransaction(registerTx.data?.id, "registration");
  console.log("Registered:", "https://testnet.arcscan.app/tx/" + registerHash);

  // Step 3: Get agent ID from Transfer event
  const latestBlock = await publicClient.getBlockNumber();
  const fromBlock = latestBlock > 10000n ? latestBlock - 10000n : 0n;
  const transferLogs = await publicClient.getLogs({
    address: IDENTITY_REGISTRY,
    event: parseAbiItem("event Transfer(address indexed from, address indexed to, uint256 indexed tokenId)"),
    args: { to: ownerWallet.address },
    fromBlock,
    toBlock: latestBlock,
  });
  const agentId = transferLogs[transferLogs.length - 1].args.tokenId.toString();
  console.log("Agent ID:", agentId);

  // Step 4: Record reputation (validator wallet)
  const tag = "x402_payment_successful";
  const feedbackHash = keccak256(toHex(tag));
  const reputationTx = await circleClient.createContractExecutionTransaction({
    walletAddress: validatorWallet.address,
    blockchain: "ARC-TESTNET",
    contractAddress: REPUTATION_REGISTRY,
    abiFunctionSignature: "giveFeedback(uint256,int128,uint8,string,string,string,string,bytes32)",
    abiParameters: [agentId, "95", "0", tag, "", "", "", feedbackHash],
    fee: { type: "level", config: { feeLevel: "MEDIUM" } },
  });
  await waitForTransaction(reputationTx.data?.id, "reputation");

  // Step 5: Request + respond to validation
  const requestHash = keccak256(toHex("validation_request_" + agentId));
  const validationReqTx = await circleClient.createContractExecutionTransaction({
    walletAddress: ownerWallet.address,
    blockchain: "ARC-TESTNET",
    contractAddress: VALIDATION_REGISTRY,
    abiFunctionSignature: "validationRequest(address,uint256,string,bytes32)",
    abiParameters: [validatorWallet.address, agentId, "ipfs://bafkreiexample", requestHash],
    fee: { type: "level", config: { feeLevel: "MEDIUM" } },
  });
  await waitForTransaction(validationReqTx.data?.id, "validation request");

  const validationResTx = await circleClient.createContractExecutionTransaction({
    walletAddress: validatorWallet.address,
    blockchain: "ARC-TESTNET",
    contractAddress: VALIDATION_REGISTRY,
    abiFunctionSignature: "validationResponse(bytes32,uint8,string,bytes32,string)",
    abiParameters: [requestHash, "100", "", "0x" + "0".repeat(64), "agent_verified"],
    fee: { type: "level", config: { feeLevel: "MEDIUM" } },
  });
  await waitForTransaction(validationResTx.data?.id, "validation response");

  console.log("Agent registration complete!");
  console.log("Explorer:", "https://testnet.arcscan.app/address/" + ownerWallet.address);
}

main().catch(console.error);
```

## Expected Output
## How It Works

1. **Two wallets** — owner registers the agent, validator records reputation (per ERC-8004, owners cannot self-attest)
2. **Identity registration** — mints an ERC-721 NFT on IdentityRegistry, giving the agent a unique onchain ID
3. **Reputation** — validator records feedback with a score and tag on ReputationRegistry
4. **Validation** — two-step request/response flow on ValidationRegistry proves the agent meets criteria

## Integration with X402

This ERC-8004 identity can be combined with x402 payments for a complete agentic flow:
See the full working example: [github.com/consumeobeydie/arc-agent-api](https://github.com/consumeobeydie/arc-agent-api)

## Resources

- [Arc ERC-8004 Docs](https://docs.arc.network/arc/tutorials/register-your-first-ai-agent)
- [Arc Testnet Explorer](https://testnet.arcscan.app)
- [Circle Developer Console](https://console.circle.com)
- [ERC-8004 Standard](https://eips.ethereum.org/EIPS/eip-8004)
