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
`receiveMessage()`, then add headroom to the fee fields that chain actually
accepts. EIP-1559 chains should receive `maxFeePerGas` and, when available,
`maxPriorityFeePerGas`. Legacy-gas chains should receive `gasPrice` instead.

```ts
import { createPublicClient, http, type Chain } from "viem";

const MAX_FEE_BUMP_BASIS_POINTS = 13_000n; // 130%
const PRIORITY_FEE_BUMP_BASIS_POINTS = 11_500n; // 115%
const BASIS_POINTS = 10_000n;

function bumpFee(value: bigint, basisPoints: bigint): bigint {
  return (value * basisPoints + BASIS_POINTS - 1n) / BASIS_POINTS;
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

  let fees: Awaited<ReturnType<typeof destinationClient.estimateFeesPerGas>>;
  try {
    fees = await destinationClient.estimateFeesPerGas();
  } catch (err) {
    throw new Error(`fee estimation failed on ${destinationChain.name}: ${err}`);
  }

  if (fees.maxFeePerGas != null) {
    return {
      maxFeePerGas: bumpFee(fees.maxFeePerGas, MAX_FEE_BUMP_BASIS_POINTS),
      ...(fees.maxPriorityFeePerGas == null
        ? {}
        : {
            maxPriorityFeePerGas: bumpFee(
              fees.maxPriorityFeePerGas,
              PRIORITY_FEE_BUMP_BASIS_POINTS,
            ),
          }),
    };
  }

  if (fees.gasPrice != null) {
    return {
      gasPrice: bumpFee(fees.gasPrice, MAX_FEE_BUMP_BASIS_POINTS),
    };
  }

  throw new Error(
    `estimateFeesPerGas returned no usable fee fields on ${destinationChain.name}; cannot construct safe fee overrides`,
  );
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
- Add modest `maxPriorityFeePerGas` headroom too when the destination chain uses
  EIP-1559. The example above uses 115%.
- On legacy-gas destination chains, pass a bumped `gasPrice` and do not include
  EIP-1559 fee fields.
- Retry by re-estimating fees rather than resubmitting the same stale signed
  transaction.

## Arc Testnet as destination

For reverse CCTP flows where Arc Testnet is the destination, use the legacy
`gasPrice` branch above. Arc Testnet transactions should not include
`maxFeePerGas` or `maxPriorityFeePerGas` overrides. Arc Testnet gas estimates
can be stale even for a single transaction, so re-estimate `gasPrice`
immediately before submission and apply the same 130% headroom. See
[#87](https://github.com/circlefin/arc-node/issues/87) for the Arc Testnet
`gasPrice` workaround.

For more on Arc's own base-fee model, see
[ADR-0004: Base Fee Parameter Validation](./adr/0004-base-fee-validation.md).
