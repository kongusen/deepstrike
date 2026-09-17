---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelInput, KernelEffect, BudgetLedger, TaskId, ResourceQuota, Message]
  python: [RuntimeRunner]
---

# Runtime Authority Matrix & Identity Model

This page promulgates two things: ① the **authority matrix** for the system's core data (one fact, one authority — A1); ② the **four identity-minting modes** and the identity registry (A3). For the layer model and representation vocabulary see [Runtime Data Model](./runtime-data-model).

> Provenance: P1 Authority Matrix and P2 Identity & Causality (2026-09-15, archived in `.local-docs/specs/`). Rulings D1–D5 are final here.

## Four Identity-Minting Modes

There are exactly four legal ways an identity comes into being; every ID must register under one. Anything that fits none is a design error.

| Mode | Meaning | Examples |
|---|---|---|
| **kernel-minted** | Minted by the kernel, only passed by the host; a forged host ID is a protocol error | EffectId / TaskId / AttemptId / HandleId / LaunchToken / SignalId |
| **host-minted, kernel-bound** | Proposed by the host, bound and frozen by the kernel at the first accepted input | OperationId (the only case; immutability after binding is the premise of replayability) |
| **provider-minted, kernel-adopted** | Minted by the provider, adopted (not re-minted) by the kernel — strict pairing requires replay-identical IDs | CallId (uniqueness scope = the containing turn; cross-turn association must go through the effect/task chain) |
| **content-addressed** | The value is the hash of the content; the algorithm prefix `sha256:` is the upgrade channel | Digest / requestFingerprint |

Kernel-side IDs are all branded strings (crates/deepstrike-core/src/runtime/kernel/wire/scalar.rs): non-empty, length-bounded, no control characters, and **rejecting numbers at deserialization** (a JSON number must never be silently promoted to an ID).

## Authority Matrix (main table)

### L0 · Protocol

| Data | Authority | Non-authoritative forms |
|---|---|---|
| Raw wire request/response bytes | Provider (gone once sent); adapter-archived copy | evidence → SessionLog |
| ProviderReplay / ProviderWireEvidence | Host Provider Adapter | evidence → SessionLog |
| Vendor dialect mapping | wire-family normalizer | — |
| Protocol capability declarations | host static vocabulary (node/src/providers/protocol-capabilities.ts) | reference |
| API key / credential | Host CredentialVault | **never inside any fingerprint/journal** |

### L1 · Canonical Semantic

| Data | Authority | Non-authoritative forms |
|---|---|---|
| Message/content semantic truth | StoredMessageState (B6) | projection → render; mirror → SDK |
| Large-object bytes | Host Object/Payload Store | reference → kernel Handle / DurableSource::Object |
| Reasoning traces | Host (L0 evidence, B3) | evidence → ProviderWireEvidence |
| Token measurements | Host TokenMeasurement (fingerprint-keyed side table) | measurement; inside checkpoint = frozen accounting anchor |

### L2 · Execution

| Data | Authority | Non-authoritative forms |
|---|---|---|
| ProviderRequestPlan + fingerprint | Host (sha256 of sanitized plan, node/src/providers/request-plan.ts) | evidence → `prompt_measured` |
| RecordedPromptMeasurement | Host, bound to the request fingerprint (discarded on mismatch) | evidence → SessionLog |
| ProviderUsage / NormalizedProviderUsage | Host per-wire-family normalizer | measurement |
| ResolvedProviderRoute | Host route resolver (content-addressed routeId) | evidence → `run_started.route` / provider_attempt |
| ProviderAttempt | Host execution runtime (primary key = effect_id + attempt_seq) | evidence → SessionLog |
| UsageAccountingPolicy → ModelUsageSettlement | Host accounting policy | two numbers cross → kernel |
| CostObservation / PricingSnapshot | Application/Billing | — |

### L3 · Kernel Control

| Data | Authority | Non-authoritative forms |
|---|---|---|
| Authority source of the five KernelInput kinds | The type is the matrix: `KernelInput::authority()` (crates/deepstrike-core/src/runtime/kernel/wire/envelope.rs) | — |
| effect_id / causation_input_id | kernel-minted | reference → host execution side |
| Task lifecycle / process lineage / WaitSet | kernel | projection → LogicalStateProjection |
| Capability grant/lease | kernel (inside checkpoint) | reference → host enforcement point |
| **Per-operation budget** | kernel BudgetLedger | report → UsageReport events |
| **Cross-operation group budget** | Host GroupLedger (node/src/runtime/run-group.ts) | **one-way delegation edge** (D4) |
| Context VM state | kernel | projection → render |
| Memory record content | Host MemoryRecordStore | receipt → kernel MemoryPersistReceipt |
| Memory write channel (RequestMemoryWrite) | **no model-facing write surface in the kernel** (F15 ruling, 0.2.66: child→parent only — the only caller channel is a child's `parent_requests`) | proposal → host decides and persists |
| StopReason vocabulary | kernel-controlled (`Other` never passes vendor text through) | mirror → host mapping input |

### L4 · Evidence / Persistence

| Data | Authority | Non-authoritative forms |
|---|---|---|
| Journal record bytes + digest chain | **core** (a host recomputing hashes is an unreachable state) | CAS storage primitive → host journal implementations |
| Journal head | host storage-layer CAS | — |
| Checkpoint content | kernel (triple digest chain) | storage → host |
| Config truth (after pruning) | genesis record → acked checkpoint's resolved_config | — |
| Business event history | Host SessionLog (independent seq space) | mirror → four SDKs (vocabulary gate) |

## Rulings (D1–D5, final)

**D1 · ProviderWireEvidence.** Authority = Host Provider Adapter; **field-level interlink** with `ProviderRequestPlan.fingerprint` (request_fingerprint is mandatory), response_id archived, raw_usage preserved verbatim under a BoundedJson cap. No implicit association by "appearing in the same turn".

**D2 · Kernel minimal knowledge of usage is constitutional.** The kernel consumes only the two comparable settlement numbers (observed_input_tokens / observed_output_tokens); cache/reasoning/billing semantics are converted by the host accounting policy. The fewer vendor semantics enter the journal, the stronger replay determinism.

**D3 · Execution-layer objects belong to the host.** ResolvedProviderRoute / ProviderAttempt / InvocationOutcome have authority = Host Execution Runtime with zero new kernel-side fields. **The kernel chain is not extended; the host chain continues from effect_id.** A route pins to an attempt, not to an effect. The `Attempt` name has two axes: harness AttemptLoop = quality attempt; ProviderAttempt = transport attempt.

**D4 · Two-layer budget = two levels + one one-way edge.** Cross-operation capacity pool authority = Host GroupLedger; per-operation execution authority = kernel BudgetLedger. The only legal flows: **host→kernel at configure (delegated grant); kernel→host at settlement (fact return)**. Mid-run budget changes go through the `HostControl.TightenResourceQuota` control plane only.

**D5 · Reasoning = L0 evidence, never L1 Content.** Three wire shapes, different cross-turn retention rules, different signature requirements — all vendor semantics; canonicalizing would drag them into L1. Replay needs the original blocks, not a canonical form.

## Projection-Pair Discipline (twin type families)

TerminationReason / PaceAction / ToolCall / ResourceQuota: the wire version is the ABI authority; the internal version is a legal projection with a deliberately richer vocabulary (internal TerminationReason has 9 variants vs wire 7 — UserAbort/Error map to **different terminal kinds**, not into the termination vocabulary). Discipline: ① the only legal crossing is the driver's exhaustive conversion functions (crates/deepstrike-core/src/runtime/kernel/wire/driver.rs); ② importing both sides requires an alias stating the authority direction (`TerminationReason as WireTermination`); ③ adding an internal variant forces the conversion seam to update via the compiler's exhaustive match; adding an ABI variant goes through the ABI rev process.
