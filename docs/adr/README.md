# Architecture Decision Records

Architecture Decision Records (ADRs) capture significant architectural and design decisions along with their context and consequences.

## When to Write an ADR

Write an ADR when making:

- **Architectural decisions** - Core design patterns, system boundaries, major component interactions
- **Significant features** - Major new capabilities, new precompile types, transaction pool changes, new APIs

If a decision affects multiple components or has lasting implications, it likely warrants an ADR.

## How to Create an ADR

1. Copy `TEMPLATE.md` to `NNNN-title-slug.md` (use the next available number)
2. Fill in the template sections
3. Set status to `Draft`
4. Open a pull request
5. Discuss in PR comments until consensus emerges
6. Update status to `Proposed` when ready for final review
7. Update status to `Accepted` or `Rejected` based on outcome
8. Merge the PR
9. Update the index table below

## States

| State | Description |
|-------|-------------|
| Draft | Initial proposal, open for early feedback |
| Proposed | Ready for review |
| Accepted | Approved and should be followed |
| Rejected | Declined with documented reasoning |
| Superseded | Replaced by a newer ADR |

## Index

| ADR | Title | Status | Date       |
|-----|-------|--------|------------|
| [0001](0001-adr-process.md) | ADR Process | Proposed | 2025-12-11 |
| [0002](0002-block-dissemination-protocol.md) | Block Dissemination Protocol | Draft | 2026-01-13 |
| [0003](0003-governance-configuration-and-validation.md) | Dynamic Block Gas Limit Configuration Validation | Draft | 2026-02-20 |
| [0004](0004-base-fee-validation.md) | Base Fee Parameter Validation | Draft | 2026-03-03 |
