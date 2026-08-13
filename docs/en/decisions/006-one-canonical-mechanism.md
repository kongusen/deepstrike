# DEC-6: One Canonical Mechanism

## Status

Accepted

## Date

2026-08-13

## Context

DeepStrike accumulated compatibility decoders, deprecated aliases, and V1/V2 names around the
kernel ABI, checkpoints, durable content, replay, context policy, scheduler, and SDK provider
surfaces. Those names and dispatch fields kept multiple historical mechanisms in the runtime
mental model even when only one was current.

Maintaining those paths duplicated behavior across Rust, Node, Python, and WASM. The project has
chosen a breaking 0.2.60 boundary and accepts that pre-0.2.60 persisted data cannot be read
directly.

## Decision

DeepStrike exposes and implements one unnumbered canonical mechanism at a time.

- First-party runtime contracts carry no ABI, checkpoint, schema, or policy version fields.
- Canonical types and functions use no V1, V2, Current, or Latest qualifiers.
- External input is validated against one strict shape; unknown fields fail closed.
- Runtime migration, revision dispatch, shape inference, and deprecated aliases are not retained.
- Git history, changelogs, migration notes, and negative rejection fixtures preserve history.
- Package semantic versions and externally owned API or model version strings are unaffected.

## Alternatives Considered

### Keep the current mechanism named V2

This would delete migration code while preserving the expectation of parallel in-process format
families.

### Keep a version field with one accepted value

This would force every payload to negotiate a format that has no alternative and leave revision
plumbing distributed across every SDK.

### Provide an offline migration tool

This would preserve knowledge of obsolete shapes and delay complete removal. The 0.2.60 boundary
explicitly accepts hard failure instead.

## Consequences

- Pre-0.2.60 first-party checkpoints, durable content, and replay envelopes are unsupported.
- Public compatibility aliases and deprecated provider classes disappear.
- Boundaries become smaller and stricter, with fewer execution paths to test and secure.
- A future format change must replace the canonical mechanism at a release boundary rather than
  add parallel runtime V1/V2 paths.
- Third-party `/v1` endpoints and versioned model names remain because DeepStrike does not own
  those identifiers.
