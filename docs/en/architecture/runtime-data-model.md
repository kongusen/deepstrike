---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [WireEnvelope, KernelInput, KernelEffect, KernelTerminal, SyscallRequest, SessionEvent]
  python: [RuntimeRunner, KernelJournal]
---

# Canonical Runtime Data Model

This is the charter of DeepStrike's runtime data constitution: the five semantic layers, the Intent/Fact/Decision loop, six engineering principles, and boundary invariants B1–B9. Companion documents: [Authority matrix](./runtime-authority), [Persistence contract](./runtime-persistence), [Causality & verification](./runtime-causality).

> Provenance: the P0–P8 research arc (2026-09-15, archived in `.local-docs/specs/runtime-data-model-p0..p8-*.md`) and the iteration roadmap (`.local-docs/specs/runtime-roadmap-0.2.63-plus-2026-09-15.md`). This page is the promulgated form; evidence with file:line citations lives in the archive.

## Five Semantic Layers

Layers are drawn by **data semantics**, not by package. No struct may span more than two layers.

```text
L0 · Protocol Evidence   Raw provider wire bytes, ProviderWireEvidence
                         Authority: Provider Adapter
L1 · Semantic State      CanonicalMessageState (currently StoredMessageState), DurableContent
                         Authority: Canonical Semantic Model
L2 · Execution Evidence  ModelInvocation / ProviderAttempt / ResolvedProviderRoute /
                         ProviderUsage / UsageAccountingPolicy / TokenMeasurement
                         Authority: Host Execution Runtime
L3 · Kernel Control      KernelInput / KernelEffect / EffectOutcome / Task / Capability / Budget
                         Authority: Kernel
L4 · Durable Truth       Journal = State Truth / Checkpoint = State Snapshot /
                         SessionLog = Evidence Truth
```

## The Three-Word Loop

Everything crossing the kernel boundary is one of three words (article B8):

| Word | Direction | Meaning |
|---|---|---|
| **Intent** | host → kernel | "I want this to happen" (ConfigureOperation / StartOperation / HostControl) |
| **Fact** | host → kernel | "This actually happened" (ResolveEffect outcomes / DeliverExternalEvent) |
| **Decision** | kernel → host | "Adjudicated: this is allowed/required" (KernelEffect / StepDisposition::Terminal) |

Anything crossing the boundary that fits none of the three, and is not a registered projection/observation form, is a constitutional violation.

**Key ruling: a model ToolCall is not an Intent.** It arrives as Fact payload (ProviderCompleted.message.tool_calls); the Intent is reconstructed inside the kernel by `derive_provider_syscalls`, behind three fail-closed gates: unknown effect / unexposed tool / consumed call_id. **The host never declares the caller** (article B9) — identity is derived by the kernel from (effect_id, call_id, published surface).

## Six Engineering Principles (A1–A6)

Every spec and PR passes these first:

| Principle | Meaning |
|---|---|
| **A1 Single Authority** | One fact, one authority |
| **A2 Explicit Representation** | A non-authority is one of the six registered forms below |
| **A3 Explicit Identity** | Every ID is kernel-minted / host-bound / provider-adopted / content-addressed (see [Authority](./runtime-authority)) |
| **A4 Explicit Causality** | Cross-layer relations link by ID, never by "roughly this order" (see [Causality](./runtime-causality)) |
| **A5 Kernel Minimal Knowledge** | Provider/billing/transport semantics stay out of the kernel unless they drive a control decision |
| **A6 Machine-Enforced Constitution** | Every architectural rule ends as a fixture / validator / compile-time exhaustive match / CI gate |

> **Deduplicate authority, not representation.**
> Different representations may exist for different purposes, but there is exactly one semantic authority, and conversion boundaries must be explicit, exhaustive, and tested. The four twin type families (TerminationReason / PaceAction / ToolCall / ResourceQuota) are this principle in practice: the wire version is the ABI authority; the internal version is a richer-vocabulary projection; the only legal crossing is the driver's exhaustive conversion functions.

## Six Legal Forms of Non-Authoritative Representation (A2 vocabulary)

Any representation that is not the authority must register as one of:

| Form | Definition | Permitted operations |
|---|---|---|
| **reference** | Holds only the authority's identity (id/digest/handle) | Dereference, pass |
| **projection** | Read-only view derived from authoritative state | Read; rebuild |
| **cache** | Temporary copy of an authoritative value | Read; invalidate and rebuild; **must self-validate by fingerprint/key** |
| **measurement** | Observation of an authoritative object, with provenance | Read; re-measure |
| **evidence** | Original record of what happened; immutable | Read; archive |
| **mirror** | Serialized projection at the ABI boundary (SDK side) | Encode/decode; **may not introduce local-only semantics** |

**Four violation shapes**: dual authority (two writable homes) / no authority (a fact nothing claims) / authority in the wrong layer (low-layer data carrying high-layer semantics) / implicit authority (association maintained by timing convention instead of fields).

## Message Representation Roles (L1)

`StoredMessageState` (crates/deepstrike-core/src/runtime/kernel/wire/checkpoint.rs) is the L1 durable semantic authority; every other representation has a registered role and may not be used outside it:

| Representation | Registered role |
|---|---|
| `StoredMessageState` / `StoredMessageBody` | **L1 durable authority** (semantic truth in checkpoint/journal) |
| `DurableContent` family | **L1 content vocabulary**: `Text/Image/Audio/Video/File` × `DurableSource{Url, Base64, FileId, Object}` |
| `LogicalMessage` | **Inbound Intent form**: StartOperation initial_context only (no tool_calls) |
| `ProviderMessage` | **Render/fact boundary form**: render output and ProviderCompleted payload |
| `types::Message` / node `Message` | **Legacy internal form** (probation; converges to CoreMessage per roadmap 0.2.67/68) |
| `ContentPart` | **Legacy render-time form** (probation; inline base64 to be removed; large bytes are materialized by the adapter at L0) |

Tool association is a **structural field** (`tool_calls` forward pointers + in-body `tool_call_id` back pointers), not a content part. Reasoning is not in the content vocabulary (article B3).

**The token-number dividing line**: in a checkpoint, a token count is a frozen accounting anchor (legal — a restore must reproduce the same budget arithmetic even if the tokenizer moved); on a runtime message object it must be a TokenMeasurement with fingerprint provenance, or it is a violation.

## Registered Encoding: content-parts-v1

On the wire, `ProviderMessage.content` / `LogicalMessage.content` are `String`; multimodal parts travel via a registered encoding:

```text
content = "[[deepstrike-content-parts]]" + base64url(JSON(parts))
```

This is the definition of registry entry `content-parts-v1`, not a smuggled channel. Encodings must be self-describing and rejectable: **content with an unknown prefix is treated as literal text, never guessed** (B5). First-class wiring (`content: String | Parts`) is reserved for an ABI rev; the trigger conditions are listed in the roadmap §8 — the wire ABI is not upgraded for type aesthetics.

## The Opaque JSON Position (F10 charter)

> **Opaque JSON projections of the ABI on the SDK side (`Record<string, unknown>` / `dict[str, Any]`) are the constitution, not debt: core alone owns the schema and canonical bytes; a typed mirror is a chartered privilege, not a right — every typed field introduced creates local semantics inside that SDK, which must be registered and pinned by parity fixtures.**

## Boundary Invariants (B1–B9)

| # | Article |
|---|---|
| B1 | Vendor raw text never enters kernel state; controlled vocabularies only, `Other` does not pass through |
| B2 | Large-object bytes never cross the kernel: the host store holds bytes, the kernel holds ref+digest+preview |
| B3 | Reasoning is L0 evidence, not L1 Content |
| B4 | Only the two settlement numbers cross into the kernel; full-field measurement stays on the host |
| B5 | Cross-boundary content encodings must be registered; unknown encodings are literal text |
| B6 | Sole authority for message semantics = StoredMessageState; other forms per the role registry |
| B7 | Attempt-level facts (route/usage/evidence/wall-clock) are host-side evidence, never kernel input |
| B8 | Everything crossing the kernel boundary is Intent, Fact, or Decision |
| B9 | The host never declares the caller; identity is derived by the kernel from published surfaces |

## Evolution Discipline

The wire layer is fully `#[serde(deny_unknown_fields)]`, **additive-only evolution**; the ABI version equals the crate version, with no per-record schema_version. New objects must register with six labels (Domain / Authority / Durability / Identity / Causation / Replay); new IDs must register a minting mode; new message forms must enter the role registry. **After promulgation, violations are CI-red, not postmortem findings.**
