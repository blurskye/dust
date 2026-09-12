# Documentation map

## Product authority

Start with [specs/README.md](specs/README.md). Its index, seven owning contracts, delivery classification, target WIT and coverage ledger replace the former numbered specs and overlapping plans. [Delivery](specs/delivery.md) distinguishes target requirements from qualified, partial, target-only and unproven implementation scope. The root [architecture pointer](../ARCHITECTURE.md) routes to the same ownership contract; it is not another specification.

## Current operational references

- [Linux user guide](user-guide-linux.md)
- [Current plugin SDK, ABI 0.2](plugin-sdk.md)
- [Current registry client](registry-client.md)
- [Linux release operations](release-linux.md)

These describe actual interfaces at the checkout being used, not independent target specifications. Source serializers, registered commands, defaults, `dust --api-manifest`, `dust --help` and the accepted production WIT remain the executable contracts. A guide names a commit only when it intentionally freezes that revision; the specification's formation baseline is not an operational-guide version.

Generated references must identify the source or generator from which they were reproduced. They describe the current executable and cannot introduce a setting, command, schema or capability that its source does not accept.

## Non-normative evidence

- [Immutable ownership and navigation baselines](history/README.md)
- [Competitive research](research/editor-comparisons.md)
- `progress/`: qualification receipts at their named revisions
- `performance/`: measured scenarios at their recorded revisions and environments

Receipts may support a classification in `specs/delivery.md`; history, research and performance evidence cannot change product requirements. Deleted competing plans/specifications remain in Git history, not active documentation. Historical failures remain useful evidence, but their old phase names, counts and next-step instructions are not current authority.
