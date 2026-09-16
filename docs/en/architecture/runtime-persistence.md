---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelInput, KernelTransaction, PlannedStep, SessionEvent]
  python: [KernelJournal]
---

# Runtime Persistence Contract (Journal / Checkpoint / SessionLog)

The L4 constitution: the three stores and their three truths, per-store articles (J1–J7 / K1–K6 / S1–S4), cross-store invariants (P-1–P-5), and the error taxonomy. The three roles are **not interchangeable**.

> Provenance: P6 Persistence Contract (2026-09-15, archived at `.local-docs/specs/runtime-data-model-p6-persistence-contract-2026-09-15.md`). Article numbers correspond one-to-one with chain-validator rules C (see [Causality](./runtime-causality)).

## Three Stores, Three Truths

| Store | Truth role | Question answered | Authority | Number space |
|---|---|---|---|---|
| **Journal** | **State Truth** | "In what order did the operation make which rulings" | core (record bytes + digest); host holds CAS storage | `step_seq` (chain position) |
| **Checkpoint** | **State Snapshot** | "What is the logical state as of through_step_seq" | kernel (triple digest + explicit projection); host holds install/ack | anchored to `step_seq` boundaries |
| **SessionLog** | **Evidence Truth** | "What did the world see" | host | `seq` (**independent**, S2) |

One fact has at most one authoritative home across the three stores. Cross-store references travel only through registered identity fields (operation_id / effect_id / digest) — **timing alignment is never an association** (P-4).

## Journal Contract (J1–J7)

**J1 · Core is the sole implementation of bytes and hashes.** Records are built by core with private read-only fields; the host stores bytes verbatim and indexes by the digest core provides. "The host recomputed a hash and found a mismatch" is an unreachable state — a journal implementation that re-serializes records to recompute hashes is in violation. Decode re-validates: a tampered entry fails at the boundary, not deep inside replay.

**J2 · CAS is a storage-layer primitive.** A genuinely atomic operation (file lock / `O_EXCL` chained naming / conditional database update), not a read-compare-write sequence.

**J3 · step_seq and SessionLog seq are two independent number spaces.** Pruning a journal prefix never punches holes in business-event numbering.

**J4 · A durable record never carries a planned step.** A record stores the normalized input + `step_digest`; rebuild = re-run the deterministic transition on the canonical input and compare digests. **Premise of legality: re-derive determinism** (mixed-batch articles M3/M4, see [Causality](./runtime-causality)) — any kernel change breaking re-plan determinism breaks J4 and validator rule C3 together. Record size is a function of the input, never of the step it produced.

**J5 · The chain is the operation's identity.** The genesis record binds `ResolvedOperationConfig` (not the sparse config, not the binary's defaults); genesis has no previous digest, and its record_digest is the operation's genesis_digest.

**J6 · Seven fields**: `operation_id / input_id / step_seq / previous_record_digest / canonical_input / input_digest / step_digest / record_digest`. One more field is an ABI change; one fewer is a broken chain.

**J7 · The error taxonomy is part of the contract and never collapses into one opaque Error:** `JournalCasConflictError` (**retryable**: abort → re-read head → rebuild → replay) / `JournalIntegrityError` (**never retryable** — retrying replays the same contradiction) / storage corruption. **The durable-step wrapper publishes no effect on any of the three.**

## Checkpoint Contract (K1–K6)

**K1 · A checkpoint is a canonical DTO of independent contractual shape, not an internal kernel snapshot.** LogicalKernelState is built by explicit projection — adding a field to the state machine must not silently change the checkpoint format, and a field the DTO needs must not silently vanish.

**K2 · Every piece of correctness state has exactly one home** (the four partitions transition / syscall / scheduler / context_vm do not overlap). Pending effects, the replay ledger, and the terminal live in transition; task attempts in scheduler; handles in context_vm; the header duplicates none of them. The `single_ownership_is_structural` test scans the serialized document to prove this — structure as proof.

**K3 · The bounded tail is exact**: `tail_inputs` covers `(base_step_seq, through_step_seq]` — no holes, no duplicates, no overrun, **validated at construction** — a checkpoint that would replay a different history cannot be constructed.

**K4 · Triple digests answer three questions**: `state_digest` (is this the captured logical state) / `tail_digest` (is this the captured tail) / `checkpoint_digest` (is the whole, header included, this). All reuse the record layer's canonical bytes — a host verifier needs no second serializer.

**K5 · Install/reclaim rules**: covered_head is validated **at the install point** (not the current head); **an ack is not a KernelInput** (a runtime-maintained handle, no record written); the prefix an ack may reclaim = `boundary(){through_step_seq, covered_head}`; the replay/dedupe ledger (accepted_inputs) is **never emptied by an ack** — redelivery below base gets an **idempotent confirmation, not a step replay**.

**K6 · The config truth chain**: bound by the genesis record → continued by the acked checkpoint's resolved_config. After pruning, config-truth authority migrates; the migration point is the ack.

> **Implementation status (closed in 0.2.65)**: the install/restore/rebase/ack paths have all landed — kernel-side `checkpoint_candidate`/`checkpoint_rebase` generation, `KernelCheckpoint::assemble/verify` install-point validation (covered_head against the install point), the `restore_from_checkpoint`/`restore_operation` dual ladders, and `note_checkpoint_acked` reclaim accounting (replay/dedupe ledger untouched, per K5); host-side, all four SDKs' `canonical_kernel_step.checkpoint()` run compareAndInstallCheckpoint → journal+kernel dual ack → pruneAckedPrefix (node canonical-kernel-step.ts, core transaction.rs/checkpoint.rs). The durable plane FileKernelJournal (node+python) does cross-process atomic CAS; restart-recovery e2e is green across all four SDKs (0.2.65 S2).

## SessionLog Contract (S1–S4)

**S1 · Evidence Truth, append-only.** Events are the original record of what happened; they are added, never edited.

**S2 · Independent number space** (the other face of J3). `seq` belongs to SessionLog itself; journal pruning punches no holes.

**S3 · The vocabulary is host-owned; mirrors must register.** A new event kind must land in all four SDK vocabularies in the same commit, or it is a violation (vocabulary manifest gate).

**S4 · The cross-store join key is `effect_id`, never `step_seq`.** When old logs lack the fields, the validator degrades with an annotation (C7) instead of failing.

## Cross-Store Invariants (P-1–P-5)

| # | Article |
|---|---|
| P-1 | One operation = one journal chain; genesis_digest is its identity |
| P-2 | The recovery ladder has exactly three rungs, all idempotent: journal-prefix replay → checkpoint+tail rebuild → idempotent confirmation below base |
| P-3 | No store's error taxonomy may collapse; the durable-step wrapper never publishes effects on journal errors |
| P-4 | The three stores interlink only through identity fields; timing alignment is never an association |
| P-5 | Pruning happens only after an ack, reclaims only the prefix declared by boundary(), and never touches the replay ledger |

## Storage Implementation Reference

Node reference implementation: node/src/runtime/kernel-journal.ts (File/InMemory variants; the three hard rules live in the module header). The four SDKs keep their ABI projections opaque (F10 charter, see [Data Model](./runtime-data-model)).
