# 实施计划：Kernel ABI v2 Reliability

## 状态

Accepted

## 依据

[ADR-002](../decisions/002-kernel-abi-reliability) 已确定直接切换 ABI v2，不提供 v1 adapter、shim 或双路径。

## 总体方案

以 Rust core 的 wire contract 为唯一事实源，按纵向可靠性能力切换，而不是先堆一批新字段再让宿主逐步猜测。每一阶段先固定失败的 ABI/state contract，再修改 core，随后在同一阶段切换 Node/Python 消费方与 replay 表达。

```text
operation_id (start_run)
        │
        ├── delivery_id ──► signal disposition ──► host ack/nack
        ├── reservation_id ► budget usage        ──► host settle/release
        └── cancel reason ─► terminal state       ──► replay/audit
```

宿主仍拥有外部原子性和 I/O；core 只拥有 identity validation、确定性状态转换、用量计数与 observation。

## 架构决策

### 1. Version gate 一次切断 v1

- `KERNEL_ABI_VERSION` 直接改为 `2`。
- `KernelInput.version != 2` 返回稳定的 version mismatch，不解析为 v2 event。
- v2 fixture 覆盖唯一支持的输入输出；v1 fixture 只保留 rejection 测试价值。
- Node/Python public constants、类型与测试在同一切片切换到 2。

### 2. Operation identity 在 run 启动时绑定

- 每个 input envelope 要求 `operation_id`、`event_id`、`observed_at_ms`；`start_run` 绑定 operation。
- 每个 step 携带 `step_seq` 与 `input_event_id`；每个 action/result 通过 `effect_id` 关联。
- `KernelRuntime`/state machine 在 run 生命周期中保存不可变 identity。
- `deliver_signal`、budget observations 与 `cancel_operation` 必须匹配当前 identity。
- operation 未启动、ID 缺失或冲突时 fail closed，不产生业务 action。

### 2A. Action/Result 是唯一宿主副作用协议

- provider/tool/milestone/workflow/preempt/approval/memory/spool/page-out 都输出带 `effect_id` 的 action。
- 宿主回传 effect result 后，内核才输出成功/失败 observation。
- 重复 result 幂等；未知或 payload 冲突 result 产生 `KernelFault`。

### 2B. KernelFault、lifecycle 与状态封装

- 版本、身份、顺序、配置、重放和 snapshot 错误使用结构化 fault，不复用 `ToolGated`。
- 生命周期固定为 Created、Configured、Running、Suspended、terminal；配置原子应用，workflow 不 auto-start。
- 删除 public `state_machine_mut()`，binding 只使用稳定的 projection/command API。

### 3. Signal 只保留 delivery-aware 路径

- 删除 `KernelInputEvent::Signal`，新增 `DeliverSignal`。
- 删除 `SignalDisposed`，新增 `SignalDeliveryDisposed`。
- router/attention 内部仍复用现有队列与 dedupe 逻辑，但 disposition 必须携带 operation/delivery correlation。
- session event、event category、OS snapshot、Node/Python event mapping 同步改成新名称和 shape。

### 4. Group budget 只接受显式 grant

- 删除 `group_tokens_base`、`group_spawns_base`、`group_rounds_base` 及 seed 方法。
- `BudgetGrant` 保存 reservation identity 和各轴获批额度；本地计数直接与 grant 比较。
- terminal 只输出一次 `BudgetUsageReported`；`BudgetExceeded` 携带 operation/reservation correlation。
- Node/Python RunGroup 执行要求 `ReservableGroupBudgetStore`，删除 accounting fallback。
- standalone run 继续由本地 scheduler/resource quota 管理，不伪造 reservation。

### 5. Cancellation 是独立状态转换

- 删除宿主通用 `timeout` 输入，新增 `CancelOperation`。
- 新增封闭的 `CancellationReason`：`user`、`deadline`、`lease_lost`、`host_shutdown`。
- state machine 从 Reason、ToolAwait、SubAgentAwait、Workflow 等 phase 进入同一 cancelled terminal path。
- 重复相同取消是 no-op；不同 operation ID 或不一致的重复取消 fail closed。
- scheduler 自身耗尽 wall-time 仍可保留内部 `Timeout` termination，但宿主不得用它代替 cancellation。

### 6. Replay 以 observation 为事实源

- snapshot/audit fold 记录 operation、delivery、reservation、cancellation 四类关联。
- 不持久化 delivery lease token、store revision、AbortSignal/CancelScope 或外部 handle。
- 用 differential test 证明 uninterrupted 与 restore 后的下一步 action/observation 一致。

## 阶段与依赖

### 阶段 A：ABI v2 foundation

先行为等价地拆分 `kernel.rs` 的 protocol/runtime/tests，再建立 version gate、统一 input/step/effect envelope、KernelFault、lifecycle 与 projection API，并让所有 StartRun 调用迁移到 v2。

依赖：无。

检查点：core 与 Rust integration tests 通过；v1 rejection、v2 round-trip、重复/冲突事件、非法生命周期和无 mutable escape hatch 明确。

### 阶段 A2：Effect protocol

把 observation 驱动的宿主命令改为 action/result：先处理 approval/workflow/preempt，再处理 memory/spool/page-out；每条路径成功/失败后才记录事实。

依赖：阶段 A。

检查点：每个宿主副作用都有稳定 effect ID；重复 result 不重复转移状态；源码中不存在由 observation 触发未完成 I/O 的路径。

### 阶段 B：Delivery-aware signal

替换 signal input/observation，贯通 state machine、session event、event log、OS snapshot，再切换 Node/Python signal gateway 与测试。

依赖：阶段 A。

检查点：accepted/queued/deduped/dropped 都回传同一 delivery ID；redelivery attempt 可区分；仓库不存在 public legacy signal path。

### 阶段 C：Reservation-backed budget grant

替换 base budget，内核直接执行 grant，输出 usage；Node/Python RunGroup 删除 accounting fallback 并用 reservation ID 结算。

依赖：阶段 A；可在阶段 B 的接口稳定后独立实现。

检查点：tokens/subagents/rounds 边界测试、终态 usage 唯一性、settle/release correlation 和并发 reservation 测试通过。

### 阶段 D：Operation cancellation

增加 cancellation input/reason/observation，统一各 phase 状态转换，并将 Node AbortSignal、Python CancelScope/CancelledError 映射到 v2 event。

依赖：阶段 A；usage terminal 语义依赖阶段 C。

检查点：所有 phase、重复取消、冲突 ID、deadline 与 lease-lost 场景通过；不再用 host timeout/critical signal 冒充取消。

### 阶段 E：Replay 与残留清理

完成 `KernelSnapshotV2`、session replay/OS audit snapshot differential fixtures、有界 event/signal dedupe，删除 v1/base/signal/observation-command/accounting fallback 残留，完成跨语言验证与文档更新。

依赖：阶段 B、C、D。

检查点：全量 Rust/Node/Python/docs 通过，代码搜索无旧 public contract，core 没有新增 I/O。

## 验证策略

每个阶段执行 RED → GREEN → REFACTOR：

1. 先添加最小失败的 wire/state test，并确认失败原因是缺少目标行为。
2. 实现 core 的最小确定性路径。
3. 切换该路径的 Node/Python binding 与 host consumer。
4. 运行阶段定向测试，随后运行受影响语言的 build/test。
5. 每两个阶段运行一次完整 workspace checkpoint。

最终验证命令以 ADR-002 的 Commands 为准，并额外执行源码残留扫描：

```bash
rg -n "group_tokens_base|group_spawns_base|group_rounds_base|signal_disposed|kind: ['\"]signal['\"]" crates node python tests
```

期望结果：除 migration/ADR 历史说明外，public/runtime 源码无匹配。

## 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| Rust enum/struct 的破坏面大于 JSON ABI | 高 | 先编译全部 workspace，按 compiler error 清点消费者；不添加临时 compatibility variant |
| operation identity 在恢复时重复绑定 | 高 | snapshot/replay 明确恢复同一 ID；StartRun 冲突测试 fail closed |
| terminal usage 重复输出导致重复 settle | 高 | 内核维护 terminal-report latch；store settle 仍保持幂等 |
| cancellation 与已完成 action 竞态 | 高 | 输入事件顺序决定唯一结果；对 completion-before-cancel 与 cancel-before-completion 做差分测试 |
| accounting-only 自定义 store 立即失效 | 中 | 编译期/启动期明确报错，只公布 reservable contract，不静默降级 |
| Node/Python wire shape 漂移 | 中 | 共用 golden JSON，分别做 binding round-trip/parity 测试 |

## 计划门禁

本计划确认后，下一阶段才会拆成每项不超过约 5 个文件、带 Acceptance/Verify/Dependencies 的可执行任务；在任务清单再次确认前不修改内核实现。
