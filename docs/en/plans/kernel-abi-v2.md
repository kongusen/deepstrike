# Implementation Plan: Kernel ABI v2 Reliability

## Status

Completed (2026-07-15)

## Basis

[ADR-002](../decisions/002-kernel-abi-reliability) selects a direct ABI v2 cutover with no v1 adapter, shim, or dual path.

## Approach

Treat the Rust core wire contract as the single source of truth and cut over one vertical reliability capability at a time. Each phase first fixes a failing ABI/state contract, then changes core, and finally switches Node/Python consumers and replay representations in that phase.

```text
operation_id (start_run)
        │
        ├── delivery_id ──► signal disposition ──► host ack/nack
        ├── reservation_id ► budget usage        ──► host settle/release
        └── cancel reason ─► terminal state       ──► replay/audit
```

Hosts continue to own external atomicity and I/O. Core owns identity validation, deterministic state transitions, usage counters, and observations.

## Architecture Decisions

### 1. The version gate cuts off v1 once

- Set `KERNEL_ABI_VERSION` directly to `2`.
- Return a stable version mismatch for `KernelInput.version != 2` before treating it as a v2 event.
- v2 fixtures define the only supported inputs and outputs; v1 fixtures remain useful only for rejection tests.
- Switch Node/Python public constants, types, and tests to 2 in the same slice.

### 2. Bind operation identity at run start

- Require `operation_id`, `event_id`, and `observed_at_ms` on every input envelope; `start_run` binds the operation.
- Carry `step_seq` and `input_event_id` on each step; correlate every action/result by `effect_id`.
- Store immutable identity for the run lifecycle in `KernelRuntime`/the state machine.
- Require `deliver_signal`, budget observations, and `cancel_operation` to match current identity.
- Missing, pre-start, or conflicting identity fails closed and emits no business action.

### 2A. Action/result is the only host side-effect protocol

- Provider/tool/milestone/workflow/preemption/approval/memory/spool/page-out all emit actions carrying `effect_id`.
- Core emits success/failure observations only after host effect-result feedback.
- Duplicate results are idempotent; unknown or conflicting results produce `KernelFault`.

### 2B. KernelFault, lifecycle, and state encapsulation

- Version, identity, ordering, configuration, replay, and snapshot errors use structured faults rather than `ToolGated`.
- Lifecycle is Created, Configured, Running, Suspended, terminal; configuration applies atomically and workflows do not auto-start.
- Remove public `state_machine_mut()`; bindings use stable projection/command APIs only.

### 3. Keep only the delivery-aware signal path

- Remove `KernelInputEvent::Signal`; add `DeliverSignal`.
- Remove `SignalDisposed`; add `SignalDeliveryDisposed`.
- Reuse the existing router/attention queue and dedupe internals while correlating every disposition with operation and delivery IDs.
- Change session events, event categories, OS snapshots, and Node/Python mappings to the new name and shape together.

### 4. Group budgets accept only explicit grants

- Remove `group_tokens_base`, `group_spawns_base`, `group_rounds_base`, and their seed methods.
- `BudgetGrant` holds reservation identity and granted capacity per axis; local counters compare directly with the grant.
- Emit `BudgetUsageReported` exactly once at terminal; correlate `BudgetExceeded` with operation and reservation IDs.
- Require `ReservableGroupBudgetStore` for Node/Python RunGroup execution and remove the accounting fallback.
- Standalone runs continue to use local scheduler/resource quotas without fabricating a reservation.

### 5. Cancellation is a dedicated state transition

- Remove the general-purpose host `timeout` input and add `CancelOperation`.
- Add a closed `CancellationReason`: `user`, `deadline`, `lease_lost`, `host_shutdown`.
- Move Reason, ToolAwait, SubAgentAwait, Workflow, and other running/waiting phases through one cancelled terminal path.
- Repeating the same cancellation is a no-op; a different operation ID or inconsistent repeat fails closed.
- Internal scheduler wall-time exhaustion may still terminate as `Timeout`, but hosts cannot use it as cancellation.

### 6. Replay treats observations as facts

- Snapshot/audit folds record operation, delivery, reservation, and cancellation correlation.
- Never persist delivery lease tokens, store revisions, AbortSignal/CancelScope values, or external handles.
- Differential tests prove equivalent next actions/observations after uninterrupted and restored execution.

## Phases and Dependencies

### Phase A: ABI v2 foundation

First split `kernel.rs` protocol/runtime/tests without behavior change, then establish the version gate, uniform input/step/effect envelope, KernelFault, lifecycle, projection API, and migrate all StartRun callers to v2.

Dependency: none.

Checkpoint: core and Rust integration tests pass; v1 rejection, v2 round-trip, duplicate/conflicting events, invalid lifecycle, and the absence of a mutable escape hatch are explicit.

### Phase A2: Effect protocol

Replace observation-driven host commands with action/results: approval/workflow/preemption first, then memory/spool/page-out. Record facts only after success/failure results.

Dependency: Phase A.

Checkpoint: every host side effect has a stable effect ID, duplicate results do not repeat transitions, and source contains no observation-triggered unfinished I/O path.

### Phase B: Delivery-aware signal

Replace signal input/observation across the state machine, session event, event log, and OS snapshot, then switch Node/Python signal gateways and tests.

Dependency: Phase A.

Checkpoint: accepted/queued/deduped/dropped dispositions echo the delivery ID, redelivery attempts remain distinct, and no public legacy signal path remains.

### Phase C: Reservation-backed budget grant

Replace base budgets, enforce grants directly, emit usage, and remove the Node/Python RunGroup accounting fallback in favor of reservation-correlated settlement.

Dependency: Phase A; implementation can proceed independently after Phase B interfaces stabilize.

Checkpoint: token/subagent/round boundaries, unique terminal usage, settle/release correlation, and concurrent reservation tests pass.

### Phase D: Operation cancellation

Add cancellation input/reason/observation, unify phase transitions, and map Node AbortSignal and Python CancelScope/CancelledError into the v2 event.

Dependency: Phase A; terminal usage semantics depend on Phase C.

Checkpoint: all phases, repeated cancellation, conflicting identity, deadline, and lease-lost scenarios pass; host timeout/critical signals no longer emulate cancellation.

### Phase E: Replay and residual cleanup

Complete `KernelSnapshot`, session replay/OS audit snapshot differential fixtures, bounded event/signal dedupe, remove v1/base/signal/observation-command/accounting-fallback residue, and finish cross-language verification and documentation.

Dependencies: Phases B, C, and D.

Checkpoint: full Rust/Node/Python/docs verification passes, source searches find no old public contract, and core gains no I/O.

## Verification Strategy

Each phase follows RED → GREEN → REFACTOR:

1. Add the smallest failing wire/state test and confirm it fails for the missing target behavior.
2. Implement the minimum deterministic core path.
3. Switch the Node/Python binding and host consumer for that path.
4. Run focused tests, then the affected language build/test.
5. Run a complete workspace checkpoint after every two phases.

Final commands come from ADR-002, plus this residual scan:

```bash
rg -n "group_tokens_base|group_spawns_base|group_rounds_base|signal_disposed|kind: ['\"]signal['\"]" crates node python tests
```

Expected result: no public/runtime source match outside migration/ADR history.

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Rust enum/struct breakage exceeds JSON ABI breakage | High | Compile the full workspace first and inventory consumers from compiler errors; add no temporary compatibility variant |
| Operation identity binds twice during restore | High | Restore the same ID explicitly and fail closed on conflicting StartRun |
| Duplicate terminal usage causes duplicate settlement | High | Maintain a kernel terminal-report latch; keep store settlement idempotent |
| Cancellation races a completed action | High | Input order determines one result; differential-test completion-before-cancel and cancel-before-completion |
| Accounting-only custom stores fail immediately | Medium | Produce a clear compile/startup error and publish only the reservable contract; never silently degrade |
| Node/Python wire shapes drift | Medium | Share golden JSON and test binding round-trip/parity separately |

## Plan Gate

After this plan is approved, the next phase will create executable tasks, each touching roughly five files or fewer with explicit Acceptance, Verify, and Dependencies. No kernel implementation changes occur before that task list is reviewed.
