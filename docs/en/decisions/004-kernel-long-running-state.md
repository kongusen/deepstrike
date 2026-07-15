# ADR-004: Long-running kernel state and external result protocol

## Status

Accepted

## Date

2026-07-15

## Context

ABI v2 moved operation, event, effect, signal, budget, cancellation, and portable replay semantics into the kernel. A full input journal still makes restore cost linear in total run length, while large tool outputs cross the ABI and enter the journal before the host spools them. Count limits protect memory but do not provide sustainable checkpoints for long-running operations.

## Decision

### 1. One transaction path

Kernel event handling converges on `normalize -> validate -> plan -> commit -> journal`. Runtime lifecycle, pending effects, step sequence, actions, and observations commit from one transition plan. Faults commit no runtime-owned state. The state machine does not expose a second lifecycle adjudication path.

### 2. Tool results are inline or external

`ToolResults` ultimately accepts one closed union:

- `inline`: the body of a small result;
- `external`: `blob_ref`, digest, original size, and preview.

The SDK atomically persists an external body before submitting the input, and the kernel validates the payload against configured policy. The old `SpoolLargeResult` action/result, pending kind, and retry branch are deleted rather than retained as a compatibility path. Files, object storage, encryption, and secrets remain host-owned.

### 3. Logical checkpoints with a bounded tail

The new checkpoint schema is independent of private `LoopStateMachine` layout. It contains a versioned logical-state DTO, base step, bounded tail inputs, pending effects, replay metadata, resource policy, and state/tail digests. Restore installs logical state and replays only the tail; a successful checkpoint rebases the journal, so restore complexity depends on tail length rather than total run length.

The new format directly replaces the full-journal snapshot. Hosts persist the checkpoint as opaque data. Host signatures and encryption provide authenticity and confidentiality; kernel digests detect corruption and inconsistent state.

### 4. Count and byte resource boundaries

SDKs configure only limits with host resource semantics. `max_input_bytes`, `snapshot_input_limit`, `snapshot_journal_bytes_limit`, and read-only `KernelDiagnostics` are implemented first. The bounded checkpoint tail will reuse these byte watermarks; container capacities remain implementation details.

## Delivery order

1. Restore hot path and byte diagnostics;
2. transition plan and one lifecycle source;
3. external tool-result payload and removal of spool legacy;
4. logical checkpoint, bounded tail, and digests;
5. incremental render caching only after end-to-end benchmarks prove value.

Each step is tested, verified, and committed independently. Wire payload and snapshot schema are not rewritten in one unreviewable commit.

## Consequences

- Long runs no longer permanently lose checkpoint capability at a fixed journal count;
- large result bodies do not enter the kernel journal or portable checkpoint;
- fault and panic boundaries do not leave runtime-owned metadata half committed;
- SDKs retain one result and checkpoint protocol;
- the checkpoint schema requires independent golden, cross-SDK parity, and uninterrupted/restore differential tests.

## Non-goals

- Core performs no file, network, database, or object-store I/O;
- core stores no API keys, lease tokens, encryption keys, or executable handles;
- no adapter is provided for the old spool or full-journal snapshot;
- recovery and render performance do not use unbounded caches.
