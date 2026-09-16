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
chain ID. Arc's CCTP domain is **26**. Circle lists that domain as "Arc testnet"
and says CCTP supports Arc testnet only, so there is no separate Arc mainnet
domain to switch to yet.

```ts
const ARC_DOMAIN = 26;
```

The deployment will tell you the same thing, which is the check to run if you
suspect the docs have moved on:

```bash
cast call 0xE737e5cEBEEBa77EFE34D4aa090756590b1CE275 \
  "localDomain()(uint32)" --rpc-url <arc-testnet-rpc>
# -> 26
```

For reference, the domains used in the examples below:

| Chain | CCTP domain |
| --- | --- |
| Ethereum (Sepolia) | 0 |
| Avalanche (Fuji) | 1 |
| Base (Sepolia) | 6 |
| Arc | 26 |

The full list is in Circle's
[supported chains and domains](https://developers.circle.com/cctp/concepts/supported-chains-and-domains).

## Contract addresses on Arc Testnet

These are the CCTP V2 contracts deployed on Arc Testnet (domain 26), from
Circle's [contract addresses](https://developers.circle.com/cctp/references/contract-addresses)
reference:

| Contract | Address |
| --- | --- |
| TokenMessengerV2 | `0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA` |
| MessageTransmitterV2 | `0xE737e5cEBEEBa77EFE34D4aa090756590b1CE275` |
| TokenMinterV2 | `0xb43db544E2c27092c107639Ad201b3dEfAbcF192` |
| MessageV2 | `0xbaC0179bB358A8936169a63408C8481D582390C4` |

The addresses cross-reference each other on chain, so you can check the table
without trusting it. `TokenMessengerV2.localMessageTransmitter()` returns the
MessageTransmitterV2 above and `localMinter()` returns the TokenMinterV2.

## USDC amounts are 6 decimals, not 18

USDC on Arc Testnet is the native gas token at
`0x3600000000000000000000000000000000000000`. Some address validators flag that
shape as a precompile or as a zero-padded typo. It is neither. The ERC-20
interface on it behaves normally.

One balance has two views. The ERC-20 interface reports 6 decimals, the same as
USDC everywhere else, while the node's native accounting works in 18-decimal
units. `eth_getBalance` and `balanceOf` on the same account return the same
holding a factor of 10^12 apart.

CCTP only ever uses the ERC-20 view. `amount`, `maxFee` and the allowance you
give the TokenMessenger are all 6-decimal units:

```ts
const amount = 1_000_000n; // 1 USDC, not 1_000_000_000_000_000_000n
```

Passing a native-unit figure asks to burn 10^12 times what you meant, so it
fails on the allowance or balance check rather than moving the wrong amount.

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
TokenMessenger, the call reverts with no error data at all. That absence is the
tell. An `eth_call` carrying the V1 selector comes back as a bare
`execution reverted`, while the same call through the V2 ABI returns a decodable
reason string such as `No TokenMessenger for domain`. If a revert carries no
data, check the selector first. If you are porting code from an older Circle
integration, confirm it is built against the V2 ABI.

## mintRecipient and destinationCaller are bytes32

Both are `bytes32`, not `address`, so a 20-byte address has to be left-padded to
32 bytes first:

```ts
// viem
import { pad } from 'viem'
const mintRecipient = pad(recipient, { size: 32 })

// ethers v6
const mintRecipient = ethers.zeroPadValue(recipient, 32)
```

Current clients refuse the unpadded value rather than sending it. viem throws
`AbiEncodingBytesSizeMismatchError` and ethers v6 throws `INVALID_ARGUMENT:
incorrect data length`, both while encoding, so the call never reaches the
chain. Hand-rolled ABI encoding has no such guard, which is where a
wrong-length recipient can still get through.

`destinationCaller` takes the same 32-byte shape for a different job.
`bytes32(0)` lets any address submit `receiveMessage` for that message, which is
the right default for most integrations. Set it to a padded address and
MessageTransmitterV2 requires the caller to match, reverting with
`Invalid caller for message` for everyone else. That is how a relayer keeps
someone else from calling its mint.

## minFinalityThreshold: use 2000 for burns from Arc

`minFinalityThreshold` controls the finality level Circle's attestation service
waits for before it signs the message:

- **1000**: Fast Transfer (confirmed, not yet finalized)
- **2000**: Standard Transfer (finalized)

Only these two thresholds exist. Per Circle's
[technical guide](https://developers.circle.com/cctp/references/technical-guide),
any value below 1000 is treated as 1000 and any value above 1000 is treated as
2000. A value like 1500 does not stay at 1500, it rounds up to a finalized
transfer.

For burns sourced **from Arc Testnet**, use **2000**. Circle's supported chains
table lists Arc testnet as a standard transfer source and marks fast transfer
`N/A`. The attestation service acts on that. Messages sourced from domain 26
that were submitted with `minFinalityThreshold: 1000` come back from the Iris
API with `finalityThresholdExecuted: 2000` and a `complete` status, so the
transfer is finalized either way. Ask for 2000 and the request matches the
outcome. Your polling code then reads one threshold and you are not building a
Fast Transfer path with its own fee on a route that cannot serve one. The other
testnets in a typical setup (Ethereum Sepolia, Base Sepolia, Avalanche Fuji) do
attest at 1000.

```ts
// Burn sourced from Arc Testnet
const minFinalityThreshold = 2000;
```

Because an Arc burn executes at 2000, MessageTransmitterV2 on the destination
routes it to `handleReceiveFinalizedMessage`, not
`handleReceiveUnfinalizedMessage`. A contract that receives Arc-sourced
transfers needs the finalized handler.

## maxFee and route fees

`maxFee` is denominated in `burnToken` units, so on Arc it is 6-decimal USDC.
The source TokenMessenger enforces a floor of
`amount * minFee / MIN_FEE_MULTIPLIER` and reverts the burn under it. It also
reverts if `maxFee` is greater than or equal to `amount`. Read the floor off the
contract instead of guessing:

```bash
cast call 0x8FE6B999Dc680CcFDD5Bf7EB0974218be2542DAA \
  "getMinFeeAmount(uint256)(uint256)" 1000000 --rpc-url <arc-testnet-rpc>
# -> 0   (minFee is 0 on Arc Testnet, so maxFee: 0 passes for a burn from Arc)
```

Fast Transfers can carry a route fee, which the Iris fees endpoint returns per
finality threshold in basis points (1 = 0.01%):

```bash
# fees for a burn from Ethereum (0) to Arc (26)
curl https://iris-api-sandbox.circle.com/v2/burn/USDC/fees/0/26
# -> [{"finalityThreshold":1000,"minimumFee":1},{"finalityThreshold":2000,"minimumFee":0}]
```

Convert the rate before you pass it. One basis point on 10 USDC is
`10_000_000n * 1n / 10_000n`, so `maxFee = 1_000n`. The 2000 rows on the Arc
routes are 0 today, which is consistent with Arc's finalized-only attestation.

## Estimating gas for CCTP calls

`eth_estimateGas` is unreliable for CCTP writes on Arc Testnet. It returns an
internal error with `data: null` for `depositForBurn`
([#108](https://github.com/circlefin/arc-node/issues/108)) and fails across USDC
and CCTP write calls more generally
([#80](https://github.com/circlefin/arc-node/issues/80)), including the preceding
ERC-20 `approve`. Clients that estimate gas automatically (ethers.js, viem,
wagmi) will throw before the transaction is ever submitted.

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
refunded. Do the same for `receiveMessage` when Arc is the destination chain.

The mint on other destination chains has a different gas problem. A
`maxFeePerGas` estimated while you were still waiting on the attestation can be
under the base fee by the time the transaction lands, which fails with
`max fee per gas less than block base fee`
([#153](https://github.com/circlefin/arc-node/issues/153)). Re-estimate the fee
immediately before you send. Padding the estimate works too.

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
destination chain's MessageTransmitterV2. The response also carries
`finalityThresholdExecuted`, the level Iris actually attested at.

Two responses that look like failures are not. Until Iris has indexed the burn
the endpoint answers 404 with
`{"error":"Message not found for provided parameters"}`. A message still waiting
on confirmations comes back with a pending status. Keep polling through both.
The attestation service allows 35 requests per second and blocks all requests
for five minutes after a 429, so poll no faster than every 5 seconds and back
off.

If a message has not completed after about 20 minutes, stop polling and look at
the burn. Confirm it succeeded on Arc
(`https://testnet.arcscan.app/tx/<burnTxHash>`) and that the domain in the URL
is the source domain of the burn, not the destination. Circle's
[resolve attestation issues](https://developers.circle.com/cctp/howtos/resolve-stuck-attestation)
guide covers the rest.

## References

- [CCTP supported chains and domains](https://developers.circle.com/cctp/concepts/supported-chains-and-domains)
- [CCTP contract addresses](https://developers.circle.com/cctp/references/contract-addresses)
- [CCTP contract interfaces](https://developers.circle.com/cctp/references/contract-interfaces)
- [CCTP technical guide](https://developers.circle.com/cctp/references/technical-guide)
- [CCTP fees](https://developers.circle.com/cctp/concepts/fees)
- [Resolve attestation issues](https://developers.circle.com/cctp/howtos/resolve-stuck-attestation)
- [USDC contract addresses](https://developers.circle.com/stablecoins/usdc-contract-addresses)
