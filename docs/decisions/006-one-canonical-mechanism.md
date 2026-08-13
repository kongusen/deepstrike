# DEC-6: One Canonical Mechanism

## Status

Accepted

## Date

2026-08-13

## Context

DeepStrike accumulated compatibility decoders, deprecated aliases, and V1/V2 names around the
kernel ABI, checkpoints, durable content, replay, context policy, scheduler, and SDK provider
surfaces. Although the current implementations were canonical in practice, their versioned names
and dispatch fields kept multiple historical mechanisms present in the runtime mental model.

Maintaining those paths duplicated behavior across Rust, Node, Python, and WASM and made every new
change answer two questions: how the mechanism works, and how old representations are lifted into
it. The project has explicitly chosen a breaking 0.2.60 boundary and accepts that pre-0.2.60 persisted
data will not be read directly.

## Decision

DeepStrike exposes and implements one unnumbered canonical mechanism at a time.

- First-party runtime contracts do not carry ABI, checkpoint, schema, or policy version fields.
- Canonical types and functions do not use V1, V2, Current, or Latest qualifiers.
- External input is validated against one strict shape. Unknown fields fail closed.
- Runtime migration, revision dispatch, format inference, and deprecated aliases are not retained.
- Git history, changelogs, migration notes, and negative rejection fixtures preserve history.
- Package semantic versions and externally owned API/model version strings are unaffected.

## Alternatives Considered

### Keep the current mechanism named V2 and reject V1

This deletes migration code but retains the expectation that another in-process V3 branch will
eventually coexist. It does not remove the versioned architecture mental model.

### Keep version fields with only one accepted value

This is strict but still makes every payload negotiate a format that has no alternative. It also
leaves revision plumbing distributed across every SDK.

### Provide an offline migration tool

This would reduce operational disruption but preserve old shape knowledge and delay complete
removal. The 0.2.60 boundary explicitly accepts hard failure instead.

## Consequences

- All pre-0.2.60 first-party checkpoints, durable content, and replay envelopes are unsupported.
- Public compatibility aliases and deprecated provider classes disappear.
- Boundaries become smaller and stricter, with fewer execution paths to test and secure.
- A future format change must replace the canonical mechanism as a release-level breaking change;
  it must not introduce parallel runtime V1/V2 paths.
- Third-party endpoints such as `/v1` and model names such as `moonshot-v1-*` remain because those
  identifiers are owned outside DeepStrike.
