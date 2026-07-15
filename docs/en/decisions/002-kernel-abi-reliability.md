# ADR-002: Kernel ABI Reliability Slice

## Status

Accepted

## Date

2026-07-14

## Assumptions

1. This slice is a stacked branch based on `codex/runtime-contract-refactor`; it is reviewed separately but depends on the operation-scope, delivery-lease, and budget-reservation contracts from that slice.
2. This is an explicit breaking upgrade: `KERNEL_ABI_VERSION` becomes `2`, and ABI v1 inputs are no longer accepted.
3. Rust core owns deterministic state transitions, adjudication, correlation, and usage accounting. Persistence, distributed atomicity, and real I/O cancellation remain host responsibilities.
4. `group_*_base`, legacy `signal`, `signal_disposed`, and the accounting-only group-budget fallback are removed directly; no dual path remains.
5. Node, Python, and WASM are all required host targets for the cutover and validation; each exposes only the ABI v2 high-level API.

If these assumptions change, update this ADR before implementation planning.

## Objective

Complete the kernel side of the SDK reliability contracts so hosts no longer infer these facts from implicit conventions:

- which kernel decision completed a leased signal delivery;
- which budget reservation a run consumed and whether it crossed its grant;
- which deterministic terminal state operation cancellation produces in each loop phase;
- whether those correlations survive JSON round-trip, session logs, and snapshot/replay.

The resulting boundary is: the host performs external `claim/reserve/cancel-I/O` work first and submits the resulting facts to core; core adjudicates them and emits correlated observations that the host uses to `ack/nack/settle/release`.

## Current Problems

### Signal delivery has business identity but no delivery identity

`RuntimeSignal.id` identifies the signal and `dedupe_key` drives kernel queue deduplication, while a durable host delivery lease has a separate lifecycle. On redelivery, `SignalDisposed` exposes only `signal_id`, so a host cannot prove which claim the disposition completed.

### Budget reservations are expressed indirectly through cumulative bases

`RunConfig.group_tokens_base`, `group_spawns_base`, and `group_rounds_base` inject usage and held capacity from other members; global limits then subtract those bases. This limits aggregate consumption but does not express this run's grant or correlate terminal usage with a reservation. Settlement depends on host side-channel state.

### Cancellation has no unified kernel semantic

The ABI has timeout, critical signal, provider error, and process preemption paths, but no operation-cancellation input. After cancelling provider/tool/sub-agent I/O, a host cannot consistently map `user`, `deadline`, `lease_lost`, or `host_shutdown` to a replayable kernel terminal state.

### Replay records decisions without complete reliability correlation

`SignalDisposed` and `BudgetExceeded` can enter session events and OS snapshots, but delivery, reservation, and operation correlations are absent. Recovery can see that a decision happened without proving which external reliability transaction it belongs to.

## Decision

### 1. ABI v2 carries uniform event, step, and effect identity

New ABI values carry only immutable identifiers and pure data:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetGrant {
    pub reservation_id: String,
    pub tokens: Option<u64>,
    pub subagents: Option<u32>,
    pub rounds: Option<u32>,
}
```

Wire names use `snake_case`. IDs are opaque strings; core never parses storage backends, lease versions, or credentials.

Every `KernelInput` envelope carries `operation_id`, `event_id`, and `observed_at_ms`; `KernelStep` carries `step_seq` and `input_event_id`; every I/O-requiring `KernelAction` carries `effect_id` and `causation_id`. Provider/tool/milestone/sub-agent results must reference the original `effect_id`.

`start_run` binds the current operation identity. Missing, conflicting replay, or cross-operation input/results are rejected at the ABI boundary and never inferred from mutable session/run state. An identical replay with the same `event_id` is an idempotent no-op; the same ID with a different payload is a fault.

### 2. Add a distinct input/output pair for leased signal delivery

Remove legacy `signal` and use only `deliver_signal` with `signal`, `delivery_id`, and required `operation_id`. Even an in-memory host queue must generate a stable identity for every delivery attempt.

For the new input, core emits `signal_delivery_disposed` with at least:

- `signal_id`
- `delivery_id`
- `operation_id`
- `disposition`
- `queue_depth`

Remove `signal_disposed` and emit only `signal_delivery_disposed`. Core never executes `ack/nack` and never stores lease tokens.

### 3. Make granted capacity a first-class RunConfig value

Remove `RunConfig.group_tokens_base`, `group_spawns_base`, and `group_rounds_base`; `budget_grant` becomes the only cumulative-budget input for a run group. A standalone run may omit it and use local `ResourceQuota`; a run that joins a RunGroup must supply a reservation-backed grant.

- token, subagent, and round cumulative axes limit this run's local consumption directly;
- the terminal step emits `budget_usage_reported` with `reservation_id` and actual local usage;
- `budget_exceeded` carries `operation_id` and the corresponding `reservation_id`.

RunGroup execution accepts only stores with `reserve -> settle | release` and no longer falls back to `read -> charge` accounting mode. Core still never reads the shared ledger.

### 4. Introduce one operation-cancellation event

Add `cancel_operation`:

```json
{
  "kind": "cancel_operation",
  "operation_id": "op-123",
  "reason": "lease_lost",
  "pending_call_ids": ["tool-7"]
}
```

`reason` is a closed enum: `user`, `deadline`, `lease_lost`, `host_shutdown`. Core performs an idempotent termination from every running/waiting phase, emits one `operation_cancelled` observation, and returns a deterministic terminal/await action. Repeating the same cancellation must not create a second terminal state; a conflicting operation ID fails closed.

The host first cancels real provider/tool/sub-agent work and supplies known pending call IDs as facts. Core owns no threads, futures, process handles, or network connections.

### 5. Actions are commands; observations record facts only

Every output that requires host work is a `KernelAction`, including provider/tool/milestone, workflow spawn, sub-agent preemption, approval request, memory persistence/query, result spool, and page-out archive. The host feeds back a result carrying `effect_id`; core emits the corresponding observation only after consuming success or failure.

`MemoryWritten`, `MemoryQueried`, `WorkflowBatchSpawned`, `AgentPreempted`, and similar observations may no longer trigger host side effects that have not happened yet.

### 6. Use KernelFault, a strict lifecycle, and a closed state boundary

ABI rejection is no longer disguised as `ToolGated`. Structured `KernelFault` codes include `version_mismatch`, `operation_mismatch`, `invalid_lifecycle`, `invalid_config`, `duplicate_event_conflict`, `unexpected_effect_result`, and `snapshot_incompatible`.

The lifecycle is `Created -> Configured -> Running <-> Suspended -> Completed|Cancelled|Failed`. Configuration validates as a whole before atomic application; workflows no longer auto-start; terminal kernels reject business mutation.

Remove public `state_machine_mut()` and host dependencies on internal structs. Replace them with read-only projections and explicit commands for status, turn, rendered context, committed-message drain, local usage, and snapshot.

### 7. Reliability correlation participates in serialization and replay

New inputs, observations, and session events must:

- round-trip through JSON without losing fields;
- reject ABI v1 fixtures with a version mismatch and make ABI v2 fixtures the only wire contract;
- let OS snapshots rebuild audit records by delivery/reservation/operation ID;
- produce equivalent next actions/observations after snapshot/restore and uninterrupted execution;
- never persist lease tokens, API keys, paths, or host cancellation handles.

The current `OsSnapshot` remains an audit projection. A stable `KernelSnapshotV2` restores real run state, event/effect replay windows, and the terminal-report latch without directly serializing the internal state-machine struct.

### 8. Bound state and optimize only from measurements

Signal delivery/event dedupe uses a fixed-capacity replay window instead of an unbounded `HashSet`; audit snapshot detail uses bounded windows or aggregate counters. Clone/allocation work in render, compression, and replay requires benchmarks before optimization.

### 9. Migrate through vertical sub-slices

The implementation order is fixed:

1. ABI module split, v2 envelope/version gate, KernelFault, lifecycle, and state encapsulation;
2. effect protocol and Action/Observation separation;
3. signal delivery disposition and bounded dedupe;
4. budget grant enforcement and usage report;
5. operation cancellation state transition;
6. snapshot/replay/golden hardening and Node/Python host cutover;
7. removal of ABI v1, legacy signal, base-budget, observation-command, and SDK fallback residue.

Each sub-slice starts with a failing contract test, then adds the minimum implementation, and leaves the workspace buildable.

## Tech Stack

- Rust 2024, Serde JSON, `deepstrike-core`
- napi-rs Node binding (`crates/deepstrike-node`)
- PyO3 Python binding (`crates/deepstrike-py`)
- Rust unit/integration tests, Jest, Pytest
- VitePress documentation and docs drift checker

## Commands

```bash
cargo test -p deepstrike-core
cargo test -p deepstrike-tests t12_golden_fixtures
cargo test --workspace --exclude deepstrike-py --exclude deepstrike-node --exclude deepstrike-wasm

cargo check -p deepstrike-node
cd node && npm run build && npm test

cargo check -p deepstrike-py
cd python && pytest

npm run docs:drift
npm run docs:build
```

## Project Structure

```text
crates/deepstrike-core/src/runtime/kernel.rs       ABI events/actions/observations and dispatch
crates/deepstrike-core/src/runtime/session.rs      durable session events
crates/deepstrike-core/src/runtime/replay.rs       OS snapshot folding and goldens
crates/deepstrike-core/src/scheduler/state_machine/ pure signal/budget/cancel transitions
crates/deepstrike-node/src/                        napi JSON/typed binding
crates/deepstrike-py/src/lib.rs                    PyO3 JSON binding
tests/rust/fixtures/                               wire-format goldens
node/src/runtime/                                  Node host adoption
python/deepstrike/runtime/                         Python host adoption
docs/decisions/                                    ADR and specification
```

## Code Style

- Express the final ABI v2 contract directly; keep no v1 adapter fields or branches.
- Use tagged unions and `snake_case` on the wire; optional fields use `default + skip_serializing_if`.
- State machines accept facts and return actions/observations; core performs no I/O.
- Rust core maintains one adjudication path, not parallel legacy/new implementations.
- Tests assert inputs, outputs, and state rather than internal call order.

## Testing Strategy

1. **RED: ABI contract** — add round-trip/golden tests for each new JSON input/observation.
2. **RED: state transition** — cover signal accepted/queued/deduped/dropped, grant boundaries, cancellation in each loop phase, and repeated cancellation.
3. **GREEN: minimum core implementation** — make Rust unit and integration tests pass first.
4. **Binding parity** — Node and Python produce equivalent wire shapes for the same ABI v2 fixtures and reject v1 fixtures.
5. **Replay differential** — compare actions, observations, usage, and correlation between uninterrupted and restored runs.
6. **Regression** — run complete Rust, Node, Python, and documentation verification.

Large snapshot updates do not replace precise assertions; each golden change must remain human-readable.

## Boundaries

### Always

- ABI v1 inputs return a stable version mismatch; no implicit upgrade or downgrade is allowed.
- Every external side effect first emits an action with `effect_id`; facts are observed only after result feedback.
- Write failing tests first; keep core, Node, and Python buildable after each sub-slice.
- Treat operation/delivery/reservation IDs as opaque correlation and never log credentials.
- Report terminal usage from kernel-local counters, not by rereading host ledgers.

### Ask First

- Change an approved ABI v2 public shape after it is published.
- Add dependencies, change CI, or introduce a breaking snapshot format change.
- Move a store, database, network, or process-control responsibility into core.

### Never

- Implement delivery claim/ack, budget reserve/settle, or external I/O cancellation in Rust core.
- Persist lease tokens, API keys, file paths, or executable handles in kernel snapshots.
- Overload timeout or critical signal as every cancellation reason.
- Silently accept ABI v1 or keep a hidden Node/Python fallback that bypasses the v2 contract.
- Drive host side effects through observations or a public mutable state-machine API.

## Success Criteria

- `KERNEL_ABI_VERSION == 2`, and every ABI v1 input returns a version mismatch.
- Core, Node, and Python public surfaces no longer contain legacy `signal`, `signal_disposed`, or `group_*_base`.
- Every input, step, action/result is traceable by operation/event/effect identity; duplicates are idempotent and conflicting replay produces a structured fault.
- Public surfaces no longer expose `state_machine_mut()`; lifecycle rules constrain configuration and event order.
- Memory/workflow/preemption/approval/spool/page-out host I/O uses action/result rather than observations as commands.
- Every `deliver_signal` produces one disposition carrying the same `delivery_id`; redeliveries remain distinguishable.
- Core enforces all three `budget_grant` axes locally and reports actual usage correlated with `reservation_id`.
- `cancel_operation` idempotently produces the same terminal cancellation state in Reason, ToolAwait, SubAgentAwait, and Workflow phases.
- Snapshot/restore preserves delivery/reservation/operation correlation and produces the same next step as uninterrupted execution.
- `KernelSnapshotV2` restores real kernel state and signal/event dedupe remains bounded.
- Node and Python expose only ABI v2, with contract tests for v1 rejection and v2 parity; full Rust/Node/Python/docs verification passes.
- Core gains no persistence, network, filesystem, provider, or process side effects.

## Non-goals

- No production Redis/PostgreSQL budget or signal store in this slice.
- No product-level ReactiveSession turn-policy change; it only consumes the new kernel decisions.
- No global session-event rename or scheduler rewrite.
- No adapter, shim, or deprecation window for ABI v1.

## Confirmed Decisions

1. This remains a stacked slice on `codex/runtime-contract-refactor`.
2. Implementation order is signal correlation, budget grant, then cancellation/replay.
3. Confirmed: cut directly to ABI v2 with no backward compatibility.
