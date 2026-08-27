# ADR-0006: Proposer Timestamp Validation

| Field         | Value         |
| ------------- | ------------- |
| Status        | Draft         |
| Author(s)     | @tomasz-kulik |
| Created       | 2026-07-01    |
| Updated       | 2026-08-25    |
| Supersedes    | -             |
| Superseded by | -             |

## Context

Every Arc block header carries a timestamp. Consensus can only finalize a valid block and a valid block must carry a
valid timestamp. The block timestamp feeds EVM opcodes (`TIMESTAMP`), base-fee computation, and anything downstream that
relies on block time being monotonically non-decreasing. The question this ADR settles is _how a validator decides
whether a proposed block's timestamp is acceptable_, and why Arc makes that decision the way it does.

### Lineage: BFT Time vs PBTS

CometBFT, the main Tendermint implementation in Go, historically computed block timestamps using
[**BFT Time**](https://github.com/cometbft/cometbft/blob/main/spec/consensus/bft-time.md) — i.e., by computing the
median of the timestamps carried by the precommit votes used to finalize the previous block. CometBFT later replaced it
with
[**Proposer-Based Timestamps (PBTS)**](https://github.com/cometbft/cometbft/blob/main/spec/consensus/proposer-based-timestamp/README.md),
where the proposer assigns the timestamp of a block from its local time and each validator accepts it only if it is
_timely_ relative to the validator's own clock. PBTS's `timely` predicate is two-sided: a proposal is timely if it is
neither too far in the past nor too far in the future relative to when the validator received it, parameterized by two
synchronous parameters: `PRECISION` and `MSGDELAY`.

## Decision

Arc cannot adopt BFT Time because Precommit votes do not, and should not carry timestamps. Consensus today signs votes
with **Ed25519**, one signature per validator, and a commit certificate carries a `Vec` of those per-vote signatures
(`type SigningScheme = Ed25519` in `crates/types/src/context.rs`; `commit_signatures` in the certificate). Under that
scheme BFT Time is technically possible, but the vote path is deliberately architected for a **planned BLS
signature-aggregation fork** (see the currently-disabled `bls_commit_certificate` scaffolding in
`crates/types/src/spec.rs`). BLS aggregation requires every validator to sign the _same_ message, and a per-vote
timestamp would make each precommit a distinct message and defeat aggregation. So a weighted-median-of-vote-timestamps approach
is incompatible with the direction Arc's vote path is headed, which pushes Arc toward a proposer-assigned timestamp
validated against local clocks — the PBTS shape, not the BFT Time shape.

As a result, the decision is to adopt a proposer-assigned block timestamp validated against each node's local clock, keeping only the **"too far in the
future"** half of PBTS's `timely` predicate with a fixed 30-second skew threshold. Nodes whose clocks drift beyond the
threshold are treated as byzantine rather than accommodated.

The clock-skew judgment is a **vote-time-only** signal, evaluated in the consensus layer rather than in execution-layer
block admission. Because it reads the local wall clock it is non-deterministic across nodes, so it may only gate a node's
**prevote** — it must never become a block's persisted validity. A too-far-future proposal is prevoted nil, but the block
is still stored with its execution-only validity, so a value that nonetheless gathers a 2f+1 commit certificate is adopted
via sync with no permanent stall and no restart.

Concretely:

1. The proposer timestamps `max(parent_block.timestamp, now())` from its local clock.
2. A validator **prevotes nil** on a block iff `header.timestamp > local_time + 30s`. The block's persisted validity
   (the execution verdict) is unaffected; only the vote is downgraded.
3. No lower-bound timeliness check is performed.
4. Clock synchronization (NTP) is an operational requirement for validators. A validator without an NTP-synchronized clock is considered byzantine.

### Where the timestamp comes from

The proposer timestamps a block from its own local clock, clamped to be non-decreasing relative to the parent:

```
timestamp = max(parent_block.timestamp, now())
```

This guarantees monotonicity even if a proposer's clock briefly runs behind the parent block's time.

### How a validator checks it

A validating node compares the proposed timestamp against _its own_ local clock when it assembles a received proposal,
and **prevotes nil** if the timestamp is too far in the future. The threshold and predicate live in the consensus layer
(`crates/malachite-app/src/handlers/skew_gate.rs`); the clock-skew judgment is a consensus concern, so the execution
layer no longer references it. The consensus layer applies it when building the `ProposedValue` to vote on, without
touching the block's stored (execution-only) validity:

```rust
// crates/malachite-app/src/handlers/skew_gate.rs
const ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS: u64 = 30;

fn header_timestamp_exceeds_skew(header_timestamp: u64, local_time: u64) -> bool {
    header_timestamp > local_time.saturating_add(ARC_PROPOSER_CLOCK_SKEW_THRESHOLD_SECS)
}

// received_proposal_part.rs / started_round.rs
// An execution-Valid block whose timestamp exceeds the skew bound is prevoted
// Invalid (nil); the block is still stored with its execution-only validity.
```

Only the **upper** bound is enforced: a proposal may not be more than 30 seconds ahead of the validator's local clock.
There is no lower-bound ("too far in the past") check, because monotonicity already constrains that direction and a
stale timestamp carries no comparable risk. Arc also targets sub-second block times (~0.5s), so an honest proposer's
`now()` is never meaningfully behind the parent; a "too far in the past" bound would guard against a situation that does
not arise in practice.

### Simplifications relative to PBTS

| PBTS                                                      | Arc                                            |
| --------------------------------------------------------- | ---------------------------------------------- |
| Two-sided `timely` predicate (past **and** future bounds) | Future bound only (`timestamp <= local + 30s`) |
| `PRECISION` + `MSGDELAY` parameters                       | Single 30s threshold                           |
| Tolerates bounded clock error as a first-class parameter  | Treats out-of-sync nodes as byzantine          |

The _past_ bound is dropped because block timestamps are already non-decreasing by construction, so a proposal cannot
rewind time, and an unusually old (but monotonic) timestamp does not threaten safety or the fee/opcode semantics that
depend on block time.

### Operational requirement

Because validators judge timestamps against their own clocks, Arc assumes node operators keep clocks synchronized (e.g.
via NTP). A node whose clock drifts beyond the skew threshold is treated as byzantine for the purpose of timestamp
validation: it will disagree with correctly-synced peers and either reject valid proposals or accept invalid ones. This
is a deliberate trade — the protocol does not attempt to tolerate arbitrary clock drift; it requires operators to
prevent it.

## Consequences

### Positive

- The proposer-timestamp design and its PBTS lineage are documented in-repo, discoverable and reviewable alongside the
  consensus code that implements it.
- The clock-skew boundary case has an explicit, bounded impact analysis, so future readers do not need to rediscover why
  it is tolerable.
- The upper-bound-only check is simple and stateless: a single comparison against the local clock, no PBTS parameters to
  tune.
- Dropping the _past_ bound greatly simplifies the algorithm's adoption by Arc: the Tendermint algorithm (and thus Malachite Core) does not need to be modified, as it was for PBTS.

### Negative

- Correctness depends on operators keeping clocks synchronized; a widely drifted validator set degrades liveness.
- The fixed 30-second threshold is a network-wide constant rather than a tunable parameter; changing it requires a code
  change.

### Neutral

- Dropping the lower-bound timeliness check diverges from CometBFT PBTS; the divergence is intentional and rests on
  Arc's monotonic-timestamp construction.
- Treating out-of-sync nodes as byzantine shifts clock management from the protocol to node operations.
- Only a malicious or compromised proposer can boundary-time a proposal to waste its own turn. A correctly-configured
  proposer never stamps a timestamp 30s in the future, so this is not a cost the design imposes on honest operation, and
  the wasted slot is no worse than any other way a proposer can forfeit its turn (e.g. being briefly down).
- A byzantine proposer can still set the next block's timestamp up to 30 seconds in the future. This is also possible in PBTS. It is considered an acceptable risk.

## Alternatives Considered

**BFT Time (median of vote timestamps).** Rejected: incompatible with the planned BLS vote aggregation (see
[Decision](#decision) above), which requires all validators to sign an identical message. Per-vote
timestamps would defeat aggregation.

**Full two-sided PBTS `timely` predicate.** Rejected as unnecessary: block timestamps are already non-decreasing, so the
"too far in the past" bound guards against a threat monotonicity already prevents. Keeping only the future bound reduces
the check to a single comparison.

**Tolerating clock skew as a tuned parameter (PBTS `PRECISION`/`MSGDELAY`).** Rejected in favor of a single fixed
threshold plus an explicit operational NTP requirement. Treating drift as an operational failure is simpler than
modeling it in the protocol and matches Arc's assumption of professionally-operated validators.

**Making the skew threshold a consensus parameter.** Rejected in favor of a compile-time constant. The threshold is
consensus-critical — every validator must apply the same value — so exposing it as a tunable parameter would mean
versioning and coordinating changes across the validator set (a governance/hardfork concern) for a value not expected to
need tuning. A fixed constant keeps the check trivial and uniform across the network. Promoting it to a
hardfork-gated or governance-controlled parameter remains open if operational experience later shows the 30-second bound
must vary.

**Gating the precommit/commit step on timeliness.** Rejected: making an already-polka'd value's precommit or final
decision conditional on a fresh timeliness check would render a decided value retroactively rejectable on timing
grounds, converting the bounded liveness cost into a potential safety hazard. Keeping timeliness a prevote-time gate —
combined with the upper-bound-only check, so the re-validation that does run at each round start can never reject an
already-timely value — is what keeps the blast radius contained.

## Appendix: Security Considerations

A security review surfaced an edge case the original design did not spell out. Because each honest validator checks the
proposal against its _own_ clock, two honest validators with slightly different clocks can reach **opposite** verdicts
on the same block when its timestamp sits right at the 30-second boundary.

### Mechanism

Suppose a proposer chooses a timestamp `T`. For a validator with local time `L`, the block is accepted iff
`T <= L + 30`. Two honest validators `A` and `B` with `L_A < L_B` (A's clock lags B's) can straddle the boundary:

- `T <= L_B + 30` → **B accepts** (prevotes the block).
- `T > L_A + 30` → **A rejects** (prevotes nil).

An honest proposer stamps `T ≈ now()`, comfortably inside 30s for every correctly-synced validator, so this split does
not arise in normal operation. Only a **malicious or compromised proposer** can deliberately pick a `T` near the
boundary to maximize the set of validators that reject while others accept.

### Impact and classification

This is a **liveness** concern, not a **safety** one:

- **Worst case:** the proposer engineers a split that leaves more than 1/3 of voting power prevoting nil. No value
  reaches a polka in that round, the round fails, and the next proposer (by round-robin) drives a new round to a
  decision. The net effect is one proposer's turn being wasted — comparable to a validator being briefly down for
  maintenance (roughly 1-in-16 heights for a single faulty proposer in the expected validator set), which the protocol
  already tolerates.
- **Safety is never at risk.** Two honest validators can never _commit_ different values as a result of a timestamp
  split, because timeliness gates only whether a value can be prevoted toward a polka. It does not gate the final
  decision.

### Why the blast radius is bounded

The timeliness check runs on the consensus paths that build a node's **prevote** — both the live-arrival path
(`crates/malachite-app/src/handlers/received_proposal_part.rs`) and the buffered/early-arrival path at round start
(`crates/malachite-app/src/handlers/started_round.rs`, for parts that arrived before their round began) — where it
downgrades **only the prevote**. It is not consulted at the precommit or commit step, it never touches a block's
persisted validity, and it does not run on the value-sync adoption path (which validates execution-only). Two properties
keep it from ever retroactively rejecting a value:

1. An untimely proposal can fail to polka in the proposer's own round, but it cannot stall the height beyond that round:
   the next proposer is free to propose a timely value and carry the height to a decision.
2. The clock verdict never becomes a block's persisted validity: a too-far-future block is stored execution-`Valid`, so a
   value that gathers a commit certificate is adopted via sync regardless of any node's clock. A prevote-time nil vote is
   the only effect, so no already-decided value is retroactively invalidated and no node stalls waiting on its clock.

Keeping the clock verdict out of persisted validity (rather than relying only on the upper-bound property to make
re-validation safe) is what lets Arc adopt this without changes to the Tendermint algorithm.
In short: a malicious proposer can burn its own turn, and no more.

### Validation timing, sync, and restart

`local_time` is the validating node's wall clock (`SystemTime::now()`), read afresh each time a proposal's prevote is
built — whether the parts arrive live or were buffered and re-offered at round start — not the block's arrival time nor
a value cached from an earlier check. The clock verdict gates only that node's prevote; it is never written to the
block's persisted validity, and because the bound is upper-only it can only relax as the wall clock advances, so a value
that was timely when first seen stays timely on every later prevote — provided the local clock does not step backwards.
If it does (e.g. an NTP correction across a restart), the node may prevote nil on a block it previously accepted; the
block is still adopted once it carries a commit certificate, because the verdict is never persisted (point 2 above).

Blocks obtained through value sync are already-decided blocks pulled from a peer. The value-sync adoption path applies
execution validation only — no clock check — so a decided block is adopted regardless of how far a node's local clock
lags its timestamp. Round-start re-validation (including after a restart) does apply the clock check when it rebuilds a
node's prevote, but only to that vote: it never touches persisted validity, so it cannot block adoption of a value that
reaches a commit certificate. It does mean the node cannot decide on that certificate directly — adoption goes around
through sync — so a persistently skewed validator degrades to a sync-follower rather than stalling, at one sync
round-trip per height. Previously the check ran at execution-layer admission (`engine_newPayload`), where a lagging
clock produced a hard `Invalid` that
Reth cached in its in-memory invalid-headers table and that was persisted as the block's validity — permanently stalling
the node at that height until a restart cleared the cache. Moving the check to a vote-time-only signal removes that
stall: no decided block is unsyncable on timing grounds, and the invalid-headers cache is never poisoned.
