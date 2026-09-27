# DApp Development on Arc Testnet

  A practical reference for developers building browser-based DApps on Arc Testnet. These notes reflect
  real-world integration experience — each section documents a pain point encountered during development,
  with a working workaround.

  ---

  ## Network Configuration

  | Parameter | Value |
  |---|---|
  | Chain ID | `5042002` (`0x4cef52`) |
  | Public RPC | `https://rpc.testnet.arc.network` |
  | Browser-safe RPC | `https://arc-testnet.drpc.org` (has CORS headers) |
  | Block explorer | `https://testnet.arcscan.app` |
  | USDC | `0x3600000000000000000000000000000000000000` |
  | Chain ID (mainnet) | `5042` |

  ---

  ## MetaMask Integration

  ### ⚠️ Always use `wallet_addEthereumChain`, never `wallet_switchEthereumChain`

  `wallet_switchEthereumChain` fails silently or throws error `4902` for Arc Testnet — even when
  the network was previously added. `wallet_addEthereumChain` is the only reliable method: it both
  adds the network if missing *and* switches to it if already present. See [arc-node#89](https://github.com/circlefin/arc-node/issues/89).

  ```typescript
  await window.ethereum.request({
    method: "wallet_addEthereumChain",
    params: [{
      chainId: "0x4CEF52",              // 5042002
      chainName: "Arc Testnet",
      nativeCurrency: { name: "ETH", symbol: "ETH", decimals: 18 },
      rpcUrls: ["https://rpc.testnet.arc.network"],
      blockExplorerUrls: ["https://testnet.arcscan.app"],
    }],
  });
  ```

  ### Gas display note

  Arc uses USDC as its native gas token, but MetaMask requires `nativeCurrency.decimals: 18` and
  treats it as ETH. MetaMask will show gas costs in "ETH" (18-decimal units) — this is cosmetic and
  cannot be changed from application code.

  ---

  ## RPC Endpoints

  ### CORS limitation

  `https://rpc.testnet.arc.network` does **not** return `Access-Control-Allow-Origin` headers.
  Browser fetch() calls fail with a CORS error. Tracked in [arc-node#90](https://github.com/circlefin/arc-node/issues/90).

  **Options:**

  | Approach | How |
  |---|---|
  | Use dRPC endpoint | `https://arc-testnet.drpc.org` — has CORS headers, works in browser |
  | Use MetaMask provider | `window.ethereum` proxies RPC, bypassing CORS |
  | Proxy backend | Forward `/rpc` → `rpc.testnet.arc.network` from your server |

  ```typescript
  import { ethers } from "ethers";

  // ✅ Works in browser — MetaMask handles CORS
  const provider = new ethers.BrowserProvider(window.ethereum);

  // ✅ Works in browser — dRPC has CORS headers
  const staticProvider = new ethers.JsonRpcProvider("https://arc-testnet.drpc.org");

  // ❌ CORS error in browser
  // const broken = new ethers.JsonRpcProvider("https://rpc.testnet.arc.network");
  ```

  ### Reliability comparison

  | Endpoint | Browser CORS | Reads | `eth_sendRawTransaction` |
  |---|---|---|---|
  | `https://arc-testnet.drpc.org` | ✅ | ✅ | ✅ Reliable |
  | `https://rpc.testnet.arc.network` | ❌ | ✅ | ⚠️ Intermittent |

  Tracked in [arc-node#92](https://github.com/circlefin/arc-node/issues/92) and [arc-node#59](https://github.com/circlefin/arc-node/issues/59).

  ---

  ## USDC Decimals

  Arc USDC has **two decimal representations** depending on context:

  | Context | Decimals | 1 USDC encoded as |
  |---|---|---|
  | ERC-20 (transfer, approve, balanceOf) | **6** | `1_000_000` |
  | Native gas (eth_gasPrice, fee history) | **18** | `1_000_000_000_000_000_000` |

  **Always use 6 decimals for token operations:**

  ```typescript
  // ✅ Correct
  const amount = ethers.parseUnits("10.00", 6);  // 10_000_000n

  // ❌ Wrong — overdraws by 10^12
  const amount = ethers.parseUnits("10.00", 18);
  ```

  Calling `usdc.decimals()` returns `6`, which is correct for all ERC-20 operations.
  The 18-decimal representation only appears in RPC gas-price responses.
  Tracked in [arc-node#91](https://github.com/circlefin/arc-node/issues/91).

  ---

  ## Gas — Two Known Issues

  ### Issue 1: `eth_estimateGas` unreliable

  Gas estimation fails or returns incorrect values for USDC write transactions (approve, transfer,
  depositForBurn). Always provide an explicit `gasLimit`. Tracked in [arc-node#80](https://github.com/circlefin/arc-node/issues/80).

  ```typescript
  // 600_000 is safe and well above actual usage (~50k–150k for CCTP operations)
  const tx = await contract.method(args, { gasLimit: 600_000n });
  ```

  ### Issue 2: Stale `eth_gasPrice` baseline

  Submitting two transactions in rapid succession may fail with **"replacement transaction underpriced"**
  unless a ≥30% gas price premium is applied. Tracked in [arc-node#87](https://github.com/circlefin/arc-node/issues/87).

  ```typescript
  const feeData  = await provider.getFeeData();
  const gasPrice = (feeData.gasPrice! * 130n) / 100n;  // +30% premium

  const tx = await contract.method(args, { gasPrice, gasLimit: 600_000n });
  ```

  ---

  ## `eth_getLogs` Block Range Limit

  Wide-range `eth_getLogs` calls (e.g., fromBlock: 0 → toBlock: latest) hang silently for 60+
  seconds and never return. Paginate with a bounded range. Tracked in [arc-node#83](https://github.com/circlefin/arc-node/issues/83).

  ```typescript
  async function getLogsInBatches(provider, filter, batchSize = 10_000) {
    const latest = await provider.getBlockNumber();
    const logs: ethers.Log[] = [];
    for (let from = 0; from <= latest; from += batchSize) {
      const chunk = await provider.getLogs({
        ...filter,
        fromBlock: from,
        toBlock: Math.min(from + batchSize - 1, latest),
      });
      logs.push(...chunk);
    }
    return logs;
  }
  ```

  ---

  ## EIP-2612 Permit (Gasless Approvals)

  Arc Testnet USDC supports EIP-2612 `permit` — off-chain approval via a typed-data signature.
  This removes the need for a separate `approve()` transaction. Tracked in [arc-node#93](https://github.com/circlefin/arc-node/issues/93).

  ```typescript
  import { ethers } from "ethers";

  const USDC  = "0x3600000000000000000000000000000000000000";
  const ABI   = [
    "function nonces(address) view returns (uint256)",
    "function name() view returns (string)",
    "function version() view returns (string)",
    "function permit(address owner, address spender, uint256 value, uint256 deadline, uint8 v, bytes32 r, bytes32 s)",
  ];

  async function signPermit(signer: ethers.Signer, spender: string, amount: bigint, deadline: bigint) {
    const usdc    = new ethers.Contract(USDC, ABI, signer);
    const owner   = await signer.getAddress();
    const nonce   = await usdc.nonces(owner);
    const chainId = (await signer.provider!.getNetwork()).chainId;

    const sig = await signer.signTypedData(
      { name: await usdc.name(), version: await usdc.version(), chainId, verifyingContract: USDC },
      { Permit: [
          { name: "owner",    type: "address" },
          { name: "spender",  type: "address" },
          { name: "value",    type: "uint256" },
          { name: "nonce",    type: "uint256" },
          { name: "deadline", type: "uint256" },
      ]},
      { owner, spender, value: amount, nonce, deadline }
    );
    return { ...ethers.Signature.from(sig), deadline };
  }
  ```

  ---

  ## Cross-Chain Bridging (CCTP V2)

  For bridging USDC to/from Arc Testnet via Circle's CCTP V2, see
  [docs/cctp-v2.md](./cctp-v2.md) — it covers Arc-specific nonce encoding, the gas estimation
  bug, RPC endpoint selection, attestation timing, and the ethers.js chain-switch workaround.

  ---

  ## Smart Contract Verification

  See [docs/contract-verification.md](./contract-verification.md) for Hardhat and Foundry configuration
  to verify contracts on ArcScan. Tracked in [arc-node#84](https://github.com/circlefin/arc-node/issues/84).

  ---

  ## Testnet Resources

  | Resource | URL |
  |---|---|
  | Block explorer | https://testnet.arcscan.app |
  | Circle USDC Faucet | https://faucet.circle.com |
  | Arc Developer Docs | https://docs.arc.network |
  | CCTP Docs | https://developers.circle.com/stablecoins/cctp-getting-started |
  