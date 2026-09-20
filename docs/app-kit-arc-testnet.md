# App Kit on Arc Testnet

This note answers the Arc Testnet integration questions raised in #88 for
Circle [App Kit](https://docs.arc.io/app-kit) (`@circle-fin/app-kit`): which
methods work on Arc, which chain identifier to pass, and how `kit.send()`
behaves.

Authoritative capability and chain tables live in the Circle docs. Prefer those
pages when versions drift; this file is an Arc-node-local pointer for builders
who start from this repository.

## Supported methods on Arc Testnet

Official App Kit docs list **Arc (testnet only)** with:

| Capability | Arc Testnet |
| --- | --- |
| Send | Yes |
| Bridge | Yes |
| Swap | Yes (USDC, EURC, and cirBTC only) |
| Unified Balance | Yes |

Adapters listed for Arc Testnet: Viem, Ethers, and Circle Wallets.

Source: [Supported blockchains and tokens](https://docs.arc.io/app-kit/references/supported-blockchains).

## Chain identifier for `kit.bridge()` / `kit.send()` / `kit.swap()`

Use the case-sensitive string **`Arc_Testnet`** (or `BridgeChain.Arc_Testnet`
from `@circle-fin/app-kit`). Spaces in the human name become underscores.

```ts
import { AppKit, BridgeChain } from '@circle-fin/app-kit'

const kit = new AppKit()
const chain = BridgeChain.Arc_Testnet // or "Arc_Testnet"
```

You can also discover support at runtime:

```ts
kit.getSupportedChains()
kit.getSupportedChains('bridge')
kit.getSupportedChains('swap')
```

Runtime discovery answers **API capability**, not whether a specific token
direction has liquidity right now. Treat `INPUT_UNSUPPORTED_ROUTE (331001)` and
`ONCHAIN_SIMULATION_FAILED` as normal route-unavailable outcomes: surface them
to the caller and never replace a failed executable quote with a market-rate
estimate. Prefer fail-closed behavior when no executable quote exists.

## Minimal examples

### Bridge USDC onto Arc Testnet

```ts
const result = await kit.bridge({
  from: { adapter: viemAdapter, chain: 'Ethereum_Sepolia' },
  to: { adapter: viemAdapter, chain: 'Arc_Testnet' },
  amount: '1.00',
})
```

Bridge transfers USDC via CCTP. See the
[App Kit overview](https://docs.arc.io/app-kit) and
[SDK reference](https://docs.arc.io/app-kit/references/sdk-reference).

### Send USDC on Arc Testnet

```ts
const result = await kit.send({
  from: { adapter: viemAdapter, chain: 'Arc_Testnet' },
  to: '0xRecipient',
  amount: '1.00',
  token: 'USDC',
})
```

`kit.send()` moves tokens between wallets **on the same chain**. On Arc that is
an on-chain transfer through the connected adapter (browser wallet or Circle
Wallets), not a Circle off-chain ledger move. Quickstart:
[Send tokens on the same blockchain](https://docs.arc.io/app-kit/quickstarts/send-tokens-same-chain).

### Swap on Arc Testnet

```ts
const result = await kit.swap({
  from: { adapter: viemAdapter, chain: 'Arc_Testnet' },
  tokenIn: 'USDC',
  tokenOut: 'EURC',
  amountIn: '1.00',
})
```

Among testnets, only Arc Testnet supports Swap, and only for USDC, EURC, and
cirBTC. See [Swap](https://docs.arc.io/app-kit/swap).

## Practical notes

- **Kit key.** A Circle Console kit key is optional but recommended for
  production volume; without one, requests use a shared rate limit.
- **Signing boundary.** Browser Viem / Ethers adapter paths keep signing in the
  connected wallet and do not need a backend private key. Circle Wallets
  server-side examples that use a developer-controlled key are a different
  custody model. A public API process should not hold a signer that can act
  against user-granted allowances.
- **Adapters.** Pass a connected Viem / Ethers / Circle Wallets adapter in
  `from` (and `to` when bridging).
- **Programmatic discovery.** Prefer `getSupportedChains(...)` over hard-coding
  when building chain pickers, so new networks do not require a docs republish.
  Pair discovery with the route-unavailable handling above; capability listing
  alone is not a guarantee that a quote will execute.
- **Historical gap.** Early App Kit releases left Arc Testnet bridge/send paths
  poorly documented in-repo; current Circle docs cover them. If a method fails
  at runtime, confirm package versions and that the chain string is exactly
  `Arc_Testnet`.

## Related Arc Testnet facts

- Chain ID: `5042002`
- Explorer: `https://testnet.arcscan.app`
- Public RPC: `https://rpc.testnet.arc.io` (see also
  [public testnet RPC notes](https://docs.arc.io/arc/references/rpc-endpoints))
