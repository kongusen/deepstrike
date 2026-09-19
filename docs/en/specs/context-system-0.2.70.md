# DeepStrike 0.2.70 — Verifiable Context System

## Status

Accepted and implemented. The kernel candidate, Rust dispatch finalizer, four SDK bridges, and
pre-dispatch session evidence have passed this implementation's acceptance checks. The release
workflow is separate.
This status does not claim that evaluation automatically replays and compares every execution input.

0.2.70 is a hard cut; old Context snapshots and SDK constructors are not compatibility requirements.

## Objective and flow

Context must support finite-budget runtime optimization and verifiable execution inputs while
retaining one semantic authority. Preparation has two stages because the kernel does not own the
provider route or native prompt count:

```text
ContextState + ContextPolicy
  → ContextCandidate in CallProviderEffect
  → host resolves route, encodes request, obtains preflight measurement
  → Rust prepare_context_dispatch
  → ContextPlan + ContextExecutionInput + component evidence
  → persist context_prepared
  → provider I/O
```

`ContextCandidate` is a mandatory field of the existing `CallProviderEffect`. There is no second
kernel effect type and no invented route or measurement placeholder. The effect's normal step digest
anchors the complete candidate. The host uses one Rust implementation to finalize the input after
its route and measurement facts exist.

## Authority and canonical objects

| Object | Role and ownership | Contents |
| --- | --- | --- |
| `ContextState` | Kernel semantic authority, reconstructed from durable state | ordered system/knowledge/history/state entry identities and content digests, task state, signals, generation |
| `ContextPolicy` | Kernel control input | resolved budget, pressure, selection and compression settings |
| `ContextCandidate` | Immutable kernel preparation facts within a provider effect | state, operation/step/sequence, policy digest, exact selection trace, budgets, pressure, cache boundary, wire projection digest |
| `ContextPlan` | Explicit decision finalized by Rust using kernel selection and host facts | candidate decisions plus route identity and measurement references; content-addressed plan identity |
| `ContextExecutionInput` | Frozen execution identity produced before provider I/O | state/policy/plan/projection/measurement/route/cache digests and causal identity |
| Provider projection | Transient ABI view | wire context and exposed tool schemas |
| Route, count, request bytes, native usage | Host evidence | actual provider/profile contract, request fingerprint, preflight and postflight facts |

Rendering and preparation do not implicitly mutate semantic partitions. Entry token counts remain
measurements. The renderer records include/collapse/page-out/omit decisions at source indices, so
repeated identical messages cannot make an omitted entry appear selected. Projected token counts
refer to the rendered content, including state and synthetic anchors.

`ContextState` is a content-addressed index of existing kernel bodies, not a second store of those
bodies. Generation is checkpointed; state digest also detects semantic changes. Existing low-level
mutable partition access is not, by itself, proof of a committed kernel mutation. The production
execution candidate is captured at the committed operation boundary.

## Kernel preparation

`ContextManager::prepare_candidate(operation_id, step_id, input_sequence, policy_digest)` returns
kernel selection facts plus its internal render. The driver checks that this render matches the
actual `LoopAction::CallLLM` projection. It then binds the **wire** `(context, tools)` canonical tuple,
including exposed tools, as `rendered_snapshot` and calculates any prefix digest from wire stable and
knowledge blocks plus the selected prefix turns.

`runtime_inputs` additionally binds measurement side tables, handle residency, knowledge lifecycle,
and cache boundaries. These optimization inputs and pending knowledge survive checkpoint restore.
Resident text, durable blocks, and payload previews of the same content retain the same semantic identity.

The candidate never claims that kernel token estimates are a provider's native count. Its state,
selection trace, budget, and policy are reproducible kernel facts. Optimization changes produce a
new candidate and therefore a different effect/step identity when those facts differ.

## Host finalization

The production boundary is `prepare_context_dispatch`, also exposed as a shared Rust JSON bridge.
Node, Python, and WASM delegate to that core implementation. The request contains the complete
`CallProviderEffect`, the request fingerprint under its declared `request_fingerprint_scope`, the actual provider route object, and a
preflight measurement with input count, source, confidence, and matching request fingerprint.

Core verifies the frozen wire projection and host evidence linkage, then creates `ContextPlan`,
`ContextExecutionInput`, and `EvaluationContextBinding`. The returned preparation includes the
state, plan, route, and measurement evidence bodies needed to retain and inspect those references.
A `context_prepared` session event is persisted **before provider I/O** and contains the preparation
and effect identity. A host must retain the encoded provider request separately when exact provider
bytes are required as evidence. Preflight, provider usage, and normalized settlement measurements
remain distinct facts.

The standalone `ContextManager::prepare_execution_input` remains a low-level core API taking
reference digests. It does not resolve provider routes, encode requests, or independently validate
host evidence bodies. Production dispatch uses the shared finalizer above.

The default session log retains evidence only for its in-memory lifetime. Cross-process recovery and auditing require FileSessionLog or an equivalent durable host store. A failed log append prevents provider dispatch. Built-in providers freeze encoded material for reuse by count and stream. Rust providers freeze the encoded body through `prepare_context_request` and send that same body through `stream_prepared`; the fingerprint also binds post-governance input and provider continuation state. Custom providers default to `adapter_input` scope, proving logical adapter input only, The corresponding Node/WASM hook is `prepareRequest`, and Python uses `prepare_request`. Default adapters require JSON-recordable state; providers with hidden encoding state or non-JSON state must implement their own preparation boundary. `rendered_snapshot` binds the kernel projection; the request fingerprint binds the host-transformed request. These are separate claims.

## Replay and evaluation

Normal kernel replay reproduces `ContextCandidate` and verifies it through the ordinary effect/step
digest. Given that reproduced candidate and recorded host route/measurement evidence, the same Rust
finalizer can recompute `ContextExecutionInput.input_digest` for comparison with `context_prepared`.
A host comparison must report a mismatch even if provider output happens to match.

`EvaluationContextBinding` projects an execution input into evaluation evidence. It binds
`execution_input`, `context_state`, `context_policy`, `context_plan`, `rendered_snapshot`,
`prompt_measurement`, `provider_route`, and optional `cache_prefix`. The existing evaluation
validator checks binding integrity, at least one binding per evaluated operation with no duplicate execution input within that operation, and evidence-reference
coverage. It does not automatically load every session event, replay an operation, or compare all
provider-attempt inputs; an evaluation host must supply that replay/comparison evidence explicitly.
An operation-level binding does not represent a complete trace of every provider attempt.

## Acceptance checks

The connected implementation must be verified against these cases before release:

- state, candidate selection, policy, or wire context/tool changes affect the recorded identity;
- an unknown/duplicate entry reference, a corrupt plan, or a mismatched generation is rejected;
- a provider projection differing from the frozen candidate is rejected;
- a measurement for a different request fingerprint is rejected;
- a cache claim identifies an actual prefix of the frozen wire projection;
- session persistence of `context_prepared` precedes provider execution;
- checkpoint restore and deterministic replay reproduce the candidate;
- recorded host facts rebind to the same execution input, and altered facts yield a different
  identity or a validation error;
- SDKs call the same Rust finalizer and do not implement independent digest or selection rules;
- evaluation coverage and component evidence-reference validation remain fail-closed.

Provider-specific raw bytes remain host evidence. This contract does not make the kernel a provider
router, introduce another scheduler, or certify host-provided observations as independently true.

## Verification (2026-09-19)

- Rust workspace: 1471 passed, 25 ignored (including 1137 core and 128 Rust SDK tests).
- Node: 1054 passed, 14 skipped; Python: 739 passed, 2 skipped; WASM: 193 passed.
- Cross-SDK conformance: 24 fixtures × 4 SDKs, with 14 runner-owned domain-coverage skips and no failures. Includes Context execution and encoded-request/continuation fingerprints.
- Regressions cover checkpoint/replay, restore equivalence, failed-log dispatch blocking, frozen encoding, measurement isolation, and rejection of non-JSON state.
- Docs drift: 157/157 paths, 274/274 symbols, 63/63 translated counterparts; docs build, formatting, and diff checks pass.
