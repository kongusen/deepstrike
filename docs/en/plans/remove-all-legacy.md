# Implementation Plan: One Canonical Mechanism

## Overview

Implement the legacy-removal specification as a breaking 0.2.60 cutover. Delete old behavior and
remove first-party V1/V2 identities so every SDK exposes one strict, unnumbered contract.

## Architecture Decisions

- Strict shape validation replaces version dispatch.
- Canonical names have no version, Current, or Latest suffix.
- Package versions and externally owned version strings remain unchanged.
- Contract tests are updated before obsolete implementations are deleted.
- Existing durable-wait edits remain part of the canonical scheduler path.

## Tasks

### Public policy contracts

- [x] Remove the runtime version field from Rust context policy.
- [x] Apply the unversioned policy contract to Node, Python, and WASM.

### Durable content and replay

- [x] Remove durable-content schema versions and old tool-result decoding.
- [x] Delete migration exports and old-format acceptance tests in every SDK.
- [x] Require an explicit provider replay protocol.

### Kernel ABI and checkpoints

- [x] Remove the ABI revision from the canonical envelope and bindings.
- [x] Delete checkpoint revision dispatch, migration DTOs, helpers, and fixtures.
- [x] Rename active checkpoint state types without V1/V2 suffixes.

### Public API and runtime behavior

- [x] Delete deprecated Node, Python, and WASM aliases and provider wrappers.
- [x] Remove old constructor forms, scheduler projections, role mappings, and workflow fallbacks.
- [x] Remove the benchmark old-input importer.

### Documentation and release

- [x] Record the one-mechanism decision and update current documentation.
- [x] Set release metadata and migration notes to 0.2.60.

## Final Checkpoint

- [x] Full Rust, Node, Python, WASM, benchmark, and documentation checks pass. The Node
  local-endpoint characterization test remains environment-gated because this sandbox cannot bind
  a loopback port; the other 151 Node suites pass.
- [x] Production scans show no legacy path, compatibility alias, first-party V1/V2 mechanism, or
  runtime format-version axis.
- [x] Third-party version strings and current protocol interoperability remain intact.

## Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Old durable data is unreadable | Intentional, high | Explicit 0.2.60 notes and deterministic rejection |
| Cross-SDK shape drift | High | Shared fixtures and parity tests |
| Third-party `/v1` strings are removed | High | Exact-symbol edits and explicit exemptions |
| Existing scheduler work is overwritten | High | Review overlapping diffs; never reset user work |

There are no open compatibility exceptions. The 0.2.60 boundary intentionally rejects 0.2.53
persisted data.
