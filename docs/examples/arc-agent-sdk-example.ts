/**
 * Arc Agent SDK Example
 * TypeScript SDK for Arc Testnet Agent Economy contracts
 *
 * npm: @consumeobeydie/arc-agent-sdk
 * github: https://github.com/consumeobeydie/arc-agent-sdk
 *
 * Contracts (Arc Testnet):
 *   AgentIdentity:  0x5275783cD74eC21739Af8f3be9c42C024F671cFb
 *   SpendingLimits: 0x615a4B25448980a6b518f9F9088C206387535192
 *   SplitPayment:   0x775D4DF117f0B63a16ade4185bDa221Adcb4AEA3
 *   EventLogger:    0x9C50765e591663ED541B2fB863626f39fC6C12e0
 *   ArcDEX:         0x1A142DF560a671c66c361A29a48Ab839Bc9F890E
 */

import { ArcAgentSDK, ARC_TESTNET, CONTRACTS } from "@consumeobeydie/arc-agent-sdk";

async function main() {
  // Initialize SDK with private key
  const sdk = new ArcAgentSDK(process.env.PRIVATE_KEY!);

  console.log("Arc Agent SDK Example");
  console.log("====================");
  console.log(`Chain: ${ARC_TESTNET.name} (${ARC_TESTNET.id})`);
  console.log(`Address: ${sdk.address}`);

  // ── 1. Register Agent Identity (ERC-8004) ────────────────────────
  console.log("\n1. Registering agent identity...");
  const { hash: registerHash } = await sdk.registerAgent(
    "Arc Agent A",
    "https://arc-intelligence-dashboard.vercel.app/agents/arc-agent-a",
    "payments,vault,missions,usdc-transfer"
  );
  console.log(`   TX: ${registerHash}`);

  // ── 2. Setup Spending Limits ──────────────────────────────────────
  console.log("\n2. Setting spending limits...");
  const { hash: limitHash } = await sdk.setSpendingLimit(
    sdk.address,
    1_000_000_000_000_000_000n, // 1 USDC daily
    5_000_000_000_000_000_000n, // 5 USDC weekly
      100_000_000_000_000_000n  // 0.1 USDC per tx
  );
  console.log(`   TX: ${limitHash}`);

  // ── 3. Check Budget ───────────────────────────────────────────────
  console.log("\n3. Checking budget...");
  const { ok, reason } = await sdk.canSpend(sdk.address, 10_000_000_000_000_000n);
  console.log(`   canSpend: ${ok} | reason: ${reason}`);

  const remaining = await sdk.remainingDaily(sdk.address);
  console.log(`   remainingDaily: ${Number(remaining) / 1e18} USDC`);

  // ── 4. Create Revenue Split ───────────────────────────────────────
  console.log("\n4. Creating revenue split (70% agent, 30% treasury)...");
  const TREASURY = "0x54b4B44749a95070560509B6Ec0be501665CcF63";
  const { hash: splitHash } = await sdk.createSplit(
    "Agent Mission Revenue",
    [sdk.address, TREASURY],
    [7000n, 3000n],
    ["agent-a", "treasury"]
  );
  console.log(`   TX: ${splitHash}`);

  // ── 5. Record Mission + Memo ──────────────────────────────────────
  console.log("\n5. Recording mission with on-chain memo...");
  const { hash: memoHash } = await sdk.logWithMemo(
    "Mission complete: API purchase",
    "invoice-2026-001",
    "agent=arc-agent-a,amount=0.01,status=success"
  );
  console.log(`   TX: ${memoHash}`);

  // ── 6. Record Mission in AgentIdentity ───────────────────────────
  console.log("\n6. Recording mission in AgentIdentity...");
  const agentId = await sdk.getAgentId(sdk.address);
  const { hash: missionHash } = await sdk.recordMission(agentId, true);
  console.log(`   TX: ${missionHash}`);

  // ── 7. Check Success Rate ─────────────────────────────────────────
  console.log("\n7. Checking agent success rate...");
  const successRate = await sdk.getSuccessRate(agentId);
  console.log(`   Success rate: ${successRate}%`);

  // ── 8. Record Spend ───────────────────────────────────────────────
  console.log("\n8. Recording spend...");
  const { hash: spendHash } = await sdk.recordSpend(
    sdk.address,
    10_000_000_000_000_000n // 0.01 USDC
  );
  console.log(`   TX: ${spendHash}`);
  const remainingAfter = await sdk.remainingDaily(sdk.address);
  console.log(`   Remaining after spend: ${Number(remainingAfter) / 1e18} USDC`);

  console.log("\n✅ Arc Agent SDK Example Complete!");
  console.log("\nContracts used:");
  Object.entries(CONTRACTS).forEach(([name, addr]) => {
    console.log(`  ${name}: ${addr}`);
  });
}

main().catch(console.error);
