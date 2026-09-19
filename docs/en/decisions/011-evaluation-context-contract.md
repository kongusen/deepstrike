# ADR-011: Bind Context Evidence to the Evaluation Runtime

## Status

Accepted

## Date

2026-09-19

## Context

0.2.70 made artifact lineage and promotion verifiable, but an evaluation could still name operations
without proving which context policy, accepted input, rendered prompt, or token measurement produced its
result. That leaves a replay gap: a changed renderer or retrieval result could hide behind the same
operation and dataset identifiers.

Context is already split into canonical state, policy, projection, cache, measurement, and evidence by
the Runtime Language. The missing piece is a compact binding in the evaluation graph.

## Decision

Add `EvaluationContextBinding` to the canonical Rust Evolution contract and require one binding for every
`EvaluationRun.operation_ids` entry. Its digest covers `operation_id`, the `ContextExecutionInput`
digest, ContextState, resolved policy, admitted plan, rendered snapshot, prompt measurement, provider
route, and an optional cache-prefix digest. The validator requires every bound digest to appear in
`evidence_refs`, checks binding integrity, and rejects unknown or unbound operations. The two-stage
Context preparation boundary is defined by [ADR-012](./012-verifiable-context-system.md).

Context bytes stay host-owned evidence. Rendered context remains a projection, and token/provider usage
remains measurement evidence; neither becomes a second kernel authority. Node, Python, and WASM expose
the same mirror shape and delegate validation to the Rust core.

## Alternatives considered

### Store rendered context bytes in the kernel ledger

Rejected. It duplicates large context state, couples the kernel to storage, and makes the ledger a second
context authority.

### Record only a prompt token count

Rejected. A count cannot prove which policy, input, retrieval result, or renderer output was measured.

### Keep Context outside EvaluationRun

Rejected. The evaluation graph would still allow an operation to be compared without a verifiable context
identity, leaving replay and regression evidence incomplete.

## Consequences

- Every new evaluation payload includes `contexts` and its evidence references.
- A policy, retrieval, renderer, cache, or measurement change creates a new binding and evaluation digest.
- Host stores remain free to choose how context bytes and provider evidence are persisted.
- SDK mirrors stay transport-only and cannot diverge in Context semantics.

## References

- [Context contract](../architecture/evaluation-context.md)
- [Evolution Runtime](../architecture/evolution-runtime.md)
- [Runtime Language](../architecture/runtime-language.md)
