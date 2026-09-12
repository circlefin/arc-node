# Arc Contracts

This directory contains Solidity contracts and tests for the Arc project, built with Foundry.

## Compiler choice for genesis-deployed contracts

**Forge is the canonical compiler** for every CREATE2-deployed contract in Arc genesis
(`Memo`, `Multicall3From`, `Denylist` impl, `ProtocolConfig` impl, `ValidatorRegistry`
impl, `PermissionedValidatorManager` impl, `GasGuzzler`, `TestToken`).

The genesis builder (`contracts/scripts/ArtifactHelper.s.sol`), all CREATE2-sensitive
tests (`tests/localdev/genesis.test.ts`) all read from `contracts/out/forge/`. 
Hardhat's compile output is **not** consumed for any CREATE2-sensitive path.

## Verifying contracts on Arcscan

Arc Testnet's explorer ([testnet.arcscan.app](https://testnet.arcscan.app)) exposes an
**Etherscan-compatible** contract verification API at `https://testnet.arcscan.app/api`.
Blockscout-native field names (`addressHash`, `name`, `contractSourceCode`, …) are **not**
accepted and return a misleading `Missing codeformat field` error even when `codeformat` is
present — that is the failure mode reported in #210. Use Etherscan V1 field names
(`contractaddress`, `contractname`, `sourceCode`, `codeformat`, …) or a tool that already
speaks that dialect.

### Foundry

Arcscan ignores the API key value but Foundry still requires a non-empty key:

```bash
export ARC_TESTNET_RPC_URL=https://rpc.testnet.arc.io
forge verify-contract <address> path/to/Contract.sol:ContractName \
  --chain 5042002 \
  --verifier custom \
  --verifier-url https://testnet.arcscan.app/api \
  --verifier-api-key empty \
  --watch
```

Pass constructor args with `--constructor-args` / `--constructor-args-path` when needed.
`foundry.toml` also registers an `arc_testnet` Etherscan entry so `--chain arc-testnet` (or `5042002`) works
once `ETHERSCAN_API_KEY` (any non-empty string) is set.

### Hardhat

```bash
export ARC_TESTNET_RPC_URL=https://rpc.testnet.arc.io
npx hardhat verify --network testnet <address> [constructorArgs...]
```

This requires `@nomicfoundation/hardhat-verify` with an Arc Testnet `customChains` /
`etherscan` entry for chain ID `5042002` pointing at `https://testnet.arcscan.app/api`
(see open wiring in related PRs if it is not yet on `main`). Disable Sourcify so it does not
race Arcscan.

## Foundry

**Foundry is a blazing fast, portable and modular toolkit for Ethereum application development written in Rust.**

Foundry consists of:

-   **Forge**: Ethereum testing framework (like Truffle, Hardhat and DappTools).
-   **Cast**: Swiss army knife for interacting with EVM smart contracts, sending transactions and getting chain data.
-   **Anvil**: Local Ethereum node, akin to Ganache, Hardhat Network.
-   **Chisel**: Fast, utilitarian, and verbose solidity REPL.

## Documentation

https://book.getfoundry.sh/

## Usage

### Unit Testing (Current Directory)

#### Build Contracts
```shell
$ forge build
```

#### Run All Unit Tests
```shell
$ forge test
```

#### Run Specific Test Contract
```shell
$ forge test --match-contract <contract_name>
```

#### Run Tests with Verbose Output
```shell
$ forge test -v
```

#### Run Tests with Gas Reports
```shell
$ forge test --gas-report
```

#### Format Code
```shell
$ forge fmt
```

#### Gas Snapshots
```shell
$ forge snapshot
```

### Development Tools

#### Local Development Node
```shell
$ anvil
```

#### Interact with Contracts
```shell
$ cast <subcommand>
```

#### Deploy Contracts (if needed)
```shell
$ forge script script/Deploy.s.sol:DeployScript --rpc-url <your_rpc_url> --private-key <your_private_key>
```

### Help

```shell
$ forge --help
$ anvil --help
$ cast --help
```