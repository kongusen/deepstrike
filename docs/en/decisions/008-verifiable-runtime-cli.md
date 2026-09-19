# ADR-008: Verifiable Runtime host command surface

- Status: Superseded by [ADR-009](./009-framework-verifiable-runtime.md)
- Date: 2026-09-19
- Version: 0.2.69

## Context

0.2.68 completed Runtime Language convergence and froze the distinction between canonical meaning,
wire data, internal projections, durable state, evidence, and mirrors. The repository already has
a chain validator and replay primitives, but operators need one command surface to inspect an
operation and distinguish a proven contradiction from incomplete evidence. 0.2.69 is a
stabilization release, so changing Kernel inputs or adding a durable authority would add the wrong
risk.

## Decision

Add a host-side `deepstrike` CLI with `inspect`, `verify`, `replay`, and `fork` commands. The CLI
loads the existing Journal, SessionLog, and Checkpoint planes, filters by operation, and emits the
versioned `verifiable-report/v1` report. `ds-chain-validator` remains the compatibility entry point
and continues to own the low-level C1–C8 verdict logic.

`replay` is offline and cannot call a provider. `fork` creates a read-only host manifest containing
the verified parent operation, boundary, and source digest. It does not append a Kernel event,
alter a Checkpoint, or become recovery authority.

## Consequences

- Operators get one discoverable narrative without duplicating validator semantics.
- The report schema and exit codes become a small host ABI for one minor release.
- Missing evidence remains explicit and may produce exit 2.
- Evolution proposals and content-addressed artifact lineage remain 0.2.70+ work.

## Alternatives rejected

- Replacing `ds-chain-validator` would break existing automation without semantic gain.
- Adding a Kernel `Fork` input would expand the ABI during its freeze window.
- Replaying through live providers would make verification nondeterministic and permit side effects.
