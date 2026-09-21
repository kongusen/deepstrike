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

The public facade still requires a provider. Model-first creation and runtime binding
separation belong to 028-28/29 and have not been implemented by these increments.
WASM/Python/Rust have not been migrated in this checkpoint.

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

Next dependency-ordered work: complete those gates, then 028-07 message taxonomy,
028-08 content authority and 028-09 projection pairs. Later waves cover provider
resolution, invocation identity, public API, packaging and release gates.

## Verification

The 028-05 regression tests first failed on duplicate declarations and a lost default
name. The 028-06 tests first failed on absent projection functions, then exposed stale
capability derivation after changing the spec. Both targeted suites now pass.
Node TypeScript build passes. Full Node regression results are recorded in the task.
