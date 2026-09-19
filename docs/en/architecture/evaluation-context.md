# Context Contract in Evaluation Runtime

Context serves runtime optimization and verifiable execution through a two-stage boundary:

```text
ContextState → ContextCandidate in CallProviderEffect
  → host route + fingerprinted preflight measurement
  → Rust prepare_context_dispatch
  → ContextPlan + ContextExecutionInput
  → persist context_prepared → provider I/O
```

## Authority and preparation

`ContextState` is the kernel's semantic authority, indexing ordered partition content, task state,
signals, and generation. `ContextPolicy` supplies resolved runtime controls. Renderer provenance
records selection at source indices, including omission, collapse, and paging.

The driver freezes those facts in mandatory `CallProviderEffect.context_candidate`. Its
`rendered_snapshot` covers the canonical wire `(context, tools)` tuple, and its cache boundary refers
to wire stable/knowledge blocks and prefix turns. The ordinary effect/step digest anchors the
candidate. The kernel does not invent a provider route or native count, and no extra kernel effect
is introduced.

The host resolves the actual route and request fingerprint, obtains the preflight measurement, then
calls the shared Rust `prepare_context_dispatch` implementation. That call validates projection and
evidence linkage and finalizes `ContextPlan` and `ContextExecutionInput`. Node, Python, and WASM
use core bridges; they do not implement their own selection or hashing rules.

The returned state, plan, route, measurement, execution input, and binding are retained in a
`context_prepared` session event before provider I/O. Raw encoded request bytes and postflight usage
remain separate host evidence. Low-level `ContextManager::prepare_execution_input` accepts reference
digests; it is not the production host-evidence validation boundary.

## Evaluation binding

`EvaluationContextBinding` projects a frozen input into evaluation evidence. It binds
`execution_input`, `context_state`, `context_policy`, `context_plan`, `rendered_snapshot`,
`prompt_measurement`, `provider_route`, and optional `cache_prefix`. Each reference must appear in
`EvaluationRun.evidence_refs`; each evaluated operation has at least one binding; separate steps may bind distinct execution inputs, and a repeated input is rejected.

This validates binding integrity and coverage. It does not automatically load session records,
replay all provider attempts, or compare their inputs. An operation-level binding is not a complete
attempt trace; evaluation hosts provide replay/comparison evidence explicitly.

## Replay and acceptance

Normal kernel replay reproduces the candidate through ordinary step-digest verification. Given the
recorded host evidence, the shared finalizer can rebind the reproduced candidate and recompute
`input_digest` for comparison with the persisted event. Changed route, count, projection, or policy
must produce a different identity or a validation failure, even when provider output is unchanged.

The two-stage implementation passed the four-SDK, checkpoint/replay, persistence-ordering, tamper,
and documentation checks recorded in the specification. The release workflow is separate.

Specification: [Context System 0.2.70](../specs/context-system-0.2.70.md) · [ADR-012](../decisions/012-verifiable-context-system.md) · [Runtime Language](./runtime-language)

Evidence retention depends on the host store: default in-memory logs do not promise cross-process durability; durable auditing requires FileSessionLog or an equivalent store.
