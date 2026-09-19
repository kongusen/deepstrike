# ADR-010: Make 0.2.70 the Evolution Runtime hard cut

## Status

Accepted

## Date

2026-09-19

## Context

0.2.69 established a storage-neutral Framework Verifiable Runtime. It can inspect, verify, replay,
and prepare a read-only fork, but it deliberately has no artifact lineage, proposal, evaluation,
or promotion authority. The next version must make runtime evolution proveable without putting
artifact bytes, provider attempts, or host policy inside the kernel.

The existing 0.2.x compatibility window also leaves first-party aliases, report wrappers, and an
old validator entry point that would make two semantic contracts coexist. Evolution needs one
canonical object model and one fail-closed boundary.

## Decision

0.2.70 is the **Evolution Runtime hard cut**. It introduces the following canonical flow:

```text
ArtifactVersion → EvolutionProposal → EvaluationRun/EvaluationFact
→ PromotionDecision → ArtifactSet activation → Verifiable Operation
```

Artifacts are immutable content-addressed objects in a host-owned ArtifactStore. Evolution records
are append-only content-addressed records in a host-owned EvolutionLedger. The kernel stores only
the artifact-set digest and promotion-decision reference required to replay an operation. A new
artifact set takes effect at a new operation boundary.

The core owns canonical bytes, digest checks, evolution validation, activation binding, and
replay semantics. SDKs and CLI adapters load bytes, call the core contract, and present reports.

The release raises `KERNEL_ABI_VERSION` to 4 and rejects earlier journal, checkpoint, report, and
evolution formats. There is no version negotiation, shape-based format detection, compatibility
wrapper, or runtime migration path. Deployments that need old operations remain on 0.2.69.

## Alternatives considered

### Keep Evolution host-only with no kernel artifact binding

Rejected. The host could promote a version while the operation journal remained unable to prove
which artifact set produced its state. Replay would be incomplete at the exact boundary where
evolution matters.

### Put artifact bytes in the kernel journal or checkpoint

Rejected. It violates the large-object boundary, increases durable state, couples the kernel to
storage, and makes replay depend on payload transport rather than immutable references.

### Support hot replacement inside a running operation

Rejected for 0.2.70. It introduces a second activation boundary, complicates checkpoint safety,
and makes a candidate's causality ambiguous. Activation begins at a new operation; a later ADR may
define a checkpoint-boundary handoff.

### Keep the 0.2.69 compatibility layer

Rejected. Evolution requires one canonical proposal/evaluation/promotion model. Keeping old
wrappers would preserve two public meanings and allow unverified fallback paths.

### Rename the release to 0.3.0

Rejected for this roadmap. The project explicitly chooses 0.2.70 as the hard-cut release; the
semver exception is recorded here so it cannot be mistaken for an additive minor release.

## Consequences

- Existing 0.2.69 journals and checkpoints cannot be opened by 0.2.70.
- SDK consumers must adopt the new canonical object names and constructors in one migration.
- The validator gains E1–E8 evolution rules and new negative fixtures.
- Artifact storage remains replaceable because only the host adapter knows locators and bytes.
- Promotion can be independently audited from proposal, evaluation facts, policy digest, and
  operation replay evidence.
- The repository needs a coordinated Rust, Node, Python, WASM, CLI, fixture, and documentation
  cutover before release.

## References

- [0.2.70 Evolution Runtime specification](../../.local-docs/specs/runtime-evolution-0.2.70.md)
- [ADR-009: Framework Verifiable Runtime](./009-framework-verifiable-runtime.md)
- [Runtime Authority](../architecture/runtime-authority.md)
- [Runtime Language](../architecture/runtime-language.md)
