# Receivables Financing Pattern

This note outlines a reference pattern for invoice factoring and purchase-order
financing flows on Arc. It is intended as a design checklist for application
developers, not as a complete production contract.

Receivables financing has three common roles:

- `seller`: creates the invoice or purchase order and receives an advance.
- `financier`: funds the advance and takes repayment risk for a fee.
- `debtor`: pays the full invoice amount when the receivable is due.

## Lifecycle

A simple non-recourse factoring flow can use the following lifecycle:

```text
Created -> Funded -> Delivered -> Repaid
                            \-> Defaulted
Created -> Cancelled
```

- `Created`: the seller records the debtor, settlement token, face value,
  advance terms, fee terms, due date, and an optional off-chain description or
  document hash.
- `Funded`: the financier advances funds to the seller. After this point, the
  seller should not be able to cancel or rewrite economic terms.
- `Delivered`: the seller posts a proof hash or delivery reference. Avoid
  storing invoices, bills of lading, or customer data directly on-chain.
- `Repaid`: the debtor pays the face value. The contract splits repayment
  between the financier and seller according to the terms fixed at creation.
- `Defaulted`: anyone can mark an unpaid receivable as defaulted after the due
  date. In a non-recourse design, this records status only and does not recover
  funds from the seller.
- `Cancelled`: the seller can cancel only before the receivable is funded.

## Implementation Checklist

Keep the economic terms immutable after creation:

- snapshot the settlement token, face value, advance amount or basis points, fee
  amount or basis points, due date, and role addresses per receivable;
- reject zero addresses and clearly define whether roles may overlap;
- bound basis-point values and ensure the final repayment split cannot exceed
  the receivable face value;
- avoid reading mutable global fee rates during repayment.

Guard the state transitions:

- allow funding only from `Created`;
- allow cancellation only before funding;
- allow delivery proof submission only by the seller after funding;
- allow repayment only before or after delivery if that matches the product
  rules, but document the choice explicitly;
- allow default only after the due date and only while the receivable is unpaid;
- emit events for creation, funding, proof submission, repayment, default, and
  cancellation.

Handle token transfers defensively:

- update contract state before making external token transfers;
- during funding, record the financier, advance amount, and `Funded` status
  before pulling or forwarding tokens so callbacks cannot re-enter while the
  receivable still appears to be `Created`;
- use safe ERC-20 transfer helpers and account for non-standard return values;
- keep all settlement math in the token's native decimals;
- test rounding behavior for small face values;
- reject terms that would make the seller or financier payout negative or exceed
  the incoming repayment amount.

## Arc-Specific Notes

Arc's EVM compatibility makes the pattern deployable with standard Solidity
tooling. Because Arc uses USDC as gas, wallets participating in the flow should
reserve enough balance for both application transfers and network fees.

If a product accepts cross-chain deposits before funding a receivable, keep the
bridge or Gateway settlement step separate from the receivable state machine.
Only mark a receivable as funded once the expected token balance is available
on Arc and the contract can transfer it atomically.

## Tests to Include

At minimum, cover these cases:

- funding, repayment, default, and cancellation happy paths;
- attempts to mutate fees, token, due date, or parties after creation;
- repayment split rounding for low-value invoices, including the smallest
  allowed face value, and an invariant that the financier payout never exceeds
  the face value;
- default exactly at and just before the due date boundary;
- cancellation after funding;
- repayment after default;
- duplicate repayment, duplicate funding, and duplicate proof submission;
- token transfer failure or short-transfer behavior;
- proof hash privacy: no raw invoice data is required on-chain.
