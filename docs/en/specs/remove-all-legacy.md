# Spec: Remove All Legacy Compatibility

## Objective

DeepStrike 0.2.60 removes every first-party legacy compatibility path and every V1/V2-style
mechanism identity from production code. Git history, changelogs, migration notes, and negative
rejection fixtures remain the historical record. The runtime and public SDKs support one
unnumbered canonical mechanism.

This is an intentional breaking boundary. Old checkpoints, replay envelopes, aliases,
constructors, default semantics, and compatibility fallbacks are not migrated at runtime.
First-party canonical payloads do not negotiate by version. Stale durable or public input fails
closed as an invalid canonical shape.

## Scope

Remove:

- Checkpoint lifting and old content carriers.
- First-party runtime version fields, including ABI, checkpoint, schema, and context-policy fields.
- V1/V2 names on canonical types, functions, constants, fixtures, and current documentation.
- Durable-content migration helpers and permissive replay protocol inference.
- Legacy scheduler projections, waits, workflow fallbacks, aliases, wrapper classes, constructor
  forms, role mappings, and benchmark importers.
- Tests that assert obsolete input remains accepted.

Retain:

- Git history, changelogs, released-version migration documents, and ADR history.
- Negative tests proving that unsupported old input is rejected deterministically.
- Current OpenAI-compatible and Anthropic-compatible interoperability adapters.
- Package release versions and externally owned versioned URLs, model names, and identifiers.

## One-Mechanism Rule

- Canonical types and builders use direct names without V1, V2, Current, or Latest qualifiers.
- Canonical envelopes, checkpoints, durable content, replay records, and context policy carry no
  first-party runtime version discriminator.
- Decoders implement one strict shape, with no revision dispatch, negotiation, migration ladder,
  or payload-shape inference.
- Tests call the active shape canonical, accepted, or rejected rather than assigning it a version.
- Architecture documentation explains the mechanism itself rather than its historical sequence.

Package semantic versions and third-party protocol strings are unaffected.

## Canonical Behavior

- Only the strict canonical checkpoint shape is readable and writable.
- Durable content uses canonical content blocks and no schema-version field.
- Provider replay must declare its protocol explicitly.
- The kernel envelope and context policy each use one strict, unversioned shape.
- Scheduler tasks use durable `WaitSet` state and canonical kernel roles.
- Workflow continuation is driven only by kernel decisions.
- Optional exposure fields use current strict semantics and cannot resurrect permissive defaults.
- Public SDKs expose factories and canonical types only.

Unknown fields fail closed at Rust, TypeScript, Python, and WASM boundaries before they reach the
kernel or provider adapters.

## Verification

- Focused rejection tests cover old checkpoint, durable-content, replay, and ABI inputs.
- Full Rust, Node, Python, and WASM builds and suites pass.
- Source scans find no production legacy path, deprecated compatibility alias, first-party V1/V2
  mechanism name, or runtime format-version axis.
- Release notes identify 0.2.60 as the breaking persisted-format and public-API boundary.

## Constraints

Pre-existing durable-wait changes are preserved and integrated into the canonical scheduler path.
The user approved the 0.2.60 hard cutover, including direct rejection of 0.2.53 persisted data.
