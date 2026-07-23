# Transaction Memos

How the predeployed `Memo` contract attaches metadata to a contract call on Arc
while preserving the original EOA as `msg.sender`.

Official product docs: [Transaction memos](https://docs.arc.io/arc/concepts/transaction-memos).

## Contract

| Network | Address |
| --- | --- |
| Local genesis / testnet predeploy | `0x5294E9927c3306DcBaDb03fe70b92e01cCede505` |

Source: [`contracts/src/memo/Memo.sol`](../contracts/src/memo/Memo.sol)

## Canonical ABI

The only entry point is:

```solidity
function memo(
    address target,
    bytes calldata data,
    bytes32 memoId,
    bytes calldata memoData
) external;
```

| Parameter | Type | Description |
| --- | --- | --- |
| `target` | `address` | Contract to call (e.g. USDC) |
| `data` | `bytes` | Calldata forwarded to `target` |
| `memoId` | `bytes32` | Application-defined identifier |
| `memoData` | `bytes` | Arbitrary memo payload (**not** `string`) |

There is **no** `callWithMemo` function on this contract.

## Gas estimation and `eth_call` (issue #189)

`memo` is state-changing (`memoIndex++`) but ordinary top-level simulation is
non-static. With the **correct** ABI and an EOA as `from`:

- `eth_estimateGas` succeeds
- `eth_call` succeeds (state changes are discarded after simulation)
- A real transaction using the estimated gas limit succeeds on-chain

Regression coverage lives in `tests/localdev/subcall.test.ts`
(`eth_estimateGas succeeds for memo...`, `eth_call succeeds for memo...`,
`estimated gas is enough to execute memo transfer on-chain`).

### Common pitfall: wrong function name or types

Callers that use a non-existent signature such as:

```text
callWithMemo(address,bytes,bytes32,string)
```

hit a selector mismatch. Solidity reverts with **empty** return data. Libraries
then report `execution reverted` during `eth_estimateGas` / `eth_call`, which is
easy to misread as a CallFrom or node simulation bug.

**Fix:** use `memo(address,bytes,bytes32,bytes)` and encode the memo as `bytes`
(e.g. `toHex('invoice-123')` / `toBytes(...)` in viem), not as a Solidity
`string`.

### Other guardrails (expected reverts)

These are intentional, not estimation bugs:

| Pattern | Result |
| --- | --- |
| Contract wallet / intermediary calls `memo` | Revert (EOA-only / no sender spoofing) |
| `STATICCALL` into `Memo` | Revert (state change in static context) |
| EOA calls `CallFrom` precompile directly | `unauthorized caller` |
| Inner target reverts | Outer tx reverts with `MemoFailed(bytes)` |

## Example: estimate then send (cast)

```bash
MEMO=0x5294E9927c3306DcBaDb03fe70b92e01cCede505
USDC=0x3600000000000000000000000000000000000000
FROM=<your-eoa>

# Estimate (must pass --from)
cast estimate --from $FROM $MEMO 'memo(address,bytes,bytes32,bytes)' \
  $USDC \
  "$(cast calldata 'transfer(address,uint256)' $FROM 2)" \
  "$(cast to-uint256 1)" \
  "0x$(printf 'test memo' | xxd -p -c 256)" \
  --rpc-url http://localhost:8545

# eth_call simulation
cast call --from $FROM $MEMO 'memo(address,bytes,bytes32,bytes)' \
  $USDC \
  "$(cast calldata 'transfer(address,uint256)' $FROM 2)" \
  "$(cast to-uint256 1)" \
  "0x$(printf 'test memo' | xxd -p -c 256)" \
  --rpc-url http://localhost:8545
```

## Example: viem

```typescript
import { encodeFunctionData, erc20Abi, toBytes, toHex } from 'viem'

const innerCalldata = encodeFunctionData({
  abi: erc20Abi,
  functionName: 'transfer',
  args: [recipient, amount],
})

await walletClient.writeContract({
  address: MEMO_ADDRESS,
  abi: [
    {
      name: 'memo',
      type: 'function',
      stateMutability: 'nonpayable',
      inputs: [
        { name: 'target', type: 'address' },
        { name: 'data', type: 'bytes' },
        { name: 'memoId', type: 'bytes32' },
        { name: 'memoData', type: 'bytes' },
      ],
      outputs: [],
    },
  ],
  functionName: 'memo',
  args: [
    USDC_ADDRESS,
    innerCalldata,
    toHex(1n, { size: 32 }),
    toBytes('Invoice INV-2026-001'),
  ],
  // Explicit gas is optional when the ABI is correct; estimateGas works.
})
```

## Related

- Local tests: `tests/localdev/subcall.test.ts`
- CallFrom precompile: `crates/precompiles/src/call_from.rs`
- Multicall3From (also uses CallFrom): `contracts/src/batch/Multicall3From.sol`
