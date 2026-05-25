# CCTP V2 Integration Guide for Arc

  This guide documents Arc-specific behaviors when integrating [Circle's Cross-Chain Transfer Protocol V2 (CCTP V2)](https://developers.circle.com/stablecoins/cctp-getting-started). These findings come from building and testing a production bridge dapp against the live Arc testnet and are not fully covered in Circle's general CCTP documentation.

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
  | Recommended gas limit | 600,000 (see §Gas Estimation Bug) |

  ---

  ## Critical: Always Use the V2 `depositForBurn` Selector

  Arc's TokenMessenger **only** supports the 7-parameter CCTP V2 variant of `depositForBurn`. The V1 4-parameter variant will revert silently.

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

  ## ⚠️ Known Bug: eth_estimateGas Unreliable on Arc Testnet

  **`eth_estimateGas` consistently fails or returns incorrect values for all USDC write transactions on Arc Testnet.** This affects ERC-20 `approve`, ERC-20 `transfer`, `depositForBurn`, and `receiveMessage`.

  ### Observed errors (without explicit gas limit)

  ```
  Error: missing revert data
  Error: could not estimate gas; transaction may fail or may require manual gas limit
  Error: execution reverted (no reason string)
  ```

  ### Root cause hypothesis

  Arc's USDC-as-gas model may cause the EVM's gas simulation to incorrectly predict reverts when it cannot account for the ERC-20 gas token balance check. The transactions themselves succeed on-chain when submitted with a fixed gas limit.

  ### Workaround — always provide an explicit gasLimit

  ```typescript
  const tx = await contract.someMethod(args, { gasLimit: 600_000n });
  ```

  600,000 is comfortably above actual gas usage (typically 50,000–150,000 for CCTP ops) and is safe to hard-code until gas estimation is fixed. This issue has been reported in [arc-node#80](https://github.com/circlefin/arc-node/issues/80).

  ---

  ## ⚠️ Known Issue: RPC Endpoint Reliability

  Two public testnet RPC endpoints are available, with significantly different reliability for transaction submission:

  | Endpoint | Read calls | `eth_sendRawTransaction` |
  |---|---|---|
  | `https://rpc.drpc.testnet.arc.network` | ✅ Reliable | ✅ Reliable |
  | `https://rpc.testnet.arc.network` | ✅ Reliable | ⚠️ Intermittently fails with `"error sending request"` |

  **Use `rpc.drpc.testnet.arc.network` as your primary endpoint**, with `rpc.testnet.arc.network` as a fallback. The unreliable forwarding behaviour of the second endpoint is related to [arc-node#59](https://github.com/circlefin/arc-node/issues/59).

  ```typescript
  // Recommended RPC config for Arc Testnet
  const RPC_PRIMARY  = "https://rpc.drpc.testnet.arc.network";
  const RPC_FALLBACK = "https://rpc.testnet.arc.network";
  ```

  ---

  ## ⚠️ Known Issue: ethers.js BrowserProvider + Chain Switch

  When building a cross-chain dapp that switches the user's wallet between Arc and another chain (e.g. Arc → Sepolia for the mint step), **ethers v6 `BrowserProvider` can throw `"underlying network changed"` during `tx.wait()`** — even when the transaction was already confirmed on-chain.

  ### Why this happens

  After MetaMask switches chains, ethers v6 detects the network change and aborts any in-flight receipt polling with a "network changed" error. This is a false negative — the transaction succeeded, but the dapp sees an error.

  ### Workaround — retry with a static provider

  After catching a network-flavoured error from `tx.wait()`, retry by fetching the receipt via a `JsonRpcProvider` (static, independent of MetaMask):

  ```typescript
  async function waitWithRetry(
    tx: ethers.TransactionResponse,
    rpcUrls: string[],
    retries = 4
  ): Promise<ethers.TransactionReceipt> {
    const isNetworkErr = (e: unknown) =>
      /(network|could not detect|connection|timeout)/i.test(
        e instanceof Error ? e.message : String(e)
      );

    for (let i = 0; i <= retries; i++) {
      try {
        const receipt = await tx.wait();
        if (receipt) return receipt;
      } catch (err) {
        if (!isNetworkErr(err) || i === retries) throw err;
      }
      await new Promise(r => setTimeout(r, 4000));
      for (const url of rpcUrls) {
        try {
          const receipt = await new ethers.JsonRpcProvider(url)
            .getTransactionReceipt(tx.hash);
          if (receipt) return receipt;
        } catch { /* try next */ }
      }
    }
    throw new Error("Transaction not confirmed after retries");
  }
  ```

  ---

  ## minFinalityThreshold

  | Chain | Recommended value | Meaning |
  |---|---|---|
  | Arc Testnet | `2000` | Finalized finality (~1–3 min attestation) |
  | Ethereum Sepolia | `1000` | Safe finality (~2–5 min attestation) |
  | Base Sepolia | `1000` | Safe finality |
  | Avalanche Fuji | `1000` | Safe finality |

  Using `1000` on Arc works but may result in longer attestation waits. `2000` (finalized) is recommended for Arc-originated burns.

  ---

  ## USDC Decimals

  Arc's native USDC at `0x3600000000000000000000000000000000000000` has a dual-decimal representation:

  - **18 decimals** — used internally for gas accounting
  - **6 decimals** — exposed via the standard ERC-20 interface (`decimals()` returns 6)

  **Always use the ERC-20 interface with 6 decimals** for all token operations.

  ```typescript
  // ✅ Correct
  const amount = ethers.parseUnits("10.00", 6); // 10 USDC = 10_000_000n

  // ❌ Wrong
  const amount = ethers.parseUnits("10.00", 18);
  ```

  ---

  ## Block Explorer

  The canonical Arc Testnet block explorer is **[testnet.arcscan.app](https://testnet.arcscan.app)**.

  Transaction URL format: `https://testnet.arcscan.app/tx/{txHash}`

  > ⚠️ `explorer.testnet.arc.network` and `explorer.arc.io` are both dead. See [arc-node#81](https://github.com/circlefin/arc-node/issues/81).

  When adding Arc Testnet to MetaMask via `wallet_addEthereumChain`, use:
  ```typescript
  blockExplorerUrls: ["https://testnet.arcscan.app"]
  ```

  ---

  ## Attestation

  After a burn on Arc, poll the Circle Iris API (sandbox endpoint for testnet):

  ```
  GET https://iris-api-sandbox.circle.com/v1/attestations/{messageHash}
  ```

  Where `messageHash = keccak256(messageBytes)` from the `MessageSent` event.

  **Timing:**
  - Arc → other chains: ~1–3 minutes (finalized finality)
  - Other chains → Arc: ~2–5 minutes (safe finality)

  Poll every 5 seconds for up to 20 minutes. If the attestation is not ready, verify the burn transaction was confirmed on-chain first.

  ---

  ## Complete Example (ethers.js v6)

  ```typescript
  import { ethers } from "ethers";

  const TOKEN_MESSENGER = "0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA";
  const ARC_USDC        = "0x3600000000000000000000000000000000000000";

  async function burnOnArc(
    signer: ethers.Signer,
    amount: bigint,           // 6-decimal units
    destinationDomain: number,
    mintRecipient: string     // EVM address
  ) {
    const messenger = new ethers.Contract(TOKEN_MESSENGER, [
      "function depositForBurn(uint256,uint32,bytes32,address,bytes32,uint256,uint32) returns (uint64)"
    ], signer);

    // Note: always provide explicit gasLimit — eth_estimateGas is unreliable on Arc
    const tx = await messenger.depositForBurn(
      amount,
      destinationDomain,
      ethers.zeroPadValue(mintRecipient, 32),
      ARC_USDC,
      ethers.ZeroHash, // destinationCaller = anyone may relay
      0n,              // maxFee
      2000,            // minFinalityThreshold (Arc: finalized)
      { gasLimit: 600_000n }
    );

    return tx.wait();
  }
  ```

  ---

  ## Resources

  - [Circle CCTP Documentation](https://developers.circle.com/stablecoins/cctp-getting-started)
  - [Arc Developer Docs](https://docs.arc.network)
  - [Arc Testnet Explorer](https://testnet.arcscan.app)
  - [Circle Iris API (sandbox)](https://iris-api-sandbox.circle.com)
  - [Circle Faucet](https://faucet.circle.com)
  - [arc-node#80](https://github.com/circlefin/arc-node/issues/80) — eth_estimateGas bug report
  - [arc-node#81](https://github.com/circlefin/arc-node/issues/81) — Dead explorer URLs report
  