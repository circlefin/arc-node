# Building Payment Contracts on Arc — Security Guide

> **Advisory:** 4 vulnerabilities found in common PaymentEscrow patterns on Arc Network.
> Reference contract: [`contracts/src/common/PaymentEscrow.sol`](../contracts/src/common/PaymentEscrow.sol)

## Vulnerability 1 — HIGH: Arithmetic overflow on `fundedAt + releaseDelay`

### The bug

```solidity
// VULNERABLE — reverts for any releaseDelay near type(uint256).max
require(block.timestamp >= e.fundedAt + e.releaseDelay, "Too early");
```

A malicious payer sets `releaseDelay = type(uint256).max`. Since Solidity 0.8+ uses checked arithmetic, `fundedAt + releaseDelay` overflows and **permanently reverts**. The payee's funds are locked forever.

### The fix

Cap `releaseDelay` at a sane maximum (e.g. 90 days):

```solidity
uint256 public constant MAX_RELEASE_DELAY = 90 days;

function fundEscrow(..., uint256 releaseDelay) external {
    require(releaseDelay <= MAX_RELEASE_DELAY, "Delay too large");
    // ...
}
```

---

## Vulnerability 2 — HIGH: Payee has no on-chain recourse

### The bug

Only the payer (or an elapsed timer) can call `releaseEscrow()`. If the payer goes offline or refuses to release, the payee — who already delivered services — cannot recover funds even after the release window passes.

### The fix

Add payee as an unconditional release authority:

```solidity
function releaseEscrow(uint256 id) external {
    Escrow storage e = escrows[id];
    bool authorized = (
        msg.sender == e.payer ||
        msg.sender == e.payee ||          // payee always has release authority
        block.timestamp >= e.fundedAt + e.releaseDelay
    );
    require(authorized, "Not authorized");
    // ...
}
```

---

## Vulnerability 3 — MEDIUM: Fee rate computed at release time, not at fund time

### The bug

```solidity
// VULNERABLE — fee changes between fundEscrow and releaseEscrow
uint256 fee = (e.amount * feeBps) / 10_000;
```

If the owner calls `setFee()` between `fundEscrow` and `releaseEscrow`, the payee receives a different amount than expected with no on-chain signal.

### The fix

Snapshot the fee into the escrow struct at fund time:

```solidity
struct Escrow {
    // ...
    uint16 feeBpsAtFund;  // locked at fundEscrow time
}

function fundEscrow(...) external {
    escrows[id].feeBpsAtFund = uint16(feeBps);
}

function releaseEscrow(uint256 id) external {
    uint256 fee = (e.amount * e.feeBpsAtFund) / 10_000;  // uses snapshotted value
}
```

---

## Vulnerability 4 — LOW: Single-step ownership transfer permanently locks protocol

### The bug

Using OpenZeppelin's `Ownable` (single-step) means a typo in `transferOwnership(newOwner)` permanently and irrecoverably surrenders contract control — the new owner can never call `onlyOwner` functions if the address is wrong.

### The fix

Use `Ownable2Step` (propose + accept):

```solidity
import "@openzeppelin/contracts/access/Ownable2Step.sol";
contract PaymentEscrow is Ownable2Step { ... }
```

Now `transferOwnership(typoAddress)` can be corrected by calling `acceptOwnership()` — which only the intended recipient can do. If the address is wrong, ownership stays with the current owner.

---

## Reference: Audited PaymentEscrow.sol

A fully patched reference contract is at [`contracts/src/common/PaymentEscrow.sol`](../contracts/src/common/PaymentEscrow.sol) with all four fixes applied.

Key features:
- Maximum 90-day release delay (V1)
- Payee has unconditional release authority (V2)
- Fee snapshotted at fund time (V3)
- Ownable2Step ownership transfer (V4)
- SafeERC20 for all token transfers
- Custom errors (gas-efficient)
