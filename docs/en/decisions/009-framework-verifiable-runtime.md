# ADR-009: Make the Framework Verifiable Runtime the 0.2.69 foundation

- Status: Accepted
- Date: 2026-09-19
- Version: 0.2.69
- Supersedes: [ADR-008](./008-verifiable-runtime-cli.md)

## Context

0.2.69 initially described the feature as a `deepstrike` host command surface. That made the CLI
look like the product boundary even though the verification semantics already belong in framework
core. A path-oriented API also prevents the same operation from being used by a database adapter,
a browser store, or an SDK without reimplementing the checks.

## Decision

0.2.69 is the **Framework Verifiable Runtime Foundation**. Rust core exposes a storage-neutral
`EvidenceBundle`, a typed `VerifiableOperation`, `VerifyOptions`, `ReplayOptions`, and a verified
read-only `ForkPlan`. These APIs accept evidence bytes supplied by an adapter and perform no file
I/O, provider call, journal append, checkpoint mutation, or recovery action.

The framework owns the semantic operations:

```text
VerifiableOperation
  ├─ inspect(strict)
  ├─ verify(VerifyOptions)
  ├─ replay(ReplayOptions)
  └─ prepare_fork(at_step, strict)
```

The `deepstrike` binary is a thin filesystem and presentation adapter. Rust, Node, Python, and WASM
SDKs expose the same operation shape and delegate semantics to the core adapter. `verifiable-report/v1`
and `verifiable-fork/v1` remain serialization mirrors for process and language boundaries; they are
not a second authority. Native bindings expose one JSON bridge for this delegation; SDK adapters only
encode evidence bytes and decode the typed report.

The existing free functions remain as source-compatible wrappers during 0.2.69. New code must use
`VerifiableOperation` and keep storage access outside the framework module.

## Consequences

- One semantic implementation serves files, databases, object stores, browser memory, and tests.
- CLI behavior can evolve independently from the framework contract.
- SDKs can expose a callable framework object without inventing a second verifier.
- Forking remains a read-only plan until a later release defines an explicit orchestration authority.
- The report schema stays stable while the internal framework API can gain typed fields in a future
  minor release.

## Alternatives considered

### Keep the CLI as the primary API

Rejected. It couples verification to paths and makes every non-CLI host build another semantic
adapter.

### Duplicate validation in each SDK

Rejected. Duplicated C1–C8 logic would allow language-specific verdicts and violate the single
semantic authority boundary.
