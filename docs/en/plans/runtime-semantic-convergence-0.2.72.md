# SPC-028 implementation progress

Target release: 0.2.72. Development branch: `codex/spc-028-language-foundation`.
This document records implementation progress; release approval is a separate gate.

## Implemented increments

- The Node public surface has one `AgentDefinition`, with model-first binding and
  detached run, context, capability, governance, and delegation projections.
- Provider runtime vocabulary is separated into Model, Provider, Endpoint, Protocol,
  Route, Adapter, measurement, usage, settlement, and evidence.
- Root exports are reduced to the public Agent language. Runtime, provider, evaluation,
  and advanced implementation surfaces are available through explicit subpaths.
- Skills, knowledge, workflows, evaluations, MCP servers, handoffs, and guardrails use
  typed public contracts and lower into the existing runtime without duplicate authority.
- Inline Skill content is loaded only after activation. Inline text Knowledge uses a
  deterministic lexical source; external file, URL, directory, and vector sources still
  require an explicit asynchronous binding.
- The advanced runtime provides a budgeted `ContextManager` with provenance, priority,
  response reserve, pinned entries, TTL expiry, deterministic selection, a stable SHA-256
  selected-context fingerprint, and an optional ledger callback. Dynamic Knowledge,
  Memory renewal recalls, Skill pins, and initial context share the same admission path.
- Workflow nodes support scoped context categories and dependency propagation modes:
  `full`, `summary`, and `reference`, with token-derived per-dependency limits.
- Node, Rust, WASM, and Python consume the shared semantic fixture domains. The rebuilt
  Python SDK passes all 30 conformance cases.

## Context management model

Context is assembled on demand. Catalog metadata is exposed before activation, while the
content is admitted only when a Skill, Memory, or Knowledge request resolves. The host
ContextManager performs deterministic budget admission and records lifecycle events; the
Kernel remains the execution authority for committed context. Explicit Skill deactivation,
kernel lease expiry, and run boundaries clear host overlays so stale content cannot reserve
budget.

## Verification

- Node TypeScript build passes.
- Full Node regression passes: 179 suites, 1,103 tests passed, 14 skipped.
- Python SDK conformance passes: 30 tests.
- SDK parity and source-reference checks pass.

Before tagging, run the release version synchronizer, documentation drift check, full
cross-SDK conformance, and the CI release gate on the commit merged to `main`.
