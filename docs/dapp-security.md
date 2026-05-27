# DApp Security Notes

Arc is EVM-compatible, so the usual browser and wallet security rules still
apply to DApps built on top of it. This page collects small implementation
notes for application developers.

For vulnerabilities in Arc itself, use the private reporting process in
[SECURITY.md](../SECURITY.md).

## Render On-chain SVG Metadata Safely

Some ERC-721 contracts return metadata from `tokenURI()` with an inline SVG
image, commonly as a `data:image/svg+xml;base64,...` URI. Treat that SVG as
untrusted input. Any wallet or contract that can influence the metadata can
also influence the SVG contents.

Do not decode the SVG and inject it into the page:

```tsx
// Unsafe: the decoded SVG becomes trusted page markup.
<div dangerouslySetInnerHTML={{ __html: atob(metadata.image.split(",")[1]) }} />
```

Render the data URI as an image instead:

```tsx
// React / Next.js
<img src={metadata.image} alt={metadata.name ?? "Token image"} />
```

```js
// Vanilla JavaScript
const img = document.createElement("img");
img.src = metadata.image;
img.alt = metadata.name || "Token image";
container.replaceChildren(img);
```

When SVG is loaded through an image element, the browser treats it as image
content instead of executing it as part of the parent document. Avoid
`innerHTML`, `dangerouslySetInnerHTML`, `DOMParser` followed by DOM insertion,
or any equivalent path that turns untrusted SVG into live page markup.

If a DApp needs interactive SVG content, render it in a sandboxed `iframe`
instead of the main document:

```html
<iframe sandbox srcdoc="<!-- sanitized SVG document goes here -->"></iframe>
```

Only add sandbox permissions that the feature truly needs, and avoid combining
`allow-scripts` with `allow-same-origin` for untrusted content.

## Payment Escrow Contract Checklist

Stablecoin escrow contracts are common on Arc because users can pay fees and
application balances in USDC. If your DApp holds user funds between deposit and
release, make the release rules explicit in storage and keep them stable after
the escrow is funded.

### Cap release delays

Do not let users set an unbounded release delay. In Solidity 0.8+, an expression
like `fundedAt + releaseDelay` reverts on overflow. A maliciously large
`releaseDelay` can therefore make every release attempt revert.

```solidity
uint256 public constant MAX_RELEASE_DELAY = 90 days;

function fundEscrow(uint256 id, uint256 releaseDelay) external {
    require(releaseDelay <= MAX_RELEASE_DELAY, "Delay too large");
    // Store escrow terms.
}
```

When checking whether an escrow is releasable, prefer a bounded delay and keep
the arithmetic path easy to audit:

```solidity
require(block.timestamp - escrow.fundedAt >= escrow.releaseDelay, "Too early");
```

### Give the payee a recovery path

If only the payer can release funds before a timer expires, an unresponsive
payer can strand delivered work. Decide up front which states allow the payee to
release funds, and encode that rule directly in the contract:

```solidity
bool canRelease = msg.sender == escrow.payer
    || msg.sender == escrow.payee
    || block.timestamp - escrow.fundedAt >= escrow.releaseDelay;

require(canRelease, "Not authorized");
```

For dispute-based flows, replace the unconditional payee path with an explicit
state transition, arbitrator decision, or timeout. The important part is that
the payee should not depend on the payer staying online forever.

### Snapshot mutable fee terms

If a protocol fee can change after an escrow is funded, calculate the release
fee from the rate that was active at funding time:

```solidity
struct Escrow {
    uint256 amount;
    uint16 feeBpsAtFund;
    // Other terms...
}

function fundEscrow(uint256 id, uint256 amount) external {
    escrows[id].amount = amount;
    escrows[id].feeBpsAtFund = uint16(feeBps);
}

function releaseEscrow(uint256 id) external {
    Escrow storage escrow = escrows[id];
    uint256 fee = (escrow.amount * escrow.feeBpsAtFund) / 10_000;
    // Release net amount.
}
```

This keeps the payee's economics from changing silently between deposit and
release.

### Use two-step ownership transfers

For admin-controlled escrow contracts, prefer two-step ownership transfer
patterns such as OpenZeppelin `Ownable2Step`. A single typo in a one-step
`transferOwnership()` call can permanently lock upgrade, pause, fee, or recovery
controls.

```solidity
import "@openzeppelin/contracts/access/Ownable2Step.sol";

contract PaymentEscrow is Ownable2Step {
    // ...
}
```
