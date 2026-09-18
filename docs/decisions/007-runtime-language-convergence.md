# ADR-007: Adopt the DeepStrike Runtime Language

## Status

Accepted for 0.2.68.

## Date

2026-09-18

## Context

DeepStrike now has explicit distinctions between kernel control flow, provider protocol
translation, durable state, execution evidence, and host accounting. The codebase still uses
older generic words such as “request”, “response”, “message”, and “usage” in places where the
authority and representation are materially different. That ambiguity makes migrations such as
DEL-1 and DEL-3 unsafe: a runtime projection can be mistaken for a durable fact, and a provider
measurement can be mistaken for kernel accounting.

## Decision

0.2.68 adopts the DeepStrike Runtime Language as the vocabulary for new code, public docs, and
migration work. The normative glossary is [Runtime Language](../architecture/runtime-language.md).

The rules are:

- `Intent`, `Fact`, and `Decision` describe the three kernel boundary directions.
- `ToolCall` is provider Fact payload; kernel derives any admissible intent from it.
- `encode` and `decode` name the Provider Protocol ↔ Canonical Semantic boundary.
- `normalize`, `render`, and `materialize` name distinct transformations and must not be used
  interchangeably.
- Every value is labelled by its representation: `Canonical`, `Wire`, `Internal`, or `Durable`.
- Every non-authoritative value is classified as `Reference`, `Projection`, `Cache`, `Measurement`,
  `Evidence`, or `Mirror`.
- Provider `Measurement` is converted by policy into `Settlement` before kernel accounting.
- `Route`, `Attempt`, and `Invocation` are separate execution identities.
- `Journal`, `Checkpoint`, and `SessionLog` are respectively State Truth, State Snapshot, and
  Evidence Truth.

Existing public names remain only where they are ABI or SDK compatibility contracts. Such names
must be documented as mirrors or wire vocabulary rather than treated as a second semantic authority.

## Alternatives considered

### Rename every type immediately

Rejected. Mechanical renaming would obscure ABI boundaries and would mix semantic migration with
breaking API changes.

### Keep the vocabulary informal

Rejected. The current token and multimodal migrations already show that informal terminology
causes authority and ownership errors.

## Consequences

- New APIs and docs must use the glossary terms and identify authority and durability.
- DEL-1 can move token values into a provenance-bearing host measurement side table without
  confusing them with SessionLog evidence or checkpoint accounting anchors.
- DEL-3 can move byte materialization to provider adapters while keeping canonical durable content
  reference-based.
- Code review and docs-drift checks can reject old terminology at representation boundaries.
