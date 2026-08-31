# ERC-8183 Developer Guide

This guide covers how to build applications on Arc using ERC-8183 (Agentic Commerce Protocol) and Circle Developer Controlled Wallets. It is based on real-world experience building [FreelanceArc](https://freelance-arc.vercel.app), a decentralized freelance marketplace deployed on Arc Testnet.

---

## What is ERC-8183?

ERC-8183 defines a minimal on-chain job lifecycle for agent commerce:

- `createJob()` — Client opens a job contract on-chain
- `setBudget()` — Provider accepts and sets the price
- `fund()` — Client locks USDC in escrow
- `submit()` — Provider submits cryptographic proof of delivery
- `complete()` — Client approves, USDC transfers automatically to provider

Every step is a real transaction, verifiable on ArcScan.

---

## Contract Addresses (Arc Testnet)

| Contract | Address |
|---|---|
| ERC-8183 AgenticCommerce | `0x0747EEf0706327138c69792bF28Cd525089e4583` |
| USDC (native) | `0x3600000000000000000000000000000000000000` |
| ERC-8004 IdentityRegistry | `0x8004A818BFB912233c491871b3d84c89A494BD9e` |
| ERC-8004 ReputationRegistry | `0x8004B663056A597Dffe9eCcC1965A193B7388713` |

---

## Arc Testnet Configuration

```typescript
import { defineChain } from "viem";

export const arcTestnet = defineChain({
  id: 923018,
  name: "Arc Testnet",
  nativeCurrency: {
    name: "USDC",
    symbol: "USDC",
    decimals: 18,
  },
  rpcUrls: {
    default: { http: ["https://rpc.testnet.arc.network"] },
  },
  blockExplorers: {
    default: {
      name: "ArcScan",
      url: "https://testnet.arcscan.app",
    },
  },
});
```

> **Important:** Arc Testnet uses USDC as the native gas token with **18 decimals** at the chain level. However, the ERC-20 USDC contract at `0x3600...` uses **6 decimals** for token transfers (e.g. escrow amounts). Use the correct decimal precision for each context to avoid failed transactions.

---

## Circle Developer Controlled Wallets

Arc integrates natively with Circle's wallet infrastructure. Use the Circle Developer Controlled Wallets SDK to manage wallets without handling private keys.

### Setup

```typescript
import { initiateDeveloperControlledWalletsClient } from "@circle-fin/developer-controlled-wallets";

const circle = initiateDeveloperControlledWalletsClient({
  apiKey: process.env.CIRCLE_API_KEY!,
  entitySecret: process.env.CIRCLE_ENTITY_SECRET!,
});
```

### Create Wallets

```typescript
const wallet = await circle.createWallets({
  blockchains: ["ARC-TESTNET"],
  count: 1,
});
```

### Execute Contract Calls

```typescript
const tx = await circle.createContractExecutionTransaction({
  walletAddress: "0x...",
  blockchain: "ARC-TESTNET",
  contractAddress: "0x0747EEf0706327138c69792bF28Cd525089e4583",
  abiFunctionSignature: "createJob(address,address,uint256,string,address)",
  abiParameters: [providerAddress, evaluatorAddress, expiredAt, description, hookAddress],
  fee: { type: "level", config: { feeLevel: "MEDIUM" } },
});
```

### Poll for Transaction Completion

```typescript
async function waitForTx(txId: string): Promise<string> {
  for (let i = 0; i < 40; i++) {
    await new Promise(r => setTimeout(r, 2000));
    const { data } = await circle.getTransaction({ id: txId });
    if (data?.transaction?.state === "COMPLETE") {
      return data.transaction.txHash!;
    }
    if (data?.transaction?.state === "FAILED") {
      throw new Error("Transaction failed");
    }
  }
  throw new Error("Transaction timeout");
}
```

---

## Full Job Lifecycle Example

The following TypeScript example demonstrates the complete ERC-8183 job lifecycle on Arc Testnet using Circle Developer Controlled Wallets and viem.

```typescript
import { createPublicClient, http, keccak256, toHex } from "viem";
import { arcTestnet } from "./arcTestnet";

const publicClient = createPublicClient({
  chain: arcTestnet,
  transport: http(),
});

const AGENTIC_COMMERCE = "0x0747EEf0706327138c69792bF28Cd525089e4583";
const USDC = "0x3600000000000000000000000000000000000000";

// Budget in USDC — 6 decimals for ERC-20 transfers
const budgetUsdc = 5;
const budgetInUnits = (budgetUsdc * 1_000_000).toString(); // "5000000"

// Step 1: Get current block for expiry
const block = await publicClient.getBlock();
const expiredAt = (block.timestamp + 3600n).toString(); // 1 hour from now

// Step 2: Create job
const createTx = await circle.createContractExecutionTransaction({
  walletAddress: clientAddress,
  blockchain: "ARC-TESTNET",
  contractAddress: AGENTIC_COMMERCE,
  abiFunctionSignature: "createJob(address,address,uint256,string,address)",
  abiParameters: [
    providerAddress,
    evaluatorAddress,
    expiredAt,
    "Build a React dashboard",
    "0x0000000000000000000000000000000000000000", // no hook
  ],
  fee: { type: "level", config: { feeLevel: "MEDIUM" } },
});
const createHash = await waitForTx(createTx.data!.id!);

// Step 3: Extract jobId from transaction receipt
const receipt = await publicClient.getTransactionReceipt({
  hash: createHash as `0x${string}`,
});
let jobId = "0";
for (const log of receipt.logs) {
  const topics = (log as any).topics;
  if (topics && topics.length >= 2) {
    jobId = BigInt(topics[1]).toString();
    break;
  }
}

// Step 4: Set budget (called by provider)
const budgetTx = await circle.createContractExecutionTransaction({
  walletAddress: providerAddress,
  blockchain: "ARC-TESTNET",
  contractAddress: AGENTIC_COMMERCE,
  abiFunctionSignature: "setBudget(uint256,uint256,bytes)",
  abiParameters: [jobId, budgetInUnits, "0x"],
  fee: { type: "level", config: { feeLevel: "MEDIUM" } },
});
await waitForTx(budgetTx.data!.id!);

// Step 5: Approve USDC spend
const approveTx = await circle.createContractExecutionTransaction({
  walletAddress: clientAddress,
  blockchain: "ARC-TESTNET",
  contractAddress: USDC,
  abiFunctionSignature: "approve(address,uint256)",
  abiParameters: [AGENTIC_COMMERCE, budgetInUnits],
  fee: { type: "level", config: { feeLevel: "MEDIUM" } },
});
await waitForTx(approveTx.data!.id!);

// Step 6: Fund escrow
const fundTx = await circle.createContractExecutionTransaction({
  walletAddress: clientAddress,
  blockchain: "ARC-TESTNET",
  contractAddress: AGENTIC_COMMERCE,
  abiFunctionSignature: "fund(uint256,bytes)",
  abiParameters: [jobId, "0x"],
  fee: { type: "level", config: { feeLevel: "MEDIUM" } },
});
await waitForTx(fundTx.data!.id!);

// Step 7: Submit deliverable (called by provider)
const deliverable = "https://github.com/example/deliverable";
const deliverableHash = keccak256(toHex(deliverable));
const submitTx = await circle.createContractExecutionTransaction({
  walletAddress: providerAddress,
  blockchain: "ARC-TESTNET",
  contractAddress: AGENTIC_COMMERCE,
  abiFunctionSignature: "submit(uint256,bytes32,bytes)",
  abiParameters: [jobId, deliverableHash, "0x"],
  fee: { type: "level", config: { feeLevel: "MEDIUM" } },
});
await waitForTx(submitTx.data!.id!);

// Step 8: Complete job and release payment
const reasonHash = keccak256(toHex("Work approved"));
const completeTx = await circle.createContractExecutionTransaction({
  walletAddress: clientAddress,
  blockchain: "ARC-TESTNET",
  contractAddress: AGENTIC_COMMERCE,
  abiFunctionSignature: "complete(uint256,bytes32,bytes)",
  abiParameters: [jobId, reasonHash, "0x"],
  fee: { type: "level", config: { feeLevel: "MEDIUM" } },
});
await waitForTx(completeTx.data!.id!);

console.log(`Job #${jobId} completed. ${budgetUsdc} USDC transferred to provider.`);
```

---

## Common Pitfalls

**USDC Decimals**
Arc Testnet USDC uses 18 decimals at the chain level but 6 decimals for ERC-20 token amounts. When calling `setBudget()` or `approve()`, use 6 decimals (`amount * 1_000_000`). The Circle Wallets SDK balance API may return amounts with 18 decimal precision — always check the `decimals` field in the token response.

**Transaction Polling**
Circle Developer Controlled Wallets transactions are asynchronous. Always poll `getTransaction()` until state is `COMPLETE` or `FAILED` before proceeding to the next step. A `FAILED` state is final — do not retry the same transaction ID.

**Job Expiry**
Set `expiredAt` generously during development. If a job expires before `fund()` is called, the client can claim a refund but cannot re-fund. Create a new job instead.

**Gas Fees**
Gas on Arc Testnet is paid in USDC. Ensure wallets have sufficient USDC balance to cover both the escrow amount and gas fees. The Circle Gas Station sponsors gas on testnet, but this may not apply to all transaction types.

---

## Getting Test USDC

Use the Circle Testnet Faucet to get USDC on Arc Testnet:
👉 https://faucet.circle.com

Select **Arc Testnet** and enter your wallet address.

---

## Resources

- [Arc Developer Docs](https://docs.arc.network)
- [ERC-8183 Specification](https://eips.ethereum.org/EIPS/eip-8183)
- [Circle Developer Controlled Wallets SDK](https://developers.circle.com/w3s/developer-controlled-wallets)
- [ArcScan Testnet Explorer](https://testnet.arcscan.app)
- [FreelanceArc — Example Application](https://github.com/bayrakdarerdem/freelance-arc)