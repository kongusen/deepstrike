# Implementation Plan: One Canonical Mechanism

## Overview

Implement `docs/specs/remove-all-legacy.md` as a breaking 0.2.60 cutover. Delete legacy behavior and
remove first-party V1/V2 format identities so the current mechanism is expressed through one strict,
unnumbered contract in every SDK.

## Architecture Decisions

- Strict shape replaces version dispatch. Former version fields are rejected as unknown input.
- Canonical names have no version, `Current`, or `Latest` suffix.
- Package versions and externally owned version strings remain unchanged.
- Each slice updates its contract tests before removing the old implementation.
- Pre-existing durable-wait edits are preserved and become part of the canonical scheduler path.

## Task List

### Phase 1: Unversion Public Policy Contracts

- [x] Task 1: Rename the Rust context-policy contract and remove its runtime version field.
  - Acceptance: `ContextPolicy` is the only Rust policy type; strict deserialization rejects
    `version`; focused tests pass.
  - Verify: `cargo test -p deepstrike-core context::policy`
  - Files: core policy, context manager, kernel config and focused tests.

- [x] Task 2: Apply the same unversioned context-policy contract to Node, Python, and WASM.
  - Acceptance: public exports expose only unversioned policy names and emit no `version` field.
  - Verify: SDK builds plus context-policy tests.
  - Files: each SDK policy module, public export, runner and tests, split per SDK while editing.

### Checkpoint: Context Policy

- [x] Rust and all SDKs build with one context-policy contract.
- [x] No `ContextPolicyV1` family symbol remains.

### Phase 2: Durable Content and Replay

- [x] Task 3: Remove Rust durable-content schema versions and legacy tool-result decoding.
  - Acceptance: canonical blocks decode strictly; old `output` and `schema_version` shapes reject.
  - Verify: focused durable-content and kernel checkpoint tests.
  - Files: core durable content, checkpoint/driver integration and fixtures.

- [x] Task 4: Remove Node durable-content versions, migration exports, and compatibility tests.
  - Acceptance: no migration helper or schema field is exported; old shapes reject.
  - Verify: Node durable-content tests and build.

- [x] Task 5: Apply Task 4 to Python and WASM.
  - Acceptance: cross-SDK canonical fixtures and rejection behavior agree.
  - Verify: focused Python/WASM tests and builds.

- [x] Task 6: Require explicit provider replay protocol in every SDK.
  - Acceptance: replay protocol is never inferred; missing protocol cannot seed provider state.
  - Verify: provider fallback/replay tests in Node, Python, and WASM.

### Checkpoint: Durable Contracts

- [x] Durable fixtures contain no first-party schema/version field.
- [x] Cross-SDK durable and replay suites pass.

### Phase 3: Kernel ABI and Checkpoint

- [x] Task 7: Remove ABI revision from the canonical envelope and bindings.
  - Acceptance: one strict envelope exists; old ABI fields and revision mismatch branches are gone.
  - Verify: kernel wire decode, binding, Rust SDK and conformance tests.

- [x] Task 8: Remove checkpoint revision dispatch and V1 migration.
  - Acceptance: checkpoint decodes one strict shape; migration DTOs/functions/fixtures are removed;
    old checkpoint fields reject.
  - Verify: core checkpoint, restore, replay, and driver tests.

- [x] Task 9: Rename all current checkpoint state DTOs without V1 suffixes.
  - Acceptance: canonical state names are used in core projection/restore/transactions.
  - Verify: `cargo test -p deepstrike-core`.

### Checkpoint: Kernel

- [x] Core focused tests pass with existing durable-wait edits intact.
- [x] No first-party ABI/checkpoint revision branch remains.

### Phase 4: Public API and Runtime Fallbacks

- [x] Task 10: Delete deprecated Node/WASM aliases and provider subclasses.
  - Acceptance: factories and canonical types are the only exports; API-surface tests prove removed
    symbols are absent.
  - Verify: Node/WASM build and API tests.

- [x] Task 11: Delete Python compatibility-only provider classes and constructor forms.
  - Acceptance: factories/canonical provider classes are the only public construction surface.
  - Verify: Python provider and API tests.

- [x] Task 12: Remove scheduler/workflow/collaboration legacy fallbacks.
  - Acceptance: only `WaitSet`, canonical roles, and kernel pace decisions drive behavior.
  - Verify: core scheduler plus Node/Python/WASM workflow tests.

- [x] Task 13: Remove the benchmark legacy input importer.
  - Acceptance: benchmark commands accept only `MetricSet` input.
  - Verify: benchmark tests.

### Phase 5: Documentation and Release Boundary

- [x] Task 14: Record the one-mechanism ADR and update current documentation.
  - Acceptance: current docs contain no V1/V2 mechanism framing; historical release records remain.
  - Verify: documentation scan and docs build/check.

- [x] Task 15: Update release metadata and migration notes for 0.2.60.
  - Acceptance: the breaking persisted-format and API cutover is documented consistently.
  - Verify: version verification workflow or equivalent local checks.

### Final Checkpoint

- [x] Full Rust, Node, Python, WASM, benchmark and docs checks pass. The Node local-endpoint
  characterization test remains environment-gated because this sandbox cannot bind a loopback
  port; the other 151 Node suites pass.
- [x] Production scans show no legacy path, deprecated compatibility alias, first-party V1/V2
  mechanism name, or runtime format-version axis.
- [x] Third-party version strings and protocol interoperability remain intact.

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Old durable data is unreadable | Intentional, high | Explicit 0.2.60 notes and deterministic rejection |
| Cross-SDK shape drift | High | Shared fixtures and focused parity tests after every slice |
| Removing a third-party `/v1` literal | High | Exact-symbol/field edits; exempt external URLs and model IDs |
| Dirty scheduler files are overwritten | High | Inspect diffs before each overlapping patch; never restore/reset |
| Broad mechanical rename hides behavior changes | Medium | Compile/test immediately after each identifier family |

## Open Questions

None. The user approved the 0.2.60 hard cutover, including rejection of current 0.2.53 persisted data.
