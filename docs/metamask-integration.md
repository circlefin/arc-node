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
      name: "ETH",
      symbol: "ETH",
      decimals: 18,
    },
    rpcUrls: ["https://rpc.drpc.testnet.arc.network"],
    blockExplorerUrls: ["https://testnet.arcscan.app"],
  }],
});
```

### Why the native currency is "ETH" and not "USDC"

Arc pays gas in USDC, but the `nativeCurrency` here still has to be `{ name: "ETH", symbol: "ETH", decimals: 18 }`. MetaMask only supports 18-decimal native currencies and validates that `decimals` equals 18, so a config with `decimals: 6` is rejected. If you pass `symbol: "USDC"` with `decimals: 18` the network is accepted but MetaMask shows the gas balance 10^12 times too high.

The practical consequences:

- MetaMask labels gas costs in "ETH" (for example "0.000000021 ETH") rather than USDC. Show the real USDC gas estimate in your own DApp UI if that matters to your users.
- This only affects the native gas display. The USDC ERC-20 token you register below keeps `decimals: 6` and shows the correct balance.

See issue [#95](https://github.com/circlefin/arc-node/issues/95) for the full background.

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
    nativeCurrency: { name: "ETH", symbol: "ETH", decimals: 18 },
    rpcUrls: ["https://rpc.drpc.testnet.arc.network"],
    blockExplorerUrls: ["https://testnet.arcscan.app"],
  }],
});
```

## Register USDC Token

**Important:** MetaMask does not automatically show USDC in the token list for custom chains. After adding the network, you must register USDC using `wallet_watchAsset`:

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
          name: "ETH",
          symbol: "ETH",
          decimals: 18,
        },
        rpcUrls: ["https://rpc.drpc.testnet.arc.network"],
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

    console.log("Wallet connected:", accounts[0]);
    return accounts[0];
  } catch (error) {
    console.error("Wallet connection failed:", error);
    throw error;
  }
}
```

## Why This Matters

Without the `wallet_watchAsset` call:
- Users see no USDC balance in MetaMask after receiving tokens
- Users assume transactions failed
- DApps appear broken

Every DApp on Arc Testnet that involves USDC transfers should include this step in their onboarding flow.

## Contract Addresses

**Arc Testnet:**
- USDC: `0x3600000000000000000000000000000000000000`
- Chain ID: `5042002` (hex: `0x4CEF52`)
- RPC: `https://rpc.drpc.testnet.arc.network`
- Explorer: `https://testnet.arcscan.app`

The public RPC `https://rpc.testnet.arc.network` works too, but `rpc.drpc.testnet.arc.network` returns `Access-Control-Allow-Origin: *`, which is the safest choice for browser DApps calling the endpoint directly (see issue [#90](https://github.com/circlefin/arc-node/issues/90)).

`https://testnet.arcscan.app` is the only block explorer that currently resolves. The older `explorer.testnet.arc.network` and `explorer.arc.io` hosts are dead, so use `testnet.arcscan.app` in the `blockExplorerUrls` field and in any transaction-link examples.

## References

- [MetaMask wallet_watchAsset documentation](https://docs.metamask.io/wallet/reference/wallet_watchasset/)
- [MetaMask wallet_addEthereumChain documentation](https://docs.metamask.io/wallet/reference/wallet_addethereumchain/)
- [Arc Network documentation](https://docs.arc.network/)
