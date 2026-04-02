# External contract artifacts

## DeterministicDeploymentProxy

[Repository](https://github.com/Arachnid/deterministic-deployment-proxy) | [Etherscan](https://etherscan.io/address/0x4e59b44847b379578588920ca78fbf26c0b4956c)

## Multicall3

[Repository](https://github.com/mds1/multicall3) | [Etherscan](https://etherscan.io/address/0xca11bde05977b3631167028862be2a173976ca11)

## Permit2

[Repository](https://github.com/Uniswap/permit2/blob/0x000000000022D473030F116dDEE9F6B43aC78BA3) | [Etherscan](https://etherscan.io/address/0x000000000022d473030f116ddee9f6b43ac78ba3)

- Solidity version 0.8.17

Because in [EIP712](https://github.com/Uniswap/permit2/blob/main/src/EIP712.sol#L20), there are two variables initialized by chain ID in the constructor. The code will not be the same on different EVM blockchains.

The difference between forge chainID 31337 (0x7a69) and Ethereum chainID 1.

```diff
 00001b20: CHAINID
+00001b21: PUSH32 0x0000000000000000000000000000000000000000000000000000000000007a69
-00001b21: PUSH32 0x0000000000000000000000000000000000000000000000000000000000000001
 00001b42: SUB
 00001b43: PUSH2 0x1b69
 00001b46: JUMPI
+00001b47: PUSH32 0x4d553c58ae79a6c4ba64f0e690a5d1cd2deff8c6b91cf38300e0f2b76f9ee346
-00001b47: PUSH32 0x866a5aba21966af95d6c7ab78eb2b2fc913915c28be3b9aa07cc04ff903e3f28
 00001b68: SWAP1
```

# Verify the manifest.json and artifacts

```bash
forge test -vvv --match-contract VerifyArtifactTest
```

Compare the code hash with Ethereum mainnet.

```bash
ETH_RPC_URL=https://eth-mainnet.g.alchemy.com/v2/**** forge test -vvv --match-contract VerifyArtifactTest
```
