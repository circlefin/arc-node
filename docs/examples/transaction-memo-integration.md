# Arc Transaction Memos: Multi-Agent Orchestrator Integration Example

This example shows how to integrate Arc's Transaction Memos feature with an existing contract without modifying it, using the predeployed Memo contract on Arc Testnet.

## Overview

Transaction Memos (announced June 18, 2026) let you attach structured context to any contract call via a predeployed Memo contract, which routes the inner call through the CallFrom precompile so the original msg.sender is preserved. The Memo event is only emitted if the inner call succeeds, giving downstream systems a clean reconciliation signal.

## Predeployed Contracts on Arc Testnet

| Contract | Address |
|----------|---------|
| Memo | 0x5294E9927c3306DcBaDb03fe70b92e01cCede505 |
| Multicall3From | 0x522fAf9A91c41c443c66765030741e4AaCe147D0 |

## Function Signature

function callWithMemo(address target, bytes calldata data, bytes32 correlationId, string calldata memo) external returns (bool success, bytes memory result)

Selector: 0xc3b2c4f8

This signature was derived by inspecting the Memo contract bytecode dispatcher and decoding a real on-chain transaction calldata to confirm parameter types, since the ABI was not yet published when this example was built.

## Common Pitfalls

- Empty revert data: behavior with cast call --from may differ from an actual sent transaction.
- Inner call must succeed: the Memo event only emits when the wrapped call succeeds.
- bytes32 correlationId must be exactly 32 bytes.

## Live Verified Example

Tested against the MultiAgentOrchestrator contract from arc-multi-agent:

- Memo contract: 0x5294E9927c3306DcBaDb03fe70b92e01cCede505
- Target: 0xe81f5BA4181eA29061C3C229c8D6EB4cFE56639C
- Tx: https://testnet.arcscan.app/tx/0x7532ce470169e2db2be43e5f9c43dd523d21e9071c04268ff5941f8ca839ef7b
- Status: Success, confirmed in 0.58 seconds

## Resources

- Arc Transaction Memos announcement: https://community.arc.io/home/blogs/arc-transaction-memos-structured-transaction-context-for-financial-workflows-on-arc-2026-06-18
- Arc Contract Addresses: https://docs.arc.io/arc/references/contract-addresses
- Full implementation: https://github.com/consumeobeydie/arc-agent-api

## Known Limitations

### EOA-only Constraint

The Memo contract is **EOA-only**. This is enforced at the precompile level, not in the Memo contract itself.

When `memo()` is called, it internally invokes the `callFrom` precompile at `0x1800000000000000000000000000000000000003`. The precompile requires that `msg.sender` equals `tx.origin` — in other words, the caller must be an Externally Owned Account (EOA), not a smart contract.

**What fails:**

If you call `memo()` from a smart contract account — such as a Circle Developer Controlled Wallet, a Modular Wallet (ERC-4337 SCA), or any other contract-based account — the transaction will revert silently. The `MemoFailed` error is **not** raised in this case; the entire call reverts at the precompile level before the Memo contract can catch it.

This was confirmed on Arc Testnet (Chain ID: 5042002) using the Circle Developer Controlled Wallets SDK. Gas estimation also fails for this reason when using `callFrom` simulation with contract-based senders.

**What works:**

Call `memo()` directly from an EOA wallet using a private key signer:

```typescript
// ✅ Works — EOA signer via viem
import { createWalletClient, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";

const account = privateKeyToAccount("0x...");
const walletClient = createWalletClient({ account, transport: http(RPC_URL) });

await walletClient.writeContract({
  address: MEMO_CONTRACT,
  abi: MEMO_ABI,
  functionName: "memo",
  args: [target, calldata, memoId, memoData],
});
```

```typescript
// ❌ Fails — Circle Developer Controlled Wallet (Smart Contract Account)
// The callFrom precompile rejects contract callers silently.
// Gas estimation will also fail with this setup.
```

**Reference:**

From [`IMemo.sol`](../../../contracts/src/memo/IMemo.sol):

> EOA-only: `memo()` invokes the `callFrom` precompile, which requires the sender argument (`msg.sender` of Memo) to equal the precompile caller or `tx.origin`. A contract caller is neither, so `callFrom` reverts and the entire call reverts without raising `MemoFailed`.

From [`ICallFrom.sol`](../../../contracts/src/call-from/ICallFrom.sol):

> Executes a call to `target` with `data` as if `sender` were the caller.
