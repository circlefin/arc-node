# ADR-0005: EIP-7702 Authorization List Recovery Bound

| Field         | Value                |
|---------------|----------------------|
| Status        | Draft                |
| Author(s)     | @stelios-daveas      |
| Created       | 2026-07-06           |
| Updated       | 2026-07-06           |
| Supersedes    | -                    |
| Superseded by | -                    |

## Context

Arc's txpool validator (`ArcTransactionValidator`) recovers the authority address of every EIP-7702 authorization tuple in a submitted transaction (`recover_authority()`, in `check_for_denylisted_addresses`) so it can check each one against the address denylist. This recovery is ECDSA work, and it runs during mempool admission, before the sender's balance or fee is checked.

Internal testing found that recovery cost scales with the number of authorizations in a transaction, with no upper bound of its own: `max_tx_input_bytes` constrains authorization count only indirectly, through an encoding-size assumption, and in practice let through far more authorizations than intended. Because recovery runs ahead of balance and fee checks, a transaction with a large authorization list could consume a disproportionate amount of validator CPU relative to the cost of submitting it.

## Decision

Add a direct, stateless cap on the authorization list's length:

```rust
pub const MAX_AUTHORIZATIONS_PER_TX: usize = 100;
```

Checked immediately after Reth's own stateless validation and before any state access — ahead of blocklist/denylist SLOAD reads and, critically, ahead of authority recovery. Applied unconditionally, independent of whether the chain has a denylist. The rejection is classified `is_bad_transaction() = false` (`ArcTransactionValidatorError::TooManyAuthorizations`), since the limit is an Arc-local policy choice, not a cross-client protocol invariant — peers running other implementations may accept longer lists, so this must not affect peer scoring.

100 was chosen to comfortably exceed realistic EIP-7702 usage (typically 1-3 delegations per transaction) while bounding worst-case per-transaction recovery cost to a small, fixed amount, regardless of encoding size, gas price, or account funding.

## Consequences

### Positive

- Recovery cost per submitted transaction is bounded by a small constant, independent of transaction size, gas price, or sender funding — no economic modeling required.
- No new configuration surface; the cap is a hardcoded, always-on guard.

### Negative

- `100` is a heuristic sized against typical usage rather than derived from a formal RPC-ingress-rate or CPU budget.
- Balance and fee checks still run after denylist authority recovery; tightening that ordering further is a candidate for future follow-up.

### Neutral

- A legitimate transaction needing more than 100 delegations in a single transaction (no known Arc use case today) would be rejected outright.

## Alternatives Considered

- **Reorder balance/fee checks entirely ahead of denylist authority recovery.** A larger pipeline restructuring than this fix required; a candidate for future follow-up.
- **Per-sender or per-IP rate limiting at the RPC layer.** An infrastructure-level change outside the txpool validator's scope.
- **Require a minimum fee or stake before performing recovery.** Rejected — duplicates fee-cap logic that already exists later in the validation pipeline and complicates the stateless/stateful split.
