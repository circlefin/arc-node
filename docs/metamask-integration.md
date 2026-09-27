# MetaMask Integration Guide

Guide for integrating Arc Testnet with MetaMask, including USDC token setup.

## Add Arc Testnet to MetaMask

Use `wallet_addEthereumChain` to add Arc Testnet:

```typescript
await window.ethereum.request({
  method: "wallet_addEthereumChain",
  params: [{
    chainId: "0x4CEF52",  // 5042002 in hex
    chainName: "Arc Testnet",
    nativeCurrency: {
      name: "USDC",
      symbol: "USDC",
      decimals: 18,
    },
    rpcUrls: ["https://rpc.drpc.testnet.arc.io"],
    blockExplorerUrls: ["https://testnet.arcscan.app"],
  }],
});
```

### Why the native currency is USDC with 18 decimals

Arc pays gas in USDC and the native balance carries 18 decimals, so `{ name: "USDC", symbol: "USDC", decimals: 18 }` is the config to send. Circle's [Connect to Arc](https://docs.arc.io/arc/references/connect-to-arc) reference publishes those values. So do viem's built in `arcTestnet` chain and this repo's own [chain config](../scripts/hardhat/chains/config.ts).

MetaMask requires `nativeCurrency.decimals` to be exactly 18 and rejects anything else with `invalidParams` ("Expected the number 18 for 'nativeCurrency.decimals'"). It only requires the symbol to be a 1 to 6 character string, so `USDC` is accepted.

The part that trips people up is that one balance is exposed at two precisions:

- The native balance, the one that pays gas, has 18 decimals. `eth_getBalance` returned `74183684466781322809` for an account holding 74.183684466781322809 USDC.
- The ERC-20 interface at `0x3600000000000000000000000000000000000000` reports `decimals() = 6` and truncates. `balanceOf` returned `74183684` for that same account in the same block.

That is why `decimals: 6` is wrong here as well as rejected: MetaMask would render the gas balance 10^12 times too high. Register the ERC-20 with `decimals: 6` as shown below and keep the two views apart when you format amounts yourself.

Issue [#95](https://github.com/circlefin/arc-node/issues/95) asks for this to be documented and suggests passing `symbol: "ETH"` to satisfy MetaMask. The decimals requirement in that issue is real. The symbol part is not: Circle's own MetaMask instructions use `USDC`.

## Switching to Arc Testnet

Do not use `wallet_switchEthereumChain` to move the user to Arc Testnet. On Arc Testnet it fails silently or throws a `4902` (chain not found) even after the network has already been added (issue [#89](https://github.com/circlefin/arc-node/issues/89)).

Call `wallet_addEthereumChain` instead. It both adds the network if it is missing and switches to it if it is already present, so it is reliable in both cases:

```typescript
// Unreliable on Arc Testnet, may resolve without switching or throw 4902
// await window.ethereum.request({
//   method: "wallet_switchEthereumChain",
//   params: [{ chainId: "0x4CEF52" }],
// });

// Reliable, works whether the network is already added or not
await window.ethereum.request({
  method: "wallet_addEthereumChain",
  params: [{
    chainId: "0x4CEF52",
    chainName: "Arc Testnet",
    nativeCurrency: { name: "USDC", symbol: "USDC", decimals: 18 },
    rpcUrls: ["https://rpc.drpc.testnet.arc.io"],
    blockExplorerUrls: ["https://testnet.arcscan.app"],
  }],
});
```

## Wait for the RPC Transport Before Sending Transactions

`wallet_addEthereumChain` resolving does not mean transactions route to Arc Testnet yet. For a short window (roughly 200 to 500 ms) MetaMask already reports the new chain from `eth_chainId` while its internal JSON-RPC router still points at the previous endpoint. A transaction sent in that window lands on the old chain (issue [#130](https://github.com/circlefin/arc-node/issues/130)). The reported case was a CCTP `receiveMessage` meant for Arc Testnet that landed on Base Sepolia instead and reverted with `"Invalid destination domain"`.

Polling `eth_chainId` on its own does not close the gap, since it flips before the routing does. Retry with a backoff, re-read the chain live on every attempt, then refuse to send if the wallet never settles:

```typescript
import { ethers } from "ethers";

// After wallet_addEthereumChain resolves, wait for the provider
// transport to catch up before sending any transaction.
async function waitForProviderChain(
  expectedChainId: number,
  retries = 6,
): Promise<void> {
  for (let i = 0; i < retries; i++) {
    if (i > 0) await new Promise((r) => setTimeout(r, 500 * i));
    // Must be a new instance each iteration: ethers caches the network per
    // BrowserProvider, so a reused one goes stale or throws "network changed".
    const provider = new ethers.BrowserProvider(window.ethereum);
    const network = await provider.getNetwork();
    if (Number(network.chainId) === expectedChainId) return;
  }
  throw new Error("Network did not stabilize. Switch manually and retry.");
}

await window.ethereum.request({
  method: "wallet_addEthereumChain",
  params: [arcTestnetConfig],
});
await waitForProviderChain(5042002);
// Now safe to send transactions
```

Construct a fresh `BrowserProvider` on each attempt, as the comment in the loop says. ethers caches the network from the first `getNetwork()` call on an instance. Later calls compare that cached value against a fresh `eth_chainId`. A provider that was not created with the `"any"` network then throws `network changed: <old> => <new>` rather than reporting the new chain (`AbstractProvider.getNetwork`, ethers 6.17.0). A provider pinned to a network or created with `staticNetwork: true` is worse: it stops re-reading altogether and returns the stale chain for as long as it lives. A `getProvider()` helper that always returns `new ethers.BrowserProvider(window.ethereum)`, never a cached singleton, keeps this right everywhere.

`getNetwork()` on a fresh instance sends `eth_chainId` over the same EIP-1193 channel a hand written poll would use, so the guarantee comes from the backoff and the fresh read rather than from the method you call. Keep the throw at the end. A flow that gives up loudly is better than one that puts a transaction on the wrong chain.

The wait is only needed when a transaction follows the network switch in the same flow. Reading balances or registering tokens is not affected.

### The same trap in viem

viem has the same failure in a different place. `client.chain` is the chain object passed at construction, so `client.chain.id` is a static config value that never follows the wallet. Read the chain with `getChainId()`, which sends `eth_chainId` on every call:

```typescript
import { createPublicClient, custom } from "viem";
import { arcTestnet } from "viem/chains";

const client = createPublicClient({
  chain: arcTestnet,
  transport: custom(window.ethereum),
});

const configured = client.chain.id; // 5042002 from the chain object, never re-read
const live = await client.getChainId(); // eth_chainId through the wallet

async function waitForWalletChain(expectedChainId: number, retries = 6): Promise<void> {
  for (let i = 0; i < retries; i++) {
    if (i > 0) await new Promise((r) => setTimeout(r, 500 * i));
    if ((await client.getChainId()) === expectedChainId) return;
  }
  throw new Error("Network did not stabilize. Switch manually and retry.");
}
```

Unlike ethers, the client can be reused across attempts. `getChainId()` dedupes only the requests that are in flight at the same moment, so each awaited call reaches the wallet (`withDedupe` clears its cache entry as soon as the promise settles). Arc Testnet ships in viem as `arcTestnet`, so there is no chain definition to hand write.

## Register USDC Token

**Important:** MetaMask's automatic token detection does not cover custom networks, so USDC does not appear in the token list on its own (issue [#97](https://github.com/circlefin/arc-node/issues/97)). After adding the network, register it with `wallet_watchAsset`:

```typescript
await window.ethereum.request({
  method: "wallet_watchAsset",
  params: {
    type: "ERC20",
    options: {
      address: "0x3600000000000000000000000000000000000000",
      symbol: "USDC",
      decimals: 6,
      image: "https://cryptologos.cc/logos/usd-coin-usdc-logo.png",
    },
  },
});
```

MetaMask will show a confirmation dialog. Once accepted, USDC will appear in the user's token list with the correct balance. Note that USDC uses `decimals: 6` here, which is correct for the ERC-20 token and separate from the 18-decimal native currency above.

**Order matters:** call `wallet_watchAsset` only after the `wallet_addEthereumChain` promise has resolved. If it fires while the user is still on a different network, MetaMask registers USDC against the wrong chain and the balance never shows up. The onboarding flow below awaits the chain add before registering the token for this reason.

## Complete Onboarding Flow

Recommended sequence for DApp wallet connection:

```typescript
async function connectWallet() {
  if (!window.ethereum) {
    throw new Error("No wallet detected. Install MetaMask to continue.");
  }

  try {
    // 1. Request account access
    const accounts = await window.ethereum.request({
      method: "eth_requestAccounts",
    });

    // 2. Add Arc Testnet network (also switches to it if already added)
    await window.ethereum.request({
      method: "wallet_addEthereumChain",
      params: [{
        chainId: "0x4CEF52",
        chainName: "Arc Testnet",
        nativeCurrency: {
          name: "USDC",
          symbol: "USDC",
          decimals: 18,
        },
        rpcUrls: ["https://rpc.drpc.testnet.arc.io"],
        blockExplorerUrls: ["https://testnet.arcscan.app"],
      }],
    });

    // 3. Register USDC token
    await window.ethereum.request({
      method: "wallet_watchAsset",
      params: {
        type: "ERC20",
        options: {
          address: "0x3600000000000000000000000000000000000000",
          symbol: "USDC",
          decimals: 6,
          image: "https://cryptologos.cc/logos/usd-coin-usdc-logo.png",
        },
      },
    });

    return accounts[0];
  } catch (error) {
    // 4001 is the EIP-1193 code for the user declining a prompt. All three
    // calls above can raise it, so treat it as an outcome to handle and not
    // an error to report.
    if ((error as { code?: number }).code === 4001) {
      return null;
    }
    throw error;
  }
}
```

If the flow continues straight into a transaction, run `waitForProviderChain` from the section above between steps 2 and 3.

`window.ethereum` is missing when no wallet is installed, so check it before the first call. Returning `null` on `4001` lets the caller re-prompt instead of surfacing a stack trace to someone who pressed cancel. If several wallet extensions are installed they compete for `window.ethereum`. ethers exposes `BrowserProvider.discover()` for the EIP-6963 announcement flow when you need to pick a specific one.

## One Balance, Two Views

The native balance and the ERC-20 at `0x3600000000000000000000000000000000000000` are the same funds, exposed twice. Once the network is registered with `symbol: "USDC"`, MetaMask reads the native row as the user's USDC balance at 18 decimals. Registering the token adds a second row for that same balance at 6 decimals, which is the view DApps actually transact against, since transfers, approvals and allowances all go through the ERC-20 interface.

Two things follow. The two rows are one balance, so never add them together. Read `decimals()` rather than assuming which view an amount came from, which is the advice Circle gives on its [contract addresses](https://docs.arc.io/arc/references/contract-addresses) page.

Without the `wallet_watchAsset` call the token row never appears. A user who receives USDC sees nothing in their token list and assumes the transfer failed. Every DApp on Arc Testnet that moves USDC should include the call in its onboarding flow.

## Contract Addresses

**Arc Testnet:**
- USDC ERC-20 interface: `0x3600000000000000000000000000000000000000`, `decimals() = 6`
- Chain ID: `5042002` (hex: `0x4CEF52`)
- Native currency: USDC, 18 decimals
- RPC: `https://rpc.drpc.testnet.arc.io`
- Explorer: `https://testnet.arcscan.app`

Circle publishes four testnet RPC hosts, `rpc.testnet.arc.io` plus dRPC, QuickNode and Blockdaemon variants of it ([RPC endpoints](https://docs.arc.io/arc/references/rpc-endpoints)). All four answered `eth_chainId` with `0x4cef52`. `rpc.drpc.testnet.arc.io` and `rpc.blockdaemon.testnet.arc.io` return `Access-Control-Allow-Origin: *`, while `rpc.testnet.arc.io` and the QuickNode host reflect the request origin instead, so dRPC is the simplest default for a page that calls the RPC directly from the browser (see issue [#90](https://github.com/circlefin/arc-node/issues/90)). This only matters for requests your own code makes. MetaMask sends the calls behind `rpcUrls` from the extension, where page CORS does not apply.

The older `*.testnet.arc.network` hosts still answer, but arc.io is the domain Circle documents now.

`https://testnet.arcscan.app` is the public block explorer, so use it in the `blockExplorerUrls` field and in any transaction-link examples. `explorer.testnet.arc.network` has no DNS record. `explorer.arc.io` redirects to Circle's Cloudflare Access sign in rather than serving a public explorer. Endpoints and explorer hosts last checked 2026-08-02.

## References

- [MetaMask wallet_addEthereumChain](https://docs.metamask.io/wallet/reference/json-rpc-methods/wallet_addethereumchain/)
- [MetaMask wallet_watchAsset](https://docs.metamask.io/wallet/reference/json-rpc-methods/wallet_watchasset/)
- [EIP-1193 provider errors](https://eips.ethereum.org/EIPS/eip-1193#provider-errors), where `4001` is defined
- [Connect to Arc](https://docs.arc.io/arc/references/connect-to-arc), the published Arc Testnet parameters
- [Arc contract addresses](https://docs.arc.io/arc/references/contract-addresses)
- [Arc documentation](https://docs.arc.io/)
