# CCTP V2 Integration Guide for Arc

  This guide documents Arc-specific behaviors when integrating [Circle's Cross-Chain Transfer Protocol V2 (CCTP V2)](https://developers.circle.com/stablecoins/cctp-getting-started). These findings come from building and testing against the live Arc testnet and are not fully covered in Circle's general CCTP documentation.

  ---

  ## Quick Reference

  | Parameter | Arc Value |
  |---|---|
  | CCTP Domain | 26 |
  | Chain ID | 5042002 |
  | TokenMessenger | `0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA` |
  | MessageTransmitter | `0xE737e5cEBEEBa77EFE34D4aa090756590b1CE275` |
  | USDC | `0x3600000000000000000000000000000000000000` |
  | `minFinalityThreshold` | `2000` (finalized) |
  | Message length | 376 bytes |
  | Attestation time | ~1–3 minutes |

  ---

  ## Critical: Always Use the V2 `depositForBurn` Selector

  Arc's TokenMessenger **only** supports the 7-parameter CCTP V2 variant of `depositForBurn`. The V1 4-parameter variant will revert.

  ### V2 (correct) — selector `0x8e0250ee`

  ```solidity
  function depositForBurn(
      uint256 amount,
      uint32  destinationDomain,
      bytes32 mintRecipient,
      address burnToken,
      bytes32 destinationCaller,   // bytes32(0) = permissionless relay
      uint256 maxFee,              // 0 for no fee cap
      uint32  minFinalityThreshold // 2000 on Arc (finalized); 1000 on others (safe)
  ) external returns (uint64 nonce);
  ```

  ### V1 (wrong — will revert on Arc) — selector `0x6fd3504e`

  ```solidity
  function depositForBurn(
      uint256 amount,
      uint32  destinationDomain,
      bytes32 mintRecipient,
      address burnToken
  ) external returns (uint64 nonce);
  ```

  ---

  ## Nonce Encoding: bytes32, Always Zero

  Arc's MessageTransmitter encodes the nonce field as **`bytes32`** (32 bytes, always `0x00…00`) in the CCTP message header — not the `uint64` used in CCTP V1 or standard CCTP V2 documentation.

  ### Message layout (376 bytes total)

  | Offset | Length | Field |
  |---|---|---|
  | 0 | 4 | `version` (uint32) |
  | 4 | 4 | `sourceDomain` (uint32) |
  | 8 | 4 | `destinationDomain` (uint32) |
  | 12 | 32 | `nonce` (bytes32, always 0x00) |
  | 44 | 32 | `sender` (bytes32) |
  | 76 | 32 | `recipient` (bytes32) |
  | 108 | 32 | `destinationCaller` (bytes32) |
  | 140 | 236 | message body |

  This was verified by scanning 221 live `MessageSent` events on Arc testnet — all had nonce = `0x0000…0000`.

  ---

  ## Replay Protection: keccak256(messageBytes)

  Because the nonce is always zero, Arc's `usedNonces` mapping uses **`keccak256(messageBytes)`** as the key — not the CCTP V1 key of `keccak256(sourceDomain, nonce)`.

  ### Correct check (CCTP V2 on Arc)

  ```typescript
  const messageHash = ethers.keccak256(messageBytes);
  const result: bigint = await messageTransmitter.usedNonces(messageHash);
  const alreadyMinted = result !== 0n;
  ```

  ### Wrong check (CCTP V1 style — silently broken on Arc)

  ```typescript
  // ❌ DO NOT USE — nonce is always 0 on Arc, this returns the same hash for every message
  const key = ethers.keccak256(
    ethers.solidityPacked(["uint32", "uint64"], [sourceDomain, nonce])
  );
  ```

  Using the V1 key means every bridge after the first would be incorrectly flagged as "already minted" once any single message with nonce=0 from that domain had been received.

  ---

  ## minFinalityThreshold

  The `minFinalityThreshold` parameter controls when Circle will issue an attestation after a burn.

  | Chain | Recommended value | Meaning |
  |---|---|---|
  | Arc Testnet | `2000` | Finalized finality (~1–3 min attestation) |
  | Ethereum Sepolia | `1000` | Safe finality (~2–5 min attestation) |
  | Base Sepolia | `1000` | Safe finality |
  | Avalanche Fuji | `1000` | Safe finality |

  Using `1000` on Arc works but may result in longer attestation waits. Using `2000` is recommended for Arc-originated burns.

  ---

  ## USDC Decimals

  Arc's native USDC at `0x3600000000000000000000000000000000000000` is a system address with a special dual-decimal representation:

  - **18 decimals** — used internally for gas accounting
  - **6 decimals** — exposed via the standard ERC-20 interface (`decimals()` returns 6)

  **Always use the ERC-20 interface with 6 decimals** for all token operations (approve, transfer, balance queries). Using 18 decimals will result in 10^12x over/underestimates.

  ```typescript
  // ✅ Correct — use 6 decimals via ERC-20 interface
  const amount = ethers.parseUnits("10.00", 6); // 10 USDC = 10_000_000n

  // ❌ Wrong — do not use 18 decimals
  const amount = ethers.parseUnits("10.00", 18);
  ```

  ---

  ## Attestation

  After a burn on Arc, poll the Circle Iris API (sandbox endpoint for testnet):

  ```
  GET https://iris-api-sandbox.circle.com/v1/attestations/{messageHash}
  ```

  Where `messageHash` is `keccak256(messageBytes)` from the `MessageSent` event.

  Expected response when ready:

  ```json
  {
    "attestation": "0x...",
    "status": "complete"
  }
  ```

  **Timing:**
  - Arc → other chains: ~1–3 minutes
  - Other chains → Arc: ~2–5 minutes

  Poll every 5 seconds. If the attestation is not ready within 20 minutes, the burn may not have been indexed — verify the transaction was confirmed on-chain first.

  ---

  ## Gas on Arc

  Arc uses USDC as its native gas token. When estimating gas for `depositForBurn` or `receiveMessage` calls on Arc, use a higher gas limit than you might expect:

  ```typescript
  // Recommended gas limit for Arc write transactions
  const gasLimit = 600_000n;
  ```

  Standard EVM gas estimation (`eth_estimateGas`) may underestimate on Arc due to the USDC-as-gas mechanics. Setting an explicit override prevents out-of-gas reverts.

  ---

  ## Complete Example (ethers.js v6)

  ```typescript
  import { ethers } from "ethers";

  const TOKEN_MESSENGER = "0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA";
  const ARC_DOMAIN = 26;
  const ARC_USDC   = "0x3600000000000000000000000000000000000000";

  async function burnOnArc(
    signer: ethers.Signer,
    amount: bigint,          // in 6-decimal units
    destinationDomain: number,
    mintRecipient: string    // EVM address of recipient
  ) {
    const messenger = new ethers.Contract(TOKEN_MESSENGER, [
      "function depositForBurn(uint256,uint32,bytes32,address,bytes32,uint256,uint32) returns (uint64)"
    ], signer);

    const recipientBytes32 = ethers.zeroPadValue(mintRecipient, 32);

    const tx = await messenger.depositForBurn(
      amount,
      destinationDomain,
      recipientBytes32,
      ARC_USDC,
      ethers.ZeroHash,  // destinationCaller = bytes32(0) = permissionless
      0n,               // maxFee = 0
      2000,             // minFinalityThreshold (Arc-originated burns)
      { gasLimit: 600_000n }
    );

    const receipt = await tx.wait();
    return receipt;
  }
  ```

  ---

  ## Resources

  - [Circle CCTP Documentation](https://developers.circle.com/stablecoins/cctp-getting-started)
  - [Arc Developer Docs](https://docs.arc.network)
  - [Arc Testnet Explorer](https://testnet.arcscan.app)
  - [Circle Iris API (sandbox)](https://iris-api-sandbox.circle.com)
  - [Circle Faucet](https://faucet.circle.com)
  