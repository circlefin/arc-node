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
[profile.default]
solc = "0.8.20"
evm_version = "paris"
optimizer = true
optimizer_runs = 200
# Useful when your contract hits Solidity's stack-depth limit.
via_ir = true

[rpc_endpoints]
arc_testnet = "https://rpc.testnet.arc.network"
```

Deploy with:

```bash
forge script script/Deploy.s.sol:Deploy \
  --rpc-url https://rpc.testnet.arc.network \
  --private-key "$DEPLOYER_PRIVATE_KEY" \
  --broadcast \
  --config-path foundry.toml
```

The deployer wallet needs Arc Testnet USDC for gas. Testnet USDC is available
from the [Circle faucet](https://faucet.circle.com).

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

## Foundry contract verification on Arcscan

Foundry can verify directly against Arcscan's Blockscout API:

```bash
forge verify-contract \
  --verifier blockscout \
  --verifier-url "https://testnet.arcscan.app/api/" \
  --compiler-version 0.8.20 \
  --chain 5042002 \
  <contract-address> src/<Contract>.sol:<Contract>
```

For constructor arguments, pass ABI-encoded args:

```bash
forge verify-contract \
  --verifier blockscout \
  --verifier-url "https://testnet.arcscan.app/api/" \
  --compiler-version 0.8.20 \
  --chain 5042002 \
  --constructor-args "$(cast abi-encode 'constructor(address,address)' <arg1> <arg2>)" \
  <contract-address> src/<Contract>.sol:<Contract>
```

Notes:

- Blockscout does not require a real Etherscan API key, but
  `@nomicfoundation/hardhat-verify` still expects a non-empty string.
- Keep the explorer URL as `https://testnet.arcscan.app` for wallet and dApp
  transaction links.

## Common Arc Testnet references

| Item                 | Value                                        |
| -------------------- | -------------------------------------------- |
| Chain ID             | `5042002` (`0x4cef52`)                       |
| RPC                  | `https://rpc.testnet.arc.network`            |
| Explorer             | `https://testnet.arcscan.app`                |
| Native USDC / gas    | `0x3600000000000000000000000000000000000000` |
| EURC                 | `0x89B50855Aa3bE2F677cD6303Cec089B5F319D72a` |
| CCTP domain          | `26`                                         |
| TokenMessengerV2     | `0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA` |
| MessageTransmitterV2 | `0xE737e5cEBEEBa77EFE34D4aa090756590b1CE275` |

For explorer readability, contracts can expose an `owner` view such as
`address public owner`. Arcscan displays public variables in the contract view,
which can help operators identify deployed instances.
