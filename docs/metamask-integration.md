# MetaMask Integration Guide

Guide for integrating Arc Testnet with MetaMask, including USDC token setup.

## Add Arc Testnet to MetaMask

Use `wallet_addEthereumChain` to add Arc Testnet:

```typescript
await window.ethereum.request({
  method: "wallet_addEthereumChain",
  params: [{
    chainId: "0x4CF252",  // 5042002 in hex
    chainName: "Arc Testnet",
    nativeCurrency: {
      name: "USDC",
      symbol: "USDC",
      decimals: 6,
    },
    rpcUrls: ["https://rpc.arc.network"],
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

MetaMask will show a confirmation dialog. Once accepted, USDC will appear in the user's token list with the correct balance.

## Complete Onboarding Flow

Recommended sequence for DApp wallet connection:

```typescript
async function connectWallet() {
  try {
    // 1. Request account access
    const accounts = await window.ethereum.request({
      method: "eth_requestAccounts",
    });

    // 2. Add Arc Testnet network
    await window.ethereum.request({
      method: "wallet_addEthereumChain",
      params: [{
        chainId: "0x4CF252",
        chainName: "Arc Testnet",
        nativeCurrency: {
          name: "USDC",
          symbol: "USDC",
          decimals: 6,
        },
        rpcUrls: ["https://rpc.arc.network"],
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
- Chain ID: `5042002` (hex: `0x4CF252`)
- RPC: `https://rpc.arc.network`
- Explorer: `https://testnet.arcscan.app`

## References

- [MetaMask wallet_watchAsset documentation](https://docs.metamask.io/wallet/reference/wallet_watchasset/)
- [MetaMask wallet_addEthereumChain documentation](https://docs.metamask.io/wallet/reference/wallet_addethereumchain/)
- [Arc Network documentation](https://docs.arc.network/)
