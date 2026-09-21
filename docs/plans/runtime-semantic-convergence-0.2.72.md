# SPC-028 implementation progress

Target release: 0.2.72. Development branch: `codex/spc-028-language-foundation`.
This is an implementation checkpoint, not release approval.

## Implemented Node increments

- 028-05: one `AgentDefinition` declaration in Node. JSON normalization uses
  `AgentDescriptor`; normalization also accepts the public facade definition and
  shares its default agent name. An AST declaration scan guards against recurrence.
- 028-06: `AgentSpec.inputs` is removed. Detached run, context, capability,
  governance and delegation projections are available through the advanced subpath.
  Capability lists are computed from current declarations and the current filter.
- 028-07/08/09: Node now names `ModelMessage`, `StoredMessage`, `RuntimeMessage` and
  `WireMessage`, registers content and projection authority directions, and exposes
  the registry through tests.
- 028-10: `GenerationProtocol` has one declaration and an explicit protocol list.
- 028-28/29: public definitions accept `model` without requiring a Provider; the
  runtime fails at execution when no Provider binding has been resolved.
- 028-47/49: `@deepstrike/sdk/runtime` and `@deepstrike/sdk/evals` package subpaths
  are declared and built.
- 028-11 through 028-27: provider route identity, exact request fingerprint
  measurement reuse, usage settlement policy, attempt evidence and provider
  conformance fixtures are now covered by executable gates.
- 028-36: Skill resources, scripts, tools, MCP servers and knowledge references
  now have typed public containers instead of `unknown[]` placeholders.
- 028-39/41: public `WorkflowDefinition` and `WorkflowStep` use named agents and
  lower to the existing runtime WorkflowSpec. Step dependencies are resolved from
  stable public step ids to runtime indexes and unknown dependencies fail closed.
- 028-43/44: public `Dataset`, `Evaluator`, `EvalRun` and `evaluate()` now form a
  typed evaluation entry point over Agent.run. Optional `includeTrace` evidence
  carries executed input, context binding, route, measurement and artifact set
  without changing the basic EvalResult shape.
- 028-63/66/67: added the 0.2.71 to 0.2.72 migration guide and updated the Node
  Quick Start/package layout for the public Agent language and runtime subpaths.
- 028-65: removed Node surface compatibility adapters and the `ProviderMessage`
  alias; runtime code now uses `ModelMessage` directly. Provider protocol adapters
  remain because they are execution implementations, not legacy public aliases.
- 028-51/52/53: WASM and Python public bindings now use `ModelMessage`; the Rust
  core wire struct keeps its internal `ProviderMessage` name because it is a Kernel
  wire representation rather than an SDK compatibility alias.
- 028-46/50: root Node exports no longer expose `KernelJournal`,
  `ProviderRequestPlan`, `ProviderAttempt`, `ContextPrepared` or
  `EvolutionRuntime`; evolution types are also removed from the root type surface.
  Those implementation surfaces are available through the runtime, providers and
  advanced subpaths. The Node conformance adapter now consumes those subpaths
  directly.
- 028-37/38: handoff targets are allowlisted at the Agent boundary and guardrail
  policies lower into GovernancePolicy with deny/veto aggregation.
- 028-36/37: skills and text knowledge seed runtime context; MCP stdio servers have
  explicit async connect/disconnect lifecycle, local tools can coexist with MCP
  tools, unsupported transports fail closed, and unbound server auth is rejected.
- Context follow-up: inline Skill content is now exposed as metadata at run start
  and pinned only after dynamic activation. Inline text Knowledge uses a deterministic
  lexical `KnowledgeSource` and is retrieved through the `knowledge` capability on
  demand; file, URL, directory and vector sources remain explicit asynchronous bindings.
- Context follow-up: advanced runtime now exposes a budgeted `ContextManager` with
  typed item provenance, priority ordering, response reserve, TTL expiry and pinned
  entries. `RuntimeRunner.pushKnowledge()` now gates dynamic Knowledge, Skill and
  initial context through that manager before committing to the Kernel. Kernel
  context remains the execution authority while host selection is deterministic and
  testable.
- Context observability follow-up: `ContextManager` now exposes a stable selected-context
  fingerprint and an optional append-only ledger callback for add, remove, expiry and
  selection events. Renewal memory recalls use the same `pushKnowledge()` admission path,
  so dynamic memory and Skill/Knowledge overlays share one budget and provenance surface.
  Explicit Skill deactivation and kernel lease expiry also clear the host overlay immediately.
- Workflow Context follow-up: `WorkflowStep.context` and `WorkflowNodeSpec.context`
  now constrain dependency propagation. Nodes can choose `full`, `summary` or
  `reference` dependency data, select the context categories they receive, and set
  a per-dependency token-derived limit.
- 028-54: Node, Rust and WASM test surfaces now consume the shared semantic
  contract fixture. The Python fixture test is present but its environment check
  is pinned to the checked-out SDK. After rebuilding the PyO3 extension with
  maturin against Python 3.12, all 30 Python SDK conformance cases pass; Rust
  binding checks also pass.

Runtime binding is optional at definition time; execution reports an unresolved binding explicitly.
Python semantic conformance is rebuilt against the checked-out SDK and passes; Rust and WASM
consume the shared fixture surfaces. Remaining release work is limited to the explicit final
audit and release-gate evidence listed below.

The Node facade now names the two public stages explicitly: `AgentDefinition` is the
serializable declaration and `AgentRuntime` is the executable handle returned by
`createAgent`. The internal `Agent` class remains a lowering input and is not the
execution contract.

## Node migration notes

For internal JSON descriptors, replace the old `AgentDefinition` import from
`agent-ir` with `AgentDescriptor` (also available from `@deepstrike/sdk/advanced`).
Application code continues to import the single public `AgentDefinition` from root.

Replace reads of `spec.inputs.run`, `.context`, `.capabilities`, `.governance` and
`.delegation` with the respective `projectAgentRun`, `projectAgentContext`,
`projectAgentCapabilities`, `projectAgentGovernance` and `projectAgentDelegation`
functions imported from `@deepstrike/sdk/advanced`. Use `spec.memory` for the memory
declaration. Projections are detached values; changing one does not edit the spec.
Edit the semantic fields on the spec and request a fresh projection instead.

## Remaining acceptance work

The implementation waves and cross-SDK fixture checks are complete for this checkpoint.
Before release, run the final semantic audit, verify the generated package surfaces against
the allowlist, and attach the release-gate evidence. The Node implementation remains the
reference surface; Python, Rust and WASM consume the same semantic fixture domains.

## Verification

The 028-05 regression tests first failed on duplicate declarations and a lost default
name. The 028-06 tests first failed on absent projection functions, then exposed stale
capability derivation after changing the spec. Both targeted suites now pass.
Node TypeScript build passes. Full Node regression: 179 suites passed, 6 skipped;
1,103 tests passed, 14 skipped. Python SDK conformance: 30 passed. Context-manager
targeted tests: 5 passed.
