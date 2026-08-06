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
