# Contract Verification on ArcScan

  ArcScan (https://testnet.arcscan.app) is powered by Blockscout. Verified contracts display their
  source code, ABI, and read/write interfaces — significantly improving the developer experience
  for anyone integrating your contract.

  ---

  ## Hardhat — `@nomicfoundation/hardhat-verify`

  ### Installation

  ```bash
  npm install --save-dev @nomicfoundation/hardhat-verify
  ```

  ### Configuration

  Add the following to your `hardhat.config.ts`. The `etherscan` block tells `hardhat-verify`
  where Blockscout's verification API is. The dummy API key is required — Blockscout ignores the
  value but `hardhat-verify` rejects an empty string.

  ```typescript
  import "@nomicfoundation/hardhat-verify";
  import { HardhatUserConfig } from "hardhat/types";

  const config: HardhatUserConfig = {
    solidity: {
      version: "0.8.20",
      settings: {
        optimizer: { enabled: true, runs: 200 },
      },
    },
    networks: {
      arc: {
        url: process.env.ARC_RPC_URL ?? "https://arc-testnet.drpc.org",
        chainId: 5042002,
        accounts: [process.env.PRIVATE_KEY!],
      },
      arcMainnet: {
        url: process.env.ARC_MAINNET_RPC_URL ?? "",
        chainId: 5042,
        accounts: [process.env.PRIVATE_KEY!],
      },
    },
    etherscan: {
      apiKey: {
        arc: "empty",            // Blockscout ignores API keys — any non-empty string works
        arcMainnet: "empty",
      },
      customChains: [
        {
          network: "arc",
          chainId: 5042002,      // Arc Testnet
          urls: {
            apiURL: "https://testnet.arcscan.app/api",
            browserURL: "https://testnet.arcscan.app",
          },
        },
        {
          network: "arcMainnet",
          chainId: 5042,         // Arc Mainnet
          urls: {
            apiURL: "https://arcscan.app/api",
            browserURL: "https://arcscan.app",
          },
        },
      ],
    },
    sourcify: {
      enabled: false,            // Prevents conflicting verification attempts via Sourcify
    },
  };

  export default config;
  ```

  ### Verify a deployed contract

  ```bash
  npx hardhat verify --network arc <CONTRACT_ADDRESS> <CONSTRUCTOR_ARG1> <CONSTRUCTOR_ARG2>
  ```

  For a contract with no constructor arguments:

  ```bash
  npx hardhat verify --network arc <CONTRACT_ADDRESS>
  ```

  ---

  ## Foundry — `forge verify-contract`

  ### Configuration (`foundry.toml`)

  ```toml
  [profile.default]
  src = "src"
  out = "out"
  libs = ["lib"]
  optimizer = true
  optimizer_runs = 200

  [etherscan]
  arc = { key = "empty", url = "https://testnet.arcscan.app/api" }
  arcMainnet = { key = "empty", url = "https://arcscan.app/api" }
  ```

  ### Verify command

  ```bash
  forge verify-contract \
    <CONTRACT_ADDRESS> \
    src/MyContract.sol:MyContract \
    --chain-id 5042002 \
    --etherscan-api-key empty \
    --verifier-url https://testnet.arcscan.app/api \
    --constructor-args $(cast abi-encode "constructor(address,uint256)" <ARG1> <ARG2>)
  ```

  For a no-argument constructor:

  ```bash
  forge verify-contract \
    <CONTRACT_ADDRESS> \
    src/MyContract.sol:MyContract \
    --chain-id 5042002 \
    --etherscan-api-key empty \
    --verifier-url https://testnet.arcscan.app/api
  ```

  ---

  ## Manual Verification via ArcScan UI

  1. Navigate to your contract on ArcScan:
     `https://testnet.arcscan.app/address/<CONTRACT_ADDRESS>?tab=contract`
  2. Click **"Verify & Publish"**
  3. Select **"Via flattened source code"**
  4. Fill in:
     - **Contract name**: exact name from the Solidity file
     - **Compiler version**: e.g., `v0.8.20+commit.a1b79de6`
     - **Optimization**: Yes / No (must match your actual compilation settings)
     - **Runs**: e.g., 200 (must match exactly)
     - **EVM version**: e.g., `shanghai` or `paris` (must match your compilation settings)
  5. Paste the flattened source code

  Flatten with Hardhat:
  ```bash
  npx hardhat flatten src/MyContract.sol > MyContract.flat.sol
  ```

  Or with Foundry:
  ```bash
  forge flatten src/MyContract.sol > MyContract.flat.sol
  ```

  ---

  ## Important: Bytecode Must Match Exactly

  Blockscout verification requires the compiled bytecode to match the on-chain deployed bytecode
  **exactly**, including the CBOR metadata suffix. This means:

  - **Compiler version** must match to the commit hash (e.g., `v0.8.20+commit.a1b79de6`)
  - **Optimizer settings** (enabled, runs) must match exactly
  - **EVM version target** must match (check `evmVersion` in your `hardhat.config.ts` or `foundry.toml`)
  - **Source code** must be identical to what was compiled — even whitespace-only changes
    invalidate the CBOR metadata hash and cause a mismatch

  If your source has been modified since deployment (e.g., you upgraded the contract logic but
  deployed an old compiled binary), the original source is effectively lost. The only path forward
  is to redeploy with the current source and verify the new contract.

  ---

  ## Blockscout Verification API

  For programmatic or scripted verification, the Blockscout v2 REST API accepts flattened
  source code directly:

  ```bash
  curl -X POST https://testnet.arcscan.app/api/v2/smart-contracts/<ADDRESS>/verification/via/flattened-code \
    -H "Content-Type: application/json" \
    -d '{
      "compiler_version": "v0.8.20+commit.a1b79de6",
      "source_code": "<FLATTENED_SOLIDITY_SOURCE>",
      "is_optimization_enabled": true,
      "optimization_runs": 200,
      "contract_name": "MyContract",
      "evm_version": "default",
      "autodetect_constructor_args": true
    }'
  ```

  A `200` response with `{"message":"Smart-contract verification started"}` means the request was
  accepted. Poll the contract endpoint to check whether verification succeeded:

  ```bash
  curl https://testnet.arcscan.app/api/v2/smart-contracts/<ADDRESS>
  # Look for "is_verified": true in the response
  ```

  ---

  ## Related Issues

  - [arc-node#84](https://github.com/circlefin/arc-node/issues/84) — Tracking the missing official documentation
  