# ADR-008: Verifiable Runtime host command surface

- Status: Superseded by [ADR-009](./009-framework-verifiable-runtime.md)
- Date: 2026-09-19
- Version: 0.2.69

## Context

0.2.68 completed the Runtime Language convergence and froze the distinction between canonical
meaning, wire data, internal projections, durable state, evidence, and mirrors. The repository
already has a chain validator and replay primitives, but operators still need separate low-level
knowledge to inspect an operation or distinguish a proven contradiction from incomplete evidence.
The 0.2.69 milestone is stabilization, so changing Kernel inputs or adding a new durable authority
would create the wrong kind of risk.

## Decision

Add a host-side `deepstrike` CLI with `inspect`, `verify`, `replay`, and `fork` commands. The CLI
loads the existing journal, SessionLog, and checkpoint planes, filters by operation, and emits the
versioned `verifiable-report/v1` report. `ds-chain-validator` remains as a compatibility entry
point and continues to own the low-level C1–C8 verdict logic.

`replay` is offline: it compares deterministic record digests and replay facts already present in
the evidence bundle and cannot call a provider. `fork` creates a read-only host manifest containing
the verified parent operation, boundary step, and source digests. It does not append a kernel
event, alter a checkpoint, or become recovery authority.

## Consequences

- Operators get one discoverable narrative without duplicating validator semantics.
- The report schema and exit codes become a small, stable host ABI for one minor release.
- Missing SessionLog, checkpoint, or replay evidence remains explicit and may produce exit 2.
- Evolution proposals and content-addressed artifact lineage remain deferred to 0.2.70+.

## Alternatives rejected

- Replacing `ds-chain-validator`: would break existing automation for no semantic gain.
- Adding a kernel `Fork` input: would expand the 0.2.69 ABI during its freeze window.
- Replaying through live providers: would make verification nondeterministic and could cause
  external side effects.
