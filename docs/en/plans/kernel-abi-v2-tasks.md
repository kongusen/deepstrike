# Task List: Kernel ABI v2 Reliability

## Status

Proposed

## Execution Rules

- Follow dependencies. Each task starts with a RED contract test, reaches GREEN with the minimum implementation, then refactors without changing behavior.
- Add no ABI v1 adapter, legacy variant, or temporary fallback.
- Run focused verification per task and full affected-language checks at checkpoints.
- The listed files are the expected maximum. If a task needs more than five, update and split this list first.

## Phase 1: ABI v2 Foundation

### Task 1: Split kernel protocol, runtime, and tests

Acceptance: `kernel.rs` becomes module/re-export glue; protocol, dispatch, and tests are separated with byte-identical behavior.

Verify: `cargo test -p deepstrike-core runtime::kernel` and `cargo test -p deepstrike-tests t12_golden_fixtures`.

Dependencies: none.

Files: `runtime/kernel.rs`, new `runtime/kernel/protocol.rs`, `runtime/kernel/runtime.rs`, `runtime/kernel/tests.rs` under `crates/deepstrike-core/src`.

### Task 2: Add v2 envelopes, identity, and KernelFault

Acceptance: ABI version 2; operation/event/step/effect identity; v1 rejection; idempotent exact replay; structured conflicting-event/effect faults and v2 goldens.

Verify: core kernel tests and `t12_golden_fixtures`.

Dependencies: Task 1.

Files: kernel protocol/runtime/tests, `tests/rust/`（`src/t12_golden_fixtures.rs`）, `tests/fixtures/abi/input_start_run.json`.

### Task 3: Enforce lifecycle and atomic configuration

Acceptance: Created/Configured/Running/Suspended/terminal lifecycle; validate configuration before applying it; no workflow auto-start; invalid order faults without partial mutation.

Verify: `cargo test -p deepstrike-core lifecycle` and `cargo test -p deepstrike-tests t13_transaction`.

Dependencies: Task 2.

Files: kernel protocol/runtime/tests, state-machine mod, `tests/rust/`（`src/t13_transaction.rs`）.

### Task 4: Close the mutable state-machine escape hatch

Acceptance: narrow status/turn/render/drain/usage projections; Rust/Node/Python/WASM no longer call `state_machine_mut()`; public runtime exposes no mutable internals.

Verify: `cargo check --workspace` and a source search for `state_machine_mut`.

Dependencies: Task 3.

Files: kernel runtime, Rust runner, Node/Python/WASM binding `lib.rs` files.

## Checkpoint A

Run Rust workspace tests and check Node, Python, and WASM crates.

## Phase 2: Effect Protocol

### Task 5: Correlate provider, tool, and milestone effects/results

Acceptance: stable effect IDs, required result correlation, idempotent duplicate results, faulted unknown/conflicting results, stable crash/replay action identity.

Verify: core effect tests and Rust `t11_runtime`.

Dependencies: Tasks 2–3.

Files: kernel protocol/runtime/tests, state-machine mod, `tests/rust/`（`src/t11_runtime.rs`）.

### Task 6: Convert approval, workflow spawn, and preemption to action/result

Acceptance: host work starts only from actions; observations appear only after result feedback; failures are not recorded as completed facts.

Verify: focused core approval/workflow/preemption tests.

Dependencies: Task 5.

Files: kernel protocol/runtime, state-machine gate/workflow/signal.

### Task 7: Convert memory, spool, and page-out to action/result

Acceptance: persistence/query/spool/archive emit effects; success observations follow successful results; explicit failure observations exist; no observation-triggered unfinished I/O.

Verify: focused core memory/spool/page-out tests.

Dependencies: Task 5.

Files: kernel protocol/runtime/tests, state-machine mod/eviction.

### Task 8: Cut Node over to the effect protocol

Acceptance: runner consumes actions and feeds effect results; logs contain completed facts only; duplicate results are tested.

Verify: Node build plus scheduler-lifecycle and memory-syscall tests.

Dependencies: Tasks 6–7.

Files: Node kernel-step, runner, session-log, and two focused tests.

### Task 9: Cut Python over to the effect protocol

Acceptance: Python consumes actions and returns effect results; logs contain completed facts only; duplicate results are idempotent.

Verify: Python memory-syscall and workflow-preempt tests.

Dependencies: Tasks 6–7.

Files: Python runner, session log, kernel event log, and two focused tests.

## Checkpoint B

Run complete Node build/tests and Python tests.

## Phase 3: Signal, Budget, and Cancellation

### Task 10: Implement delivery-aware signal and bounded dedupe

Acceptance: only `deliver_signal`/`signal_delivery_disposed`; operation/delivery validation; distinct redelivery attempts; fixed-capacity replay window.

Verify: core signal tests and Rust `t06_signals`.

Dependencies: Tasks 2 and 5.

Files: kernel protocol/runtime, state-machine signal, signal router, Rust signal tests.

### Task 11: Cut Node signal delivery to v2

Acceptance: every delivery has an identity, ack/nack uses its disposition, and no legacy fallback remains.

Verify: Node signal-delivery and attention-policy tests.

Dependencies: Task 10.

Files: Node kernel-step, runner, session-log, and two focused tests.

### Task 12: Cut Python signal delivery to v2

Acceptance: delivery-aware signal only, correlated ack/nack, no fallback.

Verify: Python signal-delivery and signal-addressing tests.

Dependencies: Task 10.

Files: Python runner, session log, OS snapshot, and two focused tests.

### Task 13: Implement reservation-backed BudgetGrant

Acceptance: no group-base seed APIs; direct token/subagent/round grant enforcement; one terminal usage report; correlated exceeded observations.

Verify: core budget-grant tests and Rust `t15_sub_agent`.

Dependencies: Tasks 2 and 5.

Files: kernel protocol/runtime, state-machine mod/gate/tests.

### Task 14: Keep only the Node reservation path

Acceptance: RunGroup requires a reservable store; no accounting fallback; grant and settlement share reservation identity.

Verify: Node run-group-budget tests.

Dependencies: Task 13.

Files: Node run-group, runner, kernel-step, session-log, and budget test.

### Task 15: Keep only the Python reservation path

Acceptance: no read/charge fallback; grant, usage, and settlement share reservation identity.

Verify: Python run-group-budget tests.

Dependencies: Task 13.

Files: Python run-group, runner, session log, and budget test.

### Task 16: Implement unified operation cancellation

Acceptance: no host timeout input; closed cancellation reasons; identical terminal cancellation across phases; conflicting identity faults; one terminal usage report.

Verify: core cancellation tests and Rust `t02_state_machine`.

Dependencies: Tasks 5, 6, and 13.

Files: kernel protocol/runtime, result types, state-machine mod/tests.

### Task 17: Cut Node and Python cancellation to v2

Acceptance: AbortSignal and CancelScope/CancelledError map to cancellation events; four reasons have tests; no timeout/critical-signal emulation.

Verify: Node scheduler-lifecycle and Python runtime-reliability tests.

Dependencies: Task 16.

Files: Node reliability/runner/test and Python reliability/test.

## Checkpoint C

Run complete Rust, Node, and Python tests.

## Phase 4: Snapshot, Replay, and Cleanup

### Task 18: Implement KernelSnapshotV2

Acceptance: restore phase, operation, effects, workflow, budget, dedupe, and terminal latch without serializing internal structs; incompatible snapshots fault; differential replay passes.

Verify: core snapshot tests and Rust goldens.

Dependencies: Tasks 10, 13, and 16.

Files: kernel protocol/runtime, runtime replay/session, Rust golden test.

### Task 19: Add Node snapshot/replay parity

Acceptance: persist/restore KernelSnapshotV2 with action/effect identity; OS snapshot remains an audit projection.

Verify: Node kernel-event-log and signal-delivery tests.

Dependencies: Task 18.

Files: Node kernel-step, runner, OS snapshot, kernel event log, and event-log test.

### Task 20: Add Python snapshot/replay parity

Acceptance: persist/restore KernelSnapshotV2 with action/effect identity; OS snapshot remains an audit projection.

Verify: Python runtime-wake and signal-delivery tests.

Dependencies: Task 18.

Files: Python runner, OS snapshot, kernel event log, and two focused tests.

### Task 21: Establish kernel performance baselines

Acceptance: benchmark step, large-context render, compression, 10k-event replay, large workflow, signal storm, and snapshot encode/decode; record time/allocation/size before micro-optimization.

Verify: `cargo bench -p deepstrike-core --no-run` and core tests.

Dependencies: Task 18.

Files: core Cargo manifest, new kernel benchmark, benchmark README. Preserve the user's existing dirty benchmark changes.

### Task 22: Remove residue and run full verification

Acceptance: no v1, legacy signal, base budget, observation-command, mutable escape hatch, or accounting fallback in runtime/public source; all Rust/Node/Python/WASM/docs checks pass; docs record completion.

Dependencies: Tasks 1–21.

Files: split residual mechanical cleanup into commits of five files or fewer; do not mix it with behavior changes.
