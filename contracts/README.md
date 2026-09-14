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

Arc Testnet's explorer ([testnet.arcscan.app](https://testnet.arcscan.app)) is
Blockscout-based. The repo `hardhat.config.ts` already registers the Arc Testnet
chain for `@nomicfoundation/hardhat-verify`:

```bash
npx hardhat verify --network testnet <address> [constructorArgs...]
```

Set `ARC_TESTNET_RPC_URL` before verifying. Foundry users can instead follow the
`arc-forge verify-contract` flow in the
[Deploy on Arc](https://docs.arc.io/arc/tutorials/deploy-on-arc) tutorial.

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