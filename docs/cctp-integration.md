# CCTP Integration on Arc

This document covers Cross-Chain Transfer Protocol (CCTP) v2 integration specifics
for Arc Mainnet. For general CCTP concepts and the full API reference, see the
[Circle CCTP documentation](https://developers.circle.com/stablecoins/cctp-getting-started).

## Arc Mainnet CCTP Parameters

| Parameter | Value |
|---|---|
| CCTP Domain | **26** |
| Chain ID | **5042** |
| TOKEN_MESSENGER_V2 | `0x28b5a0e9C621a5BadaA536219b3a228C8168cf5d` |
| MESSAGE_TRANSMITTER_V2 | `0xE737e5cEBEEBa77EFe34D4aa090BcB7619cBAa5` |
| USDC | `0x3600000000000000000000000000000000000000` |

## Attestation Timing for Domain 26

> [!IMPORTANT]
> CCTP v2 attestations for Arc Mainnet with `finalityThreshold: "finalized"` take
> **significantly longer** than other CCTP chains. Plan your retry logic and UX
> accordingly.

### Why attestations take longer

Arc uses BFT consensus with deterministic sub-second finality at the block level,
but the Circle Attestation Service indexes Arc events with a conservative finality
window to guarantee safety across reorgs and network partitions. For `"finalized"`
attestations on domain 26, this indexing delay is typically **2–6 hours**.

### Attestation lifecycle

After burning USDC on Arc with `finalityThreshold: "finalized"`, the attestation
API progresses through these states:

```
pending_confirmations   ← waiting for the Attestation Service to index the burn
↓
complete                ← attestation ready; message and attestation fields populated
```

A `pending_confirmations` response with `"message": null` is **not an error** — it
means the indexer has not yet ingested the event. Do not reattest; calling
`reattest` returns `"already finalized"` for burns that are real but not yet indexed.

### Recommended retry pattern

```typescript
const POLL_INTERVAL_MS = 30_000;   // 30 seconds
const MAX_WAIT_MS     = 8 * 60 * 60 * 1000; // 8 hours

async function waitForAttestation(txHash: string): Promise<Attestation> {
  const deadline = Date.now() + MAX_WAIT_MS;

  while (Date.now() < deadline) {
    const res = await fetch(
      `https://iris-api.circle.com/v2/messages/26?transactionHash=${txHash}`
    ).then(r => r.json());

    if (res.messages?.[0]?.status === "complete") {
      return res.messages[0];
    }

    // status === "pending_confirmations" is expected for hours; not an error.
    console.log(`Attestation pending (${res.messages?.[0]?.status}). Retrying in 30s…`);
    await new Promise(r => setTimeout(r, POLL_INTERVAL_MS));
  }

  throw new Error("Attestation timed out after 8 hours");
}
```

> [!TIP]
> For production applications, persist the transaction hash and poll from a
> background job rather than blocking a user-facing request. Return the user
> a "processing" state and notify them when the cross-chain transfer completes.

### Fast-finality burns

If your application does not require `"finalized"` finality, use
`finalityThreshold: "fast"` when calling `depositForBurnWithHook`. Fast-finality
attestations on Arc complete in seconds rather than hours, matching the UX of
other CCTP chains. Use `"finalized"` only when regulatory or reconciliation
requirements demand it.

## EVM Compatibility

Arc Mainnet runs the **Osaka** hardfork and is fully compatible with all opcodes
introduced up to and including Osaka. Notably:

- **`PUSH0` (EIP-3855) is supported.** Contracts compiled with Solidity \u226520.8.20
  and the default `evm_version` deploy and execute correctly. Setting
  `evm_version = "paris"` in `foundry.toml` is *not* required.
- **`MCOPY` (EIP-5656)** and **`BLOBHASH`/`BLOBBASEFEE` (EIP-4844)** opcodes are
  available (though blob transactions are not supported).
- **EIP-7825 gas limit cap**: transactions and RPC calls cannot request more than
  **2\u00b224 = 16,777,216 gas**. `eth_estimateGas` and `eth_call` requests that exceed
  this cap return a `-32003` out-of-gas error. Size contracts and batch operations
  to stay within this ceiling.

## Contract Verification

Arc Mainnet (chain 5042) is supported by [Sourcify](https://sourcify.dev). Use
Sourcify for automated contract verification:

```bash
# Verify via Sourcify (bypasses the Cloudflare WAF on the block explorer API)
forge verify-contract \
  --chain 5042 \
  --verifier sourcify \
  --skip-is-verified-check \
  <CONTRACT_ADDRESS> \
  src/MyContract.sol:MyContract
```

> [!NOTE]
> `forge verify-contract --verifier blockscout` will fail on Arc Mainnet because
> the block explorer API (`explorer.arc.io/api`) is protected by Cloudflare, which
> blocks server-side requests. Use `--verifier sourcify` instead; the Arc explorer
> picks up Sourcify-verified contracts automatically.
