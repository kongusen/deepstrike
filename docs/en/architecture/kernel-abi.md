---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [WireEnvelope, KernelInput, KernelEffect, KernelTerminal, KernelTransaction, SyscallRequest]
  python: [RuntimeRunner, KernelJournal, PayloadStore]
---

# Canonical Kernel ABI

The Canonical Kernel ABI is the only stable boundary between the host and the Agent OS kernel. The host owns providers, tools, credentials, filesystems, payloads, and durable I/O. The kernel owns operation lifecycle, effect identity, authority, scheduling, the Context VM, and terminal decisions.

## Envelope

Every input carries correlated operation, input, and observation-time identities. `WireU64` values use decimal strings, and strict unions reject unknown fields and variants.

```json
{
  "abi_version": 3,
  "operation_id": "op-42",
  "input_id": "input-7",
  "observed_at_ms": "1785542400000",
  "input": {
    "kind": "resolve_effect",
    "effect_id": "op-42:step:1:effect:0",
    "outcome": {
      "status": "failed",
      "failure": {
        "kind": "transport_exhausted",
        "message": "provider unavailable",
        "retryable": true
      }
    }
  }
}
```

The kernel accepts only the current revision. It does not negotiate, downgrade, or restore an old operation through an adapter.

## Five Inputs

| Input | Authority | Purpose |
|---|---|---|
| `ConfigureOperation` | host | Install resolved operation config once and create the genesis record |
| `StartOperation` | host | Atomically start `RootEntry::Agent` or `RootEntry::Workflow` with initial context |
| `ResolveEffect` | host executor | Return any effect success or the common typed failure |
| `DeliverExternalEvent` | external | Deliver a signal or child completion with kernel-validated causation |
| `HostControl` | host | Cancel, update deadline/task state, or apply a closed live-policy patch |

`StartOperation` is the only root entry. An agent root first emits `CallProvider`; a workflow root first emits `SpawnTasks`. Session identity stays in the host runner and never enters the wire.

## Effects And Terminals

`KernelEffect` expresses intent that the host must execute idempotently by effect ID and answer through `ResolveEffect`:

- `CallProvider`
- `ExecuteTools`
- `RequestApproval`
- `SpawnTasks` / `PreemptTasks`
- `PersistMemory` / `QueryMemory`
- `ArchivePageOut` / `LoadPayload`
- `EvaluateMilestone`

A terminal is not an effect. A transition disposition contains either effects or one `KernelTerminal`, never both.

## Durable Transition

The host never steps directly:

```text
prepare canonical envelope
  -> append core-owned record bytes with CAS
  -> commit using record digest
  -> publish effects or terminal
```

A record stores the normalized input, previous digest, record digest, and step digest, not a complete step. If commit fails after append, the runner rebuilds from the journal. Abort is valid only before a record becomes durable.

## Checkpoints And Payloads

Restore uses a logical checkpoint plus a bounded journal tail. Checkpoint state has one owner in each transition, P1 syscall, P2 scheduler, and P3 context-VM partition. Candidate, install, acknowledgement, and prefix pruning use explicit CAS and acknowledgement boundaries.

The host persists a large result in `PayloadStore` before core receives an opaque locator, digest, size, and bounded preview. Page-in is correlated to a handle and emits `LoadPayload`; SessionLog is never a payload lookup service.

## Syscall Causation

Model tool calls derive their caller from the pending provider effect. Canonical syscalls cover invoke, spawn, `AppendWorkflowNodes`, memory proposals, and page-in. The host cannot supply an actor, forge a workflow root, or invent a child attempt.

## Further Reading

- [Execution model](./execution-model)
- [Session and restore](./session-replay)
- [Canonical Kernel ABI ADR](/en/decisions/005-canonical-kernel-abi)
