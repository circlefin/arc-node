# Arc Contracts

This directory contains Solidity contracts and tests for the Arc project, built with Foundry.


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