# Deploying Smart Contracts on Arc Testnet with Foundry

This guide walks through writing, testing, and deploying Solidity smart contracts on Arc Testnet using Foundry.

## Network Configuration

| Parameter | Value |
|-----------|-------|
| RPC URL | https://rpc.testnet.arc.network |
| Chain ID | 5042002 |
| Gas Token | USDC |
| Explorer | https://testnet.arcscan.app |

## Installing Foundry

```bash
curl -L https://foundry.paradigm.xyz | bash
source ~/.bashrc
foundryup
```

## Creating a Project

```bash
forge init my-arc-project && cd my-arc-project
```

## Deploying to Arc Testnet

```bash
forge create src/HelloArc.sol:HelloArc \
  --rpc-url https://rpc.testnet.arc.network \
  --private-key YOUR_PRIVATE_KEY \
  --broadcast
```

## Resources

- [Arc Docs](https://docs.arc.network)
- [Foundry Book](https://book.getfoundry.sh)
- [Explorer](https://testnet.arcscan.app)
