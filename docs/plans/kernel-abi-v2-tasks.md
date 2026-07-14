# 任务清单：Kernel ABI v2 Reliability

## 状态

Proposed

## 执行规则

- 严格按依赖顺序执行；每项先提交能证明缺失行为的 RED 测试，再实现 GREEN，最后做行为不变的 REFACTOR。
- 不创建 ABI v1 adapter、legacy variant 或临时 fallback。
- 每项完成后运行列出的定向验证；每个 checkpoint 再运行完整受影响语言测试。
- 下列文件是预期上限；发现需要超过 5 个文件时，先更新任务清单并重新拆分。

## Phase 1：ABI v2 foundation

### Task 1：拆分 kernel protocol、runtime 与 tests

**Acceptance**

- `kernel.rs` 只保留模块组织与稳定 re-export。
- protocol types、step dispatch、tests 分离，wire output 与行为不变。

**Verify**

```bash
cargo test -p deepstrike-core runtime::kernel
cargo test -p deepstrike-tests t12_golden_fixtures
```

**Dependencies:** 无。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel.rs`
- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`tests.rs`）

### Task 2：建立 v2 envelope、identity 与 KernelFault

**Acceptance**

- `KERNEL_ABI_VERSION == 2`；input/step/action 使用 operation/event/step/effect identity。
- v1、重复冲突 event、未知 effect result 返回结构化 fault；相同重放幂等。
- v2 golden round-trip，v1 fixture 只验证 rejection。

**Verify**

```bash
cargo test -p deepstrike-core runtime::kernel
cargo test -p deepstrike-tests t12_golden_fixtures
```

**Dependencies:** Task 1。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`tests.rs`）
- `crates/deepstrike-core/src/runtime/mod.rs`
- `rust/src/runtime/runner.rs`
- Node、Python、WASM binding crates（各自的 `lib.rs`）
- `tests/rust/`（`src/t12_golden_fixtures.rs`）
- `tests/fixtures/abi/`（v2 input/step fixtures 与只用于 rejection 的 v1 fixture）

### Task 3：严格 lifecycle 与原子配置

**Acceptance**

- lifecycle 为 Created/Configured/Running/Suspended/terminal。
- `ConfigureRun` 先整体校验再应用；失败不产生部分 mutation。
- Workflow 不再 auto-start；非法顺序产生 `invalid_lifecycle`。
- 预载历史后的空 `Resume` 只允许 Configured → Running；带 approval 结果的 `Resume` 仍只允许从 Suspended 进入 Running。

**Verify**

```bash
cargo test -p deepstrike-core lifecycle
cargo test -p deepstrike-tests t13_transaction
```

**Dependencies:** Task 2。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/runtime/mod.rs`
- `crates/deepstrike-core/src/scheduler/state_machine/workflow.rs`
- `crates/deepstrike-core/src/runtime/kernel/`（`tests.rs`）
- `tests/rust/`（`src/t13_transaction.rs`）

### Task 4：封闭 state-machine mutable escape hatch

**Acceptance**

- core 提供 status/turn/render/drain/usage 等窄 projection API。
- Rust、Node、Python、WASM binding 不再调用 `state_machine_mut()`。
- `KernelRuntime` public surface 不返回内部可变 state machine。

**Verify**

```bash
cargo check --workspace
rg -n "state_machine_mut" rust crates/deepstrike-node crates/deepstrike-py crates/deepstrike-wasm
```

**Dependencies:** Task 3。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `rust/src/runtime/runner.rs`
- `crates/deepstrike-node/src/`（`lib.rs`）
- `crates/deepstrike-py/src/lib.rs`
- `crates/deepstrike-wasm/src/`（`lib.rs`）
- Rust ABI integration fixtures（t12 golden 与 t13 transaction）

## Checkpoint A

```bash
cargo test --workspace --exclude deepstrike-py --exclude deepstrike-node --exclude deepstrike-wasm
cargo check -p deepstrike-node
cargo check -p deepstrike-py
cargo check -p deepstrike-wasm
```

## Phase 2：Effect protocol

### Task 5：为 provider、tool 与 milestone 建立 effect/result correlation

**Acceptance**

- 每个 effect action 有稳定 `effect_id`；result 必须引用它。
- 重复 result 不重复推进状态；未知/冲突 result 产生 fault。
- crash/replay fixture 保持相同下一步 action identity。

**Verify**

```bash
cargo test -p deepstrike-core effect
cargo test -p deepstrike-tests t11_runtime
```

**Dependencies:** Task 2、3。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/scheduler/state_machine/mod.rs`
- `crates/deepstrike-core/src/runtime/kernel/`（`tests.rs`）
- `tests/rust/`（`src/t11_runtime.rs`）

### Task 6：把 approval、workflow spawn 与 preempt 改为 action/result

**Acceptance**

- approval request、workflow spawn、sub-agent preempt 通过 action 驱动。
- 对应 observation 只在宿主结果回灌后产生。
- 失败 effect 不被记录为已完成事实。

**Verify**

```bash
cargo test -p deepstrike-core approval
cargo test -p deepstrike-core workflow
cargo test -p deepstrike-core preempt
```

**Dependencies:** Task 5。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/scheduler/state_machine/gate.rs`
- `crates/deepstrike-core/src/scheduler/state_machine/workflow.rs`
- `crates/deepstrike-core/src/scheduler/state_machine/signal.rs`

### Task 7：把 memory、spool 与 page-out 改为 action/result

**Acceptance**

- memory persist/query、large-result spool、page-out archive 都输出 effect action。
- `MemoryWritten` 等成功 observation 只在 result success 后产生，并有失败 observation。
- 不再出现“observation 发出后宿主才执行 I/O”的路径。

**Verify**

```bash
cargo test -p deepstrike-core memory
cargo test -p deepstrike-core spool
cargo test -p deepstrike-core page_out
```

**Dependencies:** Task 5。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/scheduler/state_machine/mod.rs`
- `crates/deepstrike-core/src/scheduler/state_machine/eviction.rs`
- `crates/deepstrike-core/src/runtime/kernel/`（`tests.rs`）

### Task 7A：开放有边界的内核可靠性配置

**Acceptance**

- replay/effect 窗口、provider/output 恢复次数和 spool 阈值由 `RunConfig.reliability` 聚合配置。
- ABI 边界校验安全范围并原子应用；缺省值保持原行为。
- signal、budget、repeat fuse 等既有策略不重复建模，纯实现常量不公开。

**Verify**

```bash
cargo test -p deepstrike-core reliability_config
```

**Dependencies:** Task 3、5。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`、`runtime.rs`、`tests.rs`）
- `crates/deepstrike-core/src/scheduler/state_machine/`（`mod.rs`、`eviction.rs`）
- `docs/decisions/003-kernel-reliability-configuration.md`

### Task 8：Node effect protocol cutover

**Acceptance**

- Node runner 只消费 action 并回传 effect result，不从 observation 启动 I/O。
- session log 只记录完成事实；重复 effect result 测试通过。

**Verify**

```bash
cd node && npm run build
cd node && npm test -- --runInBand tests/runtime/scheduler-lifecycle.test.ts tests/runtime/memory-syscall.test.ts
```

**Dependencies:** Task 6、7。

**Files:**

- `node/src/runtime/kernel-step.ts`
- `node/src/runtime/runner.ts`
- `node/src/runtime/session-log.ts`
- `node/tests/runtime/scheduler-lifecycle.test.ts`
- `node/tests/runtime/memory-syscall.test.ts`

### Task 9：Python effect protocol cutover

**Acceptance**

- Python runner 只消费 action 并回传 effect result。
- session log 不再把未执行操作记录为事实；重复 result 幂等。

**Verify**

```bash
cd python && pytest tests/test_memory_syscall.py tests/test_workflow_preempt.py -q
```

**Dependencies:** Task 6、7。

**Files:**

- `python/deepstrike/runtime/runner.py`
- `python/deepstrike/runtime/session_log.py`
- `python/deepstrike/runtime/kernel_event_log.py`
- `python/tests/test_memory_syscall.py`
- `python/tests/test_workflow_preempt.py`

## Checkpoint B

```bash
cd node && npm run build && npm test
cd python && pytest
```

## Phase 3：Signal、budget 与 cancellation

### Task 10：实现 delivery-aware signal 与有界 dedupe

**Acceptance**

- 只存在 `deliver_signal` / `signal_delivery_disposed`。
- operation/delivery identity 校验；redelivery attempt 可区分。
- dedupe replay window 容量固定，snapshot 投影不会无界增长。

**Verify**

```bash
cargo test -p deepstrike-core signal
cargo test -p deepstrike-tests t06_signals
```

**Dependencies:** Task 2、5。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/scheduler/state_machine/signal.rs`
- `crates/deepstrike-core/src/signals/router.rs`
- `tests/rust/`（`src/t06_signals.rs`）

### Task 11：Node signal cutover

**Acceptance**

- Node gateway 每次投递生成/透传 delivery ID，只使用 ABI v2 signal。
- ack/nack 依据同一 delivery disposition；无 legacy signal fallback。

**Verify**

```bash
cd node && npm test -- --runInBand tests/runtime/signal-delivery.test.ts tests/runtime/attention-policy.test.ts
```

**Dependencies:** Task 10。

**Files:**

- `node/src/runtime/kernel-step.ts`
- `node/src/runtime/runner.ts`
- `node/src/runtime/session-log.ts`
- `node/tests/runtime/signal-delivery.test.ts`
- `node/tests/runtime/attention-policy.test.ts`

### Task 12：Python signal cutover

**Acceptance**

- Python gateway 只使用 delivery-aware v2 signal。
- delivery disposition 与 ack/nack correlation 测试通过，无 fallback。

**Verify**

```bash
cd python && pytest tests/test_signal_delivery.py tests/test_signal_addressing.py -q
```

**Dependencies:** Task 10。

**Files:**

- `python/deepstrike/runtime/runner.py`
- `python/deepstrike/runtime/session_log.py`
- `python/deepstrike/runtime/os_snapshot.py`
- `python/tests/test_signal_delivery.py`
- `python/tests/test_signal_addressing.py`

### Task 13：实现 reservation-backed BudgetGrant

**Acceptance**

- 删除所有 `group_*_base`/seed API；grant 直接限制 tokens/subagents/rounds。
- terminal 只输出一次 correlated usage；超限带 operation/reservation ID。

**Verify**

```bash
cargo test -p deepstrike-core budget_grant
cargo test -p deepstrike-tests t15_sub_agent
```

**Dependencies:** Task 2、5。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/scheduler/state_machine/mod.rs`
- `crates/deepstrike-core/src/scheduler/state_machine/gate.rs`
- `crates/deepstrike-core/src/scheduler/state_machine/tests.rs`

### Task 14：Node RunGroup 只保留 reservation path

**Acceptance**

- RunGroup 要求 `ReservableGroupBudgetStore`，删除 accounting fallback。
- grant 进入 kernel，usage 用同一 reservation ID settle/release。

**Verify**

```bash
cd node && npm test -- --runInBand tests/run-group-budget.test.ts
```

**Dependencies:** Task 13。

**Files:**

- `node/src/runtime/run-group.ts`
- `node/src/runtime/runner.ts`
- `node/src/runtime/kernel-step.ts`
- `node/src/runtime/session-log.ts`
- `node/tests/run-group-budget.test.ts`

### Task 15：Python RunGroup 只保留 reservation path

**Acceptance**

- Python RunGroup 删除 read/charge fallback。
- grant/usage/settlement 使用同一 reservation identity。

**Verify**

```bash
cd python && pytest tests/test_run_group_budget.py -q
```

**Dependencies:** Task 13。

**Files:**

- `python/deepstrike/runtime/run_group.py`
- `python/deepstrike/runtime/runner.py`
- `python/deepstrike/runtime/session_log.py`
- `python/tests/test_run_group_budget.py`

### Task 16：实现统一 operation cancellation

**Acceptance**

- 删除宿主 `timeout` input，新增 closed cancellation reason 与 action/result cleanup。
- Reason/ToolAwait/SubAgentAwait/Workflow 产生一致、幂等 terminal cancellation。
- 冲突 operation/result fail closed；terminal usage 仍只报告一次。

**Verify**

```bash
cargo test -p deepstrike-core cancellation
cargo test -p deepstrike-tests t02_state_machine
```

**Dependencies:** Task 5、6、13。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/types/result.rs`
- `crates/deepstrike-core/src/scheduler/state_machine/mod.rs`
- `crates/deepstrike-core/src/scheduler/state_machine/tests.rs`

### Task 17：Node/Python cancellation cutover

**Acceptance**

- AbortSignal、CancelScope/CancelledError 映射到 `cancel_operation`。
- lease lost、deadline、host shutdown、user 四类 reason 有跨语言测试。
- SDK 不再用 timeout/critical signal 模拟取消。

**Verify**

```bash
cd node && npm test -- --runInBand tests/runtime/scheduler-lifecycle.test.ts
cd python && pytest tests/test_runtime_reliability.py -q
```

**Dependencies:** Task 16。

**Files:**

- `node/src/runtime/reliability.ts`
- `node/src/runtime/runner.ts`
- `node/tests/runtime/scheduler-lifecycle.test.ts`
- `python/deepstrike/runtime/reliability.py`
- `python/tests/test_runtime_reliability.py`

## Checkpoint C

```bash
cargo test --workspace --exclude deepstrike-py --exclude deepstrike-node --exclude deepstrike-wasm
cd node && npm run build && npm test
cd python && pytest
```

## Phase 4：Snapshot、replay 与清理

### Task 18：实现 KernelSnapshotV2

**Acceptance**

- snapshot 恢复 phase、operation、pending effects、workflow、budget usage、dedupe window 与 terminal latch。
- snapshot schema 独立于内部 state-machine serde；不兼容 snapshot 返回 fault。
- uninterrupted/restored differential test 等价。

**Verify**

```bash
cargo test -p deepstrike-core snapshot_v2
cargo test -p deepstrike-tests t12_golden_fixtures
```

**Dependencies:** Task 10、13、16。

**Files:**

- `crates/deepstrike-core/src/runtime/kernel/`（`protocol.rs`）
- `crates/deepstrike-core/src/runtime/kernel/`（`runtime.rs`）
- `crates/deepstrike-core/src/runtime/replay.rs`
- `crates/deepstrike-core/src/runtime/session.rs`
- `tests/rust/`（`src/t12_golden_fixtures.rs`）

### Task 19：Node snapshot/replay parity

**Acceptance**

- Node 可持久化/恢复 KernelSnapshotV2，并保持 action/effect identity。
- OS snapshot 明确为 audit projection，不冒充 runtime snapshot。

**Verify**

```bash
cd node && npm test -- --runInBand tests/runtime/kernel-event-log.test.ts tests/runtime/signal-delivery.test.ts
```

**Dependencies:** Task 18。

**Files:**

- `node/src/runtime/kernel-step.ts`
- `node/src/runtime/runner.ts`
- `node/src/runtime/os-snapshot.ts`
- `node/src/runtime/kernel-event-log.ts`
- `node/tests/runtime/kernel-event-log.test.ts`

### Task 20：Python snapshot/replay parity

**Acceptance**

- Python 可持久化/恢复 KernelSnapshotV2，并保持 action/effect identity。
- OS snapshot 只作为 audit projection。

**Verify**

```bash
cd python && pytest tests/test_runtime_wake.py tests/test_signal_delivery.py -q
```

**Dependencies:** Task 18。

**Files:**

- `python/deepstrike/runtime/runner.py`
- `python/deepstrike/runtime/os_snapshot.py`
- `python/deepstrike/runtime/kernel_event_log.py`
- `python/tests/test_runtime_wake.py`
- `python/tests/test_signal_delivery.py`

### Task 21：建立 kernel performance baseline

**Acceptance**

- 基准覆盖 step、large-context render、compression、10k-event replay、large workflow、signal storm、snapshot encode/decode。
- 记录时间、allocation/size 基线；不在无数据时做 clone 微优化。

**Verify**

```bash
cargo bench -p deepstrike-core --no-run
cargo test -p deepstrike-core
```

**Dependencies:** Task 18。

**Files:**

- `crates/deepstrike-core/Cargo.toml`
- `crates/deepstrike-core/benches/kernel_runtime.rs`
- `benchmark/README.md`

备注：`benchmark/README.md` 当前有用户未提交改动；执行本任务前必须先检查并保留该改动，无法安全合并时暂停该文件。

### Task 22：删除残留并完成全量验证

**Acceptance**

- runtime/public 源码无 v1、legacy signal、base budget、observation-command、mutable escape hatch 或 accounting fallback。
- Rust/Node/Python/WASM/docs 全部通过。
- ADR、plan、task 状态更新为完成，记录实际验证结果。

**Verify**

```bash
rg -n "group_tokens_base|group_spawns_base|group_rounds_base|signal_disposed|state_machine_mut|KERNEL_ABI_VERSION: u32 = 1" crates rust node python tests
cargo test --workspace --exclude deepstrike-py --exclude deepstrike-node --exclude deepstrike-wasm
cargo check -p deepstrike-node && cargo check -p deepstrike-py && cargo check -p deepstrike-wasm
cd node && npm run build && npm test
cd python && pytest
npm run docs:drift && npm run docs:build
```

**Dependencies:** Tasks 1–21。

**Files:** 按残留扫描拆成独立机械删除提交；若超过 5 个文件，不与行为修改混在同一提交。
