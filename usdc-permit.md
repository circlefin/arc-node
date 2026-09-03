# USDC EIP-2612 Permit

Arc Testnet USDC supports EIP-2612 `permit`, allowing a token allowance to be approved with an off-chain signature instead of a separate on-chain `approve` transaction.

## Arc Testnet USDC

- Chain ID: `5042002`
- USDC contract: `0x3600000000000000000000000000000000000000`

## EIP-712 Domain

When signing an EIP-2612 permit for Arc Testnet USDC, use:

```ts
const domain = {
  name: "USDC",
  version: "2",
  chainId: 5042002,
  verifyingContract: USDC_ADDRESS,
};
const types = {
  Permit: [
    { name: "owner", type: "address" },
    { name: "spender", type: "address" },
    { name: "value", type: "uint256" },
    { name: "nonce", type: "uint256" },
    { name: "deadline", type: "uint256" },
  ],
};
const nonce = await publicClient.readContract({
  address: USDC_ADDRESS,
  abi: erc20PermitAbi,
  functionName: "nonces",
  args: [userAddress],
});

const deadline = BigInt(Math.floor(Date.now() / 1000) + 3600);

const signature = await walletClient.signTypedData({
  domain: {
    name: "USDC",
    version: "2",
    chainId: 5042002n,
    verifyingContract: USDC_ADDRESS,
  },
  types: {
    Permit: [
      { name: "owner", type: "address" },
      { name: "spender", type: "address" },
      { name: "value", type: "uint256" },
      { name: "nonce", type: "uint256" },
      { name: "deadline", type: "uint256" },
    ],
  },
  primaryType: "Permit",
  message: {
    owner: userAddress,
    spender: relayerAddress,
    value: amount,
    nonce,
    deadline,
  },
});
```
