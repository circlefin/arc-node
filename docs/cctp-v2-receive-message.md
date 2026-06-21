# CCTP v2 receiveMessage gas estimation

This note covers a common integration failure when relaying CCTP v2 burns from
Arc Testnet to a destination chain with `MessageTransmitterV2.receiveMessage()`.

## Symptom

A viem or MetaMask-powered relay can fail immediately with an EIP-1559 fee
error similar to:

```text
RPC submit: max fee per gas less than block base fee
```

This usually appears on the destination chain when calling
`receiveMessage(message, attestation)` after the CCTP attestation is available.

## Why it happens

`writeContract()` estimates EIP-1559 fees before the wallet signs the
transaction. Between fee estimation and mempool submission, the destination
chain base fee can move. If the signed transaction's `maxFeePerGas` is now below
the current `baseFeePerGas`, the destination node rejects it before execution.

This is easier to hit on low-fee testnets, where a very small absolute fee move
can be a meaningful relative change.

## Mitigation

Estimate fees on the destination chain immediately before calling
`receiveMessage()`, then add headroom to `maxFeePerGas`.

```ts
import { createPublicClient, http, type Chain } from "viem";

const FEE_BUMP_BASIS_POINTS = 13_000n; // 130%
const BASIS_POINTS = 10_000n;

function bumpFee(value: bigint): bigint {
  return (value * FEE_BUMP_BASIS_POINTS + BASIS_POINTS - 1n) / BASIS_POINTS;
}

async function estimateReceiveMessageFees({
  destinationChain,
  destinationRpcUrl,
}: {
  destinationChain: Chain;
  destinationRpcUrl: string;
}) {
  const destinationClient = createPublicClient({
    chain: destinationChain,
    transport: http(destinationRpcUrl),
  });

  const fees = await destinationClient.estimateFeesPerGas();
  const maxFeePerGas = fees.maxFeePerGas ?? fees.gasPrice;

  if (maxFeePerGas == null) {
    return {};
  }

  return {
    maxFeePerGas: bumpFee(maxFeePerGas),
    ...(fees.maxPriorityFeePerGas == null
      ? {}
      : { maxPriorityFeePerGas: fees.maxPriorityFeePerGas }),
  };
}
```

Then pass the padded fee fields into the destination-chain write:

```ts
const feeOverrides = await estimateReceiveMessageFees({
  destinationChain,
  destinationRpcUrl,
});

const hash = await walletClient.writeContract({
  account,
  chain: destinationChain,
  address: messageTransmitterV2Address,
  abi: messageTransmitterV2Abi,
  functionName: "receiveMessage",
  args: [message, attestation],
  ...feeOverrides,
});
```

## Integration checklist

- Estimate fees against the destination chain, not Arc Testnet.
- Estimate immediately before `receiveMessage()`; do not reuse fees captured
  before waiting for attestation.
- Add enough `maxFeePerGas` headroom for the destination chain's fee volatility.
  The example above uses 130%; more conservative relayers may choose a higher
  multiplier.
- Keep `maxPriorityFeePerGas` from the destination chain estimate unless your
  wallet or relayer has a chain-specific priority-fee policy.
- Retry by re-estimating fees rather than resubmitting the same stale signed
  transaction.

For more on Arc's own base-fee model, see
[ADR-0004: Base Fee Parameter Validation](./adr/0004-base-fee-validation.md).
