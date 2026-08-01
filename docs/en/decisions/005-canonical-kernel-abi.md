# ADR-005: Canonical Kernel ABI

## Status

Accepted

## Date

2026-07-29

## Context

The kernel ABI has two historical layers. v1 established inputs, actions, observations, and cross-language driving. v2 (ADR-002) added operation, event, and effect identity; strict lifecycle; structured faults; delivery-aware signals; reservation budgets; typed cancellation; and prepare/commit. Because v2 grew incrementally, it still exposed overlapping configuration paths, privileged workflow completion, several effect-result entry points, `Done` as a non-executable effect, public direct step beside durable transitions, full-journal snapshots containing a complete `KernelStep`, and host session, path, vendor-error, and `now_ms` data in core. A source audit against v0.2.50 (`23db0ed`) produced the DEC-1..9 adjudications and the pre-migration hardening list. This ADR freezes the resulting contract.

## Decision

### 1. The runtime has one Canonical Kernel ABI

The runtime does not choose between v1 and v2 and does not support both. Runtime 0.2.51 provides no old-ABI adapter, negotiation, shape-based version detection, or SDK fallback. `KERNEL_ABI_VERSION = 3` is a fail-closed wire gate, not a product name. Unknown revisions, fields, variants, effect identities, causation, lifecycle transitions, or digests are rejected without advancing the step sequence, mutating lifecycle, or publishing effects. An unfinished old-ABI operation cannot resume after upgrade; deployments that must resume one remain on 0.2.50.

### 2. Five top-level inputs reduce to P1/P2/P3

`KernelInput` contains only `ConfigureOperation`, `StartOperation`, `ResolveEffect`, `DeliverExternalEvent`, and `HostControl`. These classes encode authority and lifecycle and follow separate validation paths. Every variant explicitly declares its authority/lifecycle pair and reduces to a P1 syscall, P2 task-table transition, or P3 context-VM transition. Inputs that cannot reduce to those primitives do not enter the ABI. There is no free-form `AgentRequest { actor_id }`, caller-supplied workflow authority, host-issued skill activation, or SDK-forged tool result.

### 3. Root start is atomic

The only root entry is `StartOperation { entry: RootEntry::Agent | RootEntry::Workflow, initial_context }`. An agent root directly publishes `CallProvider`; a workflow root directly creates its DAG and publishes `SpawnTasks`. The old workflow `LoadWorkflow` and privileged `CompleteRun` path are removed, and root workflow completion commits terminal in core. `RootKind` is immutable while `ExecutionFocus` may move between an agent turn and a nested workflow controller. `InitialContext` and `LogicalAgentSpec` are independent wire DTOs and contain no host session or path fields.

### 4. One durable transition protocol

Production transitions use only `prepare -> journal CAS append -> commit`. Core enforces bootstrap byte/depth limits before decode, then normalizes, validates, and plans. The host appends core-produced record bytes with compare-and-append, and only a successful commit publishes effects, observations, or terminal. `abort` is legal only before append. Any failure after append discards the runtime and rebuilds it from the journal. A CAS conflict closes the loop through abort, head reload, rebuild, and byte-identical input retry. Bindings expose no production direct step.

### 5. Core owns effect lifecycle

Every host effect completes through `ResolveEffect { effect_id, outcome }`; `EffectOutcome::Succeeded | Failed` is the only result entry. Approval, milestone, memory, payload, and task control all have a uniform host-failure path. Before `SpawnTasks` is committed, core fixes `task_id`, `attempt_id`, and `launch_token`; attempts become Running only after spawn acknowledgement. Core does not redispatch automatically. Unsupported effects fail with `ProtocolError`, and configured host-effect support prevents an unsupported effect from being emitted. Vendor raw errors remain host diagnostics rather than kernel recovery inputs.

### 6. Terminal is not an effect

Terminal is committed state, requires no resolution, and has no effect ID. Its observation and single usage report commit in the same transition. After terminal, every new state-changing input, including signal delivery, is rejected without a journal record or queue mutation. A redelivery of an already accepted input returns the same result.

### 7. Agent authority is derived from causation

Provider tool calls enter P1 from the pending provider effect and call identity. Child requests attach to `ChildCompleted(task_id, attempt_id)`, from which core derives the caller. Memory inputs are proposals and cannot provide tenant/namespace, record ID, trust, timestamp, or session provenance. Core combines the operation's opaque `MemoryAccessBinding`, accepted envelope time, and causation to author the final memory effect. A quarantined task cannot widen workflow, memory, or capability authority, and an agent syscall cannot impersonate a host root start.

### 8. External payload and page-in/out live in P3

The host persists an oversized tool result before submitting `External`. Core receives only an opaque payload locator, digest, original size, and bounded preview, then associates inline/external results with P3 handles and residency. `read_result` reduces to `SyscallRequest::PageIn { handle_id }` and can publish only the correlated `LoadPayload` effect. `SpoolLargeResult`, its result/observation, and SessionLog payload scans are removed. A `PayloadRef` is opaque and is never interpreted as a path.

Old `spool_ref` data cannot be losslessly relabeled because it lacks canonical digest and original size. A host that still has the body must read it, compute SHA-256 and UTF-8 byte length, persist it under a new `PayloadStore` locator, and construct a complete `External` descriptor. Missing or unverifiable bodies invalidate the handle and produce typed `StorageUnavailable` on page-in.

### 9. Recovery uses logical checkpoint plus bounded tail

`LogicalKernelState` partitions ownership across transition, P1 syscall, P2 scheduler, and P3 context VM state. Checkpoints do not serialize private state-machine layout, `RenderedContext`, or a complete `KernelStep`. Installation uses candidate, host persistence, covered-head CAS install, and acknowledgement. Appends after candidate remain in the tail; install need not cover the current transaction head; prefix reclamation waits for acknowledgement. Record and byte hard limits return retryable `CheckpointRequired` before accepting the input. Full accepted-input snapshots, generic `Resume`, workflow `resumed_*`, and SessionLog workflow reconstruction are removed. Checkpoints have their own `KERNEL_CHECKPOINT_VERSION`.

### 10. Cross-language wire discipline has one record implementation

The wire fixes `WireU64` as decimal strings, strict tagged unions, unknown-field rejection, fixed-point policy, finite observation floats, and canonical bytes. Core alone creates canonical input bytes, record digests, and the chain. Rust, Node, Python, and WASM consume those bytes instead of reimplementing the hash contract. Host session/tenant/user identity, paths, vendor errors, async handles, and executor retry/backoff do not enter core wire.

## Relationship to prior decisions

- ADR-001's operation identity, required mutation, observer separation, and budget reservation remain in force.
- ADR-002 remains the history of superseded ABI; ADR-005 supersedes its concrete wire shape while retaining its reliability principles.
- ADR-003's rule that public reliability settings need host resource semantics remains in force; host retry, path, and provider configuration move further out of core.
- ADR-004's single transaction path, external payload, and logical checkpoint plus bounded tail are required here. Its transaction order is corrected to `normalize -> validate -> plan/prepare -> durable journal CAS append -> commit/publish`.
- The P1/P2/P3 ownership model in the Agent OS three-primitives specification remains upstream of this ABI. SDK API shape cannot define kernel authority or lifecycle.

## Delivery order

Implementation proceeds through the phased tasks and Checkpoints A-F in the canonical ABI specification: hardening and contract freeze; protocol types; record chain and transaction; root authority; unified effects and payloads; logical checkpoints; four-language host cutover; then SDK API, documentation, and the breaking release. Each checkpoint keeps the workspace buildable and adds executable contract coverage before migration expands.

## Non-goals

- No adapter, shim, negotiation, or deprecation window for legacy ABI revisions.
- Core performs no network, file, database, provider, tool, or sub-agent I/O and stores no API keys, lease tokens, encryption keys, paths, or executable handles.
- Core does not own host retry, backoff, physical cancellation, payload encryption, or retention.
- This ADR does not extend terminal semantics based on `CancellationReason`.
- SDK `run`/`runWorkflow` surface names do not define kernel authority or lifecycle.

## Sources

The authoritative implementation specification is `.local-docs/.local_spc/canonical-kernel-abi.md`, with audit and adjudication records under `.local-docs/.local_spc/audit/`. These local working documents are excluded from the repository; this ADR is the repository-facing decision record.
