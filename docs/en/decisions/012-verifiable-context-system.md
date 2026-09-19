# ADR-012: Verifiable Context System

## Status

Accepted and implemented. This implementation passed acceptance checks recorded in the linked specification; the release workflow is separate.

## Date

2026-09-19

## Context

Partitioned Context, compression, cache boundaries, and measurement side tables already support
runtime optimization. Verification also needs to identify the state, selection, projection, and host
facts consumed by each provider attempt. The kernel does not know the host's final route or native
prompt count when it emits a provider effect.

## Decision

Use two preparation stages with one Rust implementation of canonical validation and hashing:

1. Kernel `ContextState` remains the semantic authority. The renderer emits an exact selection
   trace, and the driver freezes a `ContextCandidate` in the mandatory field of `CallProviderEffect`.
   The candidate binds state, policy, selection, budget, and the wire `(context, tools)` digest.
2. The host resolves the route, encodes the request, and obtains a fingerprinted preflight count.
   It calls Rust `prepare_context_dispatch` through the shared core/JSON boundary to produce
   `ContextPlan`, `ContextExecutionInput`, and component evidence. It persists `context_prepared`
   before provider I/O.

There is no second kernel effect type, fabricated provider route, or placeholder native count.
`ContextExecutionInput` freezes before provider I/O, after the kernel effect has been emitted.
Provider request bytes and native usage remain host evidence. SDKs mirror contracts and delegate
finalization to Rust; they do not own independent selection or canonical digest rules.

`ContextManager::prepare_execution_input` remains a low-level reference-digest API. Production
execution uses the host-evidence-aware dispatch finalizer.

## Alternatives considered

- **Make provider rendering authoritative:** rejected because transport views must not become a
  second writer of semantic state.
- **Keep selection implicit:** rejected because equal rendered content cannot identify which
  duplicate source entry was omitted or why another was collapsed.
- **Freeze a complete input inside the kernel effect:** rejected because the host's route and count
  are not yet known. Placeholder identities would claim evidence that does not exist.
- **Add a second preparation effect:** unnecessary; the immutable candidate fits the existing
  provider effect and the host already owns provider dispatch.
- **Store raw provider requests in the kernel ledger:** rejected; host storage retains provider
  evidence while the kernel ledger anchors the candidate through its normal step digest.

## Consequences

Kernel replay reproduces candidates through ordinary effect/step verification. A host can rebind
recorded route and measurement evidence using the same finalizer, then compare the resulting
execution-input identity with `context_prepared`.

`EvaluationContextBinding` binds that execution input and its diagnostic component references.
Existing evaluation validation checks binding integrity and evidence coverage. Automatic loading
of session evidence, per-attempt replay, and evaluation comparison are not implied by the binding;
evaluation hosts must supply comparison evidence explicitly.

The 0.2.70 hard cut permits removal of old Context snapshots and SDK constructors. Full SDK,
checkpoint/replay, tamper, persistence-ordering, and documentation gates are release acceptance
requirements, not inferred from the existence of the new types.

References: [Context System 0.2.70](../specs/context-system-0.2.70.md) · [Context Contract](../architecture/evaluation-context.md)

Evidence retention depends on the host store: default in-memory logs do not promise cross-process durability; durable auditing requires FileSessionLog or an equivalent store.
