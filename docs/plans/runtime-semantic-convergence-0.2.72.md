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
- 028-54: Node, Rust and WASM test surfaces now consume the shared semantic
  contract fixture. The Python fixture test is present but its environment check
  is pinned to the checked-out SDK. `cargo check -p deepstrike-node -p deepstrike-py`
  passes; executing the Python adapter remains blocked on this host because the
  checked-out PyO3 extension cannot be linked against the installed macOS Python
  development symbols. This is a build-environment blocker, not a fixture mismatch.

Runtime binding is optional at definition time; execution reports an unresolved binding explicitly.
WASM/Python/Rust have not been migrated in this checkpoint.

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

The previous Wave 1 commits established a baseline, but their passing Node tests do
not establish every acceptance condition in the proposal. Remaining work includes
classification coverage based on source declarations, ABI-only Canonical exceptions
(the current list also contains provider types), and consolidation of the older
glossary definitions with the new vocabulary. Cross-SDK language conformance is pending.

Next dependency-ordered work: cross-SDK semantic fixture consumption, root export
allowlist, migration guide and final release gates. The Node implementation is the
reference surface; Python, Rust and WASM still need their corresponding fixture checks.

## Verification

The 028-05 regression tests first failed on duplicate declarations and a lost default
name. The 028-06 tests first failed on absent projection functions, then exposed stale
capability derivation after changing the spec. Both targeted suites now pass.
Node TypeScript build passes. Full Node regression results are recorded in the task.
