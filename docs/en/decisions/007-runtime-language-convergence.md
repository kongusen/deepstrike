# ADR-007: Adopt the DeepStrike Runtime Language

## Status

Accepted for 0.2.68.

## Date

2026-09-18

## Context

DeepStrike now distinguishes kernel control flow, provider protocol translation, durable state,
execution evidence, and host accounting. The codebase still uses generic words such as request,
response, message, and usage where authority and representation are materially different. That
ambiguity makes migrations such as DEL-1 and DEL-3 unsafe.

## Decision

0.2.68 adopts the DeepStrike Runtime Language for new code, public documentation, and migration
work. The normative glossary is [Runtime Language](../architecture/runtime-language.md).

- `Intent`, `Fact`, and `Decision` describe the three kernel boundary directions.
- `ToolCall` is provider Fact payload; the kernel derives any admissible intent from it.
- `encode`, `decode`, `normalize`, `render`, and `materialize` name distinct transformations.
- Every value is labelled `Canonical`, `Wire`, `Internal`, or `Durable`.
- Every non-authoritative value is classified as `Reference`, `Projection`, `Cache`, `Measurement`,
  `Evidence`, or `Mirror`.
- Provider `Measurement` becomes kernel accounting only after policy `Settlement`.
- `Route`, `Attempt`, and `Invocation` are separate execution identities.
- `Journal`, `Checkpoint`, and `SessionLog` are State Truth, State Snapshot, and Evidence Truth.

Existing public names remain where they are ABI or SDK compatibility contracts. They are documented
as mirrors or wire vocabulary rather than treated as a second semantic authority.

## Alternatives considered

### Rename every type immediately

Rejected. Mechanical renaming would obscure ABI boundaries and mix semantic migration with breaking
API changes.

### Keep the vocabulary informal

Rejected. Informal terminology has already caused authority and ownership errors in token and
multimodal migrations.

## Consequences

- New APIs and docs must use the glossary terms and identify authority and durability.
- DEL-1 can move token values into a provenance-bearing host measurement side table.
- DEL-3 can move byte materialization to provider adapters while keeping durable content reference-based.
- Code review and docs-drift checks can reject old terminology at representation boundaries.
