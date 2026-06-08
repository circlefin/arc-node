# CCTP V2 Integration on Arc

This guide covers Circle's Cross-Chain Transfer Protocol (CCTP) V2 on Arc
Testnet. It documents the Arc-specific values a developer needs to move native
USDC on and off Arc plus the integration details that are easy to get wrong.

CCTP moves native USDC between chains by burning it on the source chain and
minting it on the destination chain, with no wrapped tokens and no liquidity
pools. The flow is the same on Arc as on any other supported chain:

1. Approve the TokenMessenger to spend your USDC on the source chain.
2. Call `depositForBurn` on the source chain. This burns the USDC and emits a
   `MessageSent` event.
3. Fetch the attestation for that burn from Circle's Iris API.
4. Call `receiveMessage` on the destination chain with the message and
   attestation. This mints the USDC to the recipient.

## Arc Testnet domain is 26

Every CCTP transfer identifies the destination chain by a numeric domain, not by
chain ID. Arc's CCTP domain is **26** (the testnet shares the mainnet domain).

```ts
const ARC_DOMAIN = 26;
```

For reference, the domains used in the examples below:

| Chain | CCTP domain |
| --- | --- |
| Ethereum (Sepolia) | 0 |
| Avalanche (Fuji) | 1 |
| Base (Sepolia) | 6 |
| Arc | 26 |

The full list is in Circle's
[supported blockchains](https://developers.circle.com/cctp/cctp-supported-blockchains).

## Contract addresses on Arc Testnet

These are the CCTP V2 contracts deployed on Arc Testnet (domain 26), from
Circle's [EVM smart contracts](https://developers.circle.com/cctp/evm-smart-contracts)
reference:

| Contract | Address |
| --- | --- |
| TokenMessengerV2 | `0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA` |
| MessageTransmitterV2 | `0xE737e5cEBEEBa77EFE34D4aa090756590b1CE275` |
| TokenMinterV2 | `0xb43db544E2c27092c107639Ad201b3dEfAbcF192` |
| MessageV2 | `0xbaC0179bB358A8936169a63408C8481D582390C4` |

USDC on Arc Testnet is the native gas token at
`0x3600000000000000000000000000000000000000`.

## Use the V2 `depositForBurn`, not V1

All CCTP contracts on Arc are V2. The V2 `depositForBurn` takes seven
parameters:

```solidity
function depositForBurn(
    uint256 amount,
    uint32 destinationDomain,
    bytes32 mintRecipient,
    address burnToken,
    bytes32 destinationCaller,   // V2: who may call receiveMessage, or bytes32(0) for anyone
    uint256 maxFee,              // V2: max fee in burnToken units
    uint32 minFinalityThreshold  // V2: finality level to attest at
) external returns (uint64 nonce);
```

The V1 function had only four parameters
(`amount, destinationDomain, mintRecipient, burnToken`). The two ABIs have
different selectors:

- V2 `depositForBurn`: `0x8e0250ee`
- V1 `depositForBurn`: `0x6fd3504e`

If a library or copied example calls the V1 selector against Arc's V2
TokenMessenger, the call reverts on-chain with no useful error data. If you are
porting code from an older Circle integration, confirm it is built against the
V2 ABI.

## minFinalityThreshold: use 2000 for burns from Arc

`minFinalityThreshold` controls the finality level Circle's attestation service
waits for before it signs the message:

- **1000**: Fast Transfer (confirmed, not yet finalized)
- **2000**: Standard Transfer (finalized)

A value above 2000 is capped to 2000.

For burns sourced **from Arc Testnet**, use **2000**. A burn submitted with 1000
can leave the attestation stuck in `pending` in the Iris API rather than
progressing to `complete`, so the destination mint never becomes available. The
other testnets in a typical setup (Ethereum Sepolia, Base Sepolia, Avalanche
Fuji) attest fine at 1000.

```ts
// Burn sourced from Arc Testnet
const minFinalityThreshold = 2000;
```

Note also that Fast Transfers can carry a non-zero minimum fee depending on the
route, while finalized transfers to Arc currently do not. You can check the live
fee for a route with the Iris fees endpoint:

```bash
# fees for a burn from Ethereum (0) to Arc (26)
curl https://iris-api-sandbox.circle.com/v2/burn/USDC/fees/0/26
# -> [{"finalityThreshold":1000,"minimumFee":1},{"finalityThreshold":2000,"minimumFee":0}]
```

Your `maxFee` must be greater than or equal to the minimum fee for the route. A
lower `maxFee` reverts the burn on-chain.

## Estimating gas for CCTP calls

`eth_estimateGas` can fail for `depositForBurn` (and intermittently for the
preceding ERC-20 `approve`) on Arc Testnet, returning an internal error with
`data: null`. Clients that estimate gas automatically (ethers.js, viem, wagmi)
will throw before the transaction is ever submitted.

Pass an explicit gas limit to skip estimation:

```ts
const tx = await tokenMessenger.depositForBurn(
  amount, destinationDomain, mintRecipient, burnToken,
  destinationCaller, maxFee, minFinalityThreshold,
  { gasLimit: 600_000n } // skip eth_estimateGas
);
```

A limit of 600,000 covers an `approve` plus a CCTP burn with headroom. Actual
gas used is typically in the 180,000 to 260,000 range and the unused portion is
refunded.

## Fetching the attestation

After the burn confirms, poll Circle's Iris API for the attestation, keyed by
the source domain and the burn transaction hash. On testnet the host is
`https://iris-api-sandbox.circle.com` (mainnet is `https://iris-api.circle.com`):

```bash
# source domain 26 = Arc; pass the burn tx hash
curl "https://iris-api-sandbox.circle.com/v2/messages/26?transactionHash=<burnTxHash>"
```

Poll until the message `status` is `complete` and an `attestation` is present,
then submit the returned `message` and `attestation` to `receiveMessage` on the
destination chain's MessageTransmitterV2. The Iris API is rate limited to 35
requests per second.

## References

- [CCTP supported blockchains](https://developers.circle.com/cctp/cctp-supported-blockchains)
- [CCTP EVM smart contracts](https://developers.circle.com/cctp/evm-smart-contracts)
- [CCTP contract interfaces](https://developers.circle.com/cctp/references/contract-interfaces)
- [CCTP technical guide](https://developers.circle.com/cctp/technical-guide)
