# ADR-0001: ADR Process

| Field         | Value                |
|---------------|----------------------|
| Status        | Draft                |
| Author(s)     | @christophercampbell |
| Created       | 2025-12-11           |
| Updated       | 2025-12-11           |
| Supersedes    | -                    |
| Superseded by | -                    |

## Context

As Arc grows in complexity, architectural decisions are made through informal discussions that leave no permanent record. This creates problems:

- New contributors lack context for why things are built a certain way
- Decisions get revisited without awareness of prior reasoning
- No audit trail exists for compliance or governance review
- Knowledge is lost when team members leave

We need a lightweight process to capture significant decisions without creating bureaucratic overhead.

## Decision

Adopt Architecture Decision Records (ADRs) with the following structure:

**Location:** `docs/adr/`

**Naming:** `NNNN-title-slug.md` using zero-padded sequential numbers.

**States:**
- Draft - Initial proposal, open for early feedback
- Proposed - Ready for review
- Accepted - Approved and should be followed
- Rejected - Declined with documented reasoning
- Superseded - Replaced by a newer ADR

**Workflow:**
1. Author creates a branch and copies `TEMPLATE.md` to a new numbered file
2. Author fills in the template and sets status to Draft
3. Author opens a pull request for team discussion
4. When ready for final review, author updates status to Proposed
5. Team reaches consensus through PR comments
6. Author updates status to Accepted or Rejected
7. PR merges and author updates the README index

**Scope:** ADRs are required for:
- Architectural decisions affecting core design patterns, system boundaries, or component interactions
- Significant features including new precompile types, transaction pool changes, or new APIs

## Consequences

### Positive

- Decisions are documented with context and rationale
- New contributors can understand the "why" behind the codebase
- Audit trail exists for compliance and governance
- Rejected alternatives are recorded, preventing repeated proposals

### Negative

- Adds overhead to the decision-making process
- Requires discipline to maintain the index and follow the process
- May slow down urgent decisions

### Neutral

- ADRs become part of the codebase history through git
- The process itself can evolve through future ADRs

## Alternatives Considered

**No formal process:** Lack of architectural documentation is the problem we're solving.

**Formal governance with required approvers:** Heavyweight process inappropriate for current team size and cadence. Can be revisited via a future ADR if needed.
