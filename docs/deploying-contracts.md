# Deploying Solidity contracts on Arc

Arc is EVM-compatible, but developer tooling should be configured with the Arc
network parameters explicitly. This page covers the settings that are most often
needed when deploying contracts to Arc Testnet.

## Arc Testnet network parameters

```ts
const arcTestnet = {
  chainId: 5042002,
  chainName: "Arc Testnet",
  nativeCurrency: {
    name: "USDC",
    symbol: "USDC",
    decimals: 18,
  },
  rpcUrls: ["https://rpc.testnet.arc.network"],
  blockExplorerUrls: ["https://testnet.arcscan.app"],
};
```

## Solidity compiler target

When compiling contracts for Arc Testnet, target the Paris EVM unless your
contract stack has confirmed support for newer EVM opcodes on the target Arc
network.

Solidity 0.8.20 and later can emit the Shanghai `PUSH0` opcode by default.
Using `evmVersion: "paris"` avoids emitting `PUSH0` and keeps bytecode
compatible with pre-Shanghai EVM targets.

Hardhat example:

```ts
import type { HardhatUserConfig } from "hardhat/config";

const config: HardhatUserConfig = {
  solidity: {
    version: "0.8.24",
    settings: {
      optimizer: { enabled: true, runs: 200 },
      evmVersion: "paris",
    },
  },
  networks: {
    arcTestnet: {
      url: "https://rpc.testnet.arc.network",
      chainId: 5042002,
      accounts: process.env.PRIVATE_KEY ? [process.env.PRIVATE_KEY] : [],
    },
  },
};

export default config;
```

Foundry example:

```toml
# foundry.toml
evm_version = "paris"
rpc_endpoints = { arc_testnet = "https://rpc.testnet.arc.network" }
```

## Hardhat contract verification on Arcscan

Arcscan is Blockscout-based. With `@nomicfoundation/hardhat-verify`, configure a
custom chain that points to the Arcscan API endpoint.

```ts
import "@nomicfoundation/hardhat-verify";
import type { HardhatUserConfig } from "hardhat/config";

const config: HardhatUserConfig = {
  networks: {
    arcTestnet: {
      url: "https://rpc.testnet.arc.network",
      chainId: 5042002,
      accounts: process.env.PRIVATE_KEY ? [process.env.PRIVATE_KEY] : [],
    },
  },
  etherscan: {
    apiKey: {
      arcTestnet: "empty",
    },
    customChains: [
      {
        network: "arcTestnet",
        chainId: 5042002,
        urls: {
          apiURL: "https://testnet.arcscan.app/api",
          browserURL: "https://testnet.arcscan.app",
        },
      },
    ],
  },
  sourcify: {
    enabled: false,
  },
};

export default config;
```

Then verify with constructor arguments as usual:

```bash
npx hardhat verify --network arcTestnet <contract-address> <constructor-args>
```

Notes:

- Blockscout does not require a real Etherscan API key, but
  `@nomicfoundation/hardhat-verify` still expects a non-empty string.
- Keep the explorer URL as `https://testnet.arcscan.app` for wallet and dApp
  transaction links.
