# ADR-002：Kernel ABI Reliability 切片

## 状态

Accepted

## 日期

2026-07-14

## 前置假设

1. 本切片基于 `codex/runtime-contract-refactor` 建立 stacked branch；它独立评审，但依赖上一切片已经定义的 operation scope、delivery lease 与 budget reservation 契约。
2. 这是一次明确的破坏性升级：`KERNEL_ABI_VERSION` 提升为 `2`，ABI v1 input 不再接受。
3. Rust core 只拥有确定性状态转换、裁决、关联与用量核算；持久化、分布式原子性和真实 I/O 取消继续由宿主拥有。
4. `group_*_base`、legacy `signal`、`signal_disposed` 与 accounting-only group budget fallback 直接移除，不保留双路径。
5. Node 与 Python 是本切片必须同时验证的宿主；WASM 只要求 core ABI 仍可编译，暂不增加专用上层 API。

若这些假设被调整，应先更新本 ADR，再进入实现计划。

## 目标

把 SDK reliability 契约在内核边界上补齐，使宿主不再通过隐含约定推断以下事实：

- 一个 leased signal delivery 最终对应了哪次内核裁决；
- 一个 run 实际消费的是哪笔 budget reservation，以及是否越过获批额度；
- operation cancellation 在不同 loop phase 中产生了什么确定性终态；
- 上述关联在 JSON round-trip、session log 与 snapshot/replay 后是否仍保持一致。

成功后的边界是：宿主先完成 `claim/reserve/cancel-I/O` 等外部动作，再把事实送入内核；内核统一裁决并输出带关联标识的 observation，宿主据此 `ack/nack/settle/release`。

## 当前问题

### Signal delivery 只有业务信号身份，没有投递身份

`RuntimeSignal.id` 标识信号本身，`dedupe_key` 用于内核队列去重；但宿主的 durable delivery lease 是另一条生命周期。同一信号发生 redelivery 时，内核当前只输出 `signal_id`，宿主无法证明某个 `SignalDisposed` 对应哪次 claim。

### Budget reservation 通过累计 base 间接表达

`RunConfig.group_tokens_base`、`group_spawns_base` 和 `group_rounds_base` 把其他成员的累计/占用量注入内核，再由全局 limit 减去 base。它能限制总量，却没有表达“本 run 获批了多少”以及“这次用量属于哪笔 reservation”，settlement 仍依赖宿主旁路状态。

### Cancellation 没有独立内核语义

当前 ABI 有 timeout、critical signal、provider error 与 process preemption 等局部路径，但没有统一的 operation cancellation input。宿主取消 provider/tool/sub-agent 后，难以把 `user`、`deadline`、`lease_lost`、`host_shutdown` 映射成一致且可 replay 的内核终态。

### Replay 记录结果，但没有完整的可靠性关联

`SignalDisposed` 和 `BudgetExceeded` 可进入 session event / OS snapshot，但缺少 delivery/reservation/operation correlation。恢复逻辑可以看到“发生过裁决”，却不能证明它属于哪个外部可靠性事务。

## 决策

### 1. ABI v2 使用统一 event、step、effect identity

新增的 ABI 值只携带不可变标识和纯数据：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetGrant {
    pub reservation_id: String,
    pub tokens: Option<u64>,
    pub subagents: Option<u32>,
    pub rounds: Option<u32>,
}
```

命名使用 wire-format 的 `snake_case`。ID 在 ABI 中是 opaque string；内核不解析其存储后端、租约版本或凭证。

`operation_id`、`event_id`、`observed_at_ms` 提升到每个 `KernelInput` 的统一 envelope；`KernelStep` 携带 `step_seq` 与 `input_event_id`；每个要求宿主执行 I/O 的 `KernelAction` 携带 `effect_id` 与 `causation_id`。Provider/tool/milestone/sub-agent 等结果必须引用原 `effect_id`。

`start_run` 绑定当前 operation identity。缺失、重复冲突或跨 operation 的 input/result 在 ABI 边界直接拒绝，不再从 session/run mutable state 推断。相同 `event_id` 的完全相同重放是幂等 no-op；相同 ID 不同 payload 是 fault。

### 2. 为 leased signal 增加独立的 delivery input/output

移除 legacy `signal`，统一使用 `deliver_signal` input，携带 `signal`、`delivery_id` 与必填 `operation_id`。即使宿主使用内存队列，也必须为每次投递生成稳定的 delivery identity。

内核为新输入输出 `signal_delivery_disposed`，至少包含：

- `signal_id`
- `delivery_id`
- `operation_id`
- `disposition`
- `queue_depth`

移除 `signal_disposed`，只输出 `signal_delivery_disposed`。内核不执行 `ack/nack`，也不保存 lease token。

### 3. 让获批额度成为一等 RunConfig

移除 `RunConfig.group_tokens_base`、`group_spawns_base`、`group_rounds_base`，以 `budget_grant` 作为 run-group 累计预算的唯一输入。standalone run 可不提供 grant，继续使用本地 `ResourceQuota`；加入 RunGroup 的 run 必须提供 reservation-backed grant。

- token/subagent/round 三个累计轴按 grant 直接限制本 run 的本地消费；
- terminal step 输出 `budget_usage_reported`，包含 `reservation_id` 与实际本地用量；
- 越界输出带 `operation_id` 和对应 `reservation_id` 的 `budget_exceeded`。

RunGroup 执行路径只接受具备 `reserve -> settle | release` 的 store，不再退回 `read -> charge` accounting 模式。内核仍不读取共享账本。

### 4. 引入统一的 operation cancellation event

新增 `cancel_operation` input：

```json
{
  "kind": "cancel_operation",
  "operation_id": "op-123",
  "reason": "lease_lost",
  "pending_call_ids": ["tool-7"]
}
```

`reason` 使用封闭枚举：`user`、`deadline`、`lease_lost`、`host_shutdown`。内核在所有可运行/等待 phase 中执行幂等终止，输出一次 `operation_cancelled` observation，并返回确定性的 terminal/await action。重复相同取消不得生成第二次终态；冲突 operation ID 必须 fail closed。

宿主负责先触发 provider/tool/sub-agent 的真实取消，并把已知 pending call IDs 作为事实输入。内核不持有线程、future、process handle 或网络连接。

### 5. Action 是命令，Observation 只记录事实

所有要求宿主执行动作的输出统一建模为 `KernelAction`，包括 provider/tool/milestone、workflow spawn、sub-agent preempt、approval request、memory persist/query、result spool 与 page-out archive。宿主通过带 `effect_id` 的结果 input 回灌；只有成功或失败结果被内核消费后，才输出对应 observation。

禁止再用 `MemoryWritten`、`MemoryQueried`、`WorkflowBatchSpawned`、`AgentPreempted` 等 observation 触发尚未发生的宿主副作用。

### 6. 使用 KernelFault、严格 lifecycle 与封闭状态边界

ABI 拒绝不再伪装成 `ToolGated`，而是进入结构化 `KernelFault`：`version_mismatch`、`operation_mismatch`、`invalid_lifecycle`、`invalid_config`、`duplicate_event_conflict`、`unexpected_effect_result`、`snapshot_incompatible`。

内核生命周期固定为 `Created -> Configured -> Running <-> Suspended -> Completed|Cancelled|Failed`。配置先整体校验再原子应用；workflow 不再隐式 auto-start；terminal 后不接受业务 mutation。

移除 public `state_machine_mut()` 及宿主对内部 struct 的直接依赖，以只读投影和明确命令替代：status、turn、rendered context、committed-message drain、local usage、snapshot。

### 7. 可靠性关联必须参与序列化与 replay

新的 input、observation 和 session event 必须：

- JSON round-trip 不丢字段；
- ABI v1 fixture 明确以 version mismatch 被拒绝，ABI v2 fixture 成为唯一 wire contract；
- OS snapshot 可按 delivery/reservation/operation ID 重建审计记录；
- uninterrupted 与 snapshot/restore 后的下一步 action/observation 等价；
- 不把 lease token、API key、路径或宿主 cancellation handle 写入 snapshot。

当前 `OsSnapshot` 仅保留为审计投影；新增稳定的 `KernelSnapshotV2` 恢复真实运行状态、event/effect 去重窗口与 terminal-report latch，不直接序列化内部 state-machine struct。

### 8. 有界状态与性能纪律

signal delivery/event dedupe 使用固定容量 replay window，不保留无界 `HashSet`；audit snapshot 中的历史明细采用有界窗口或聚合计数。render/compression/replay 的 clone 与 allocation 优化必须先建立基准，不凭静态观感微调。

### 9. 以纵向子切片迁移，不做横向大爆炸

实现顺序固定为：

1. ABI 模块拆分、v2 envelope/version gate、KernelFault、lifecycle 与状态封装；
2. effect protocol 与 Action/Observation 分离；
3. signal delivery disposition 与有界 dedupe；
4. budget grant enforcement 与 usage report；
5. operation cancellation state transition；
6. snapshot/replay/golden hardening 及 Node/Python host cutover；
7. 删除 ABI v1、legacy signal、base budget、observation-command 与 SDK fallback 残留。

每个子切片都必须先添加失败的契约测试，再做最小实现，并保持 workspace 可构建。

## 技术栈

- Rust 2024、Serde JSON、`deepstrike-core`
- napi-rs Node binding（`crates/deepstrike-node`）
- PyO3 Python binding（`crates/deepstrike-py`）
- Rust unit/integration tests、Jest、Pytest
- VitePress 文档与 docs drift checker

## 命令

```bash
# Core 与 Rust ABI/golden
cargo test -p deepstrike-core
cargo test -p deepstrike-tests t12_golden_fixtures

# 全 Rust workspace（与 CI 一致，排除 FFI/wasm 构建）
cargo test --workspace --exclude deepstrike-py --exclude deepstrike-node --exclude deepstrike-wasm

# Node binding + SDK
cargo check -p deepstrike-node
cd node && npm run build && npm test

# Python binding + SDK（本地虚拟环境已具备时）
cargo check -p deepstrike-py
cd python && pytest

# 文档
npm run docs:drift
npm run docs:build
```

## 项目结构

```text
crates/deepstrike-core/src/runtime/kernel.rs       ABI event/action/observation 与 step dispatch
crates/deepstrike-core/src/runtime/session.rs      durable session event
crates/deepstrike-core/src/runtime/replay.rs       OS snapshot fold 与 golden
crates/deepstrike-core/src/scheduler/state_machine/ 纯状态转换、signal/budget/cancel 语义
crates/deepstrike-node/src/                        napi JSON/typed binding
crates/deepstrike-py/src/lib.rs                    PyO3 JSON binding
tests/rust/fixtures/                               wire-format golden fixture
node/src/runtime/                                  Node host adoption
python/deepstrike/runtime/                         Python host adoption
docs/decisions/                                    ADR 与规格
```

## 代码风格

- ABI v2 直接表达最终契约，不为 v1 保留适配字段或分支。
- wire enum 使用 tagged union 与 `snake_case`；可选字段使用 `default + skip_serializing_if`。
- 状态机只接受事实并返回 action/observation；禁止在 core 内添加 I/O。
- Rust 内部只有一个 adjudication path，不维护 legacy/new 两套逻辑。
- 测试断言输入/输出与状态，不绑定内部函数调用顺序。

## 测试策略

1. **RED：ABI contract**——先为每个新 JSON input/observation 添加 round-trip/golden 测试。
2. **RED：state transition**——覆盖 signal accepted/queued/deduped/dropped、grant boundary、各 loop phase cancellation 和重复取消。
3. **GREEN：最小 core 实现**——先让 Rust 单元与 integration 通过。
4. **Binding parity**——Node/Python 对同一 ABI v2 fixture 生成等价 wire shape，并拒绝 v1 fixture。
5. **Replay differential**——比较 uninterrupted 与 restore 后的 action、observation 和 usage/correlation。
6. **Regression**——运行完整 Rust、Node、Python 与 docs 验证。

不以大面积 snapshot 更新代替精确断言；每个 golden 变化必须人工可读。

## 边界

### 始终执行

- ABI v1 input 必须稳定返回 version mismatch，不做隐式升级或降级。
- 所有外部副作用先产生带 `effect_id` 的 action，结果回灌后才记录事实 observation。
- 新行为先写失败测试；每个子切片保持 core、Node、Python 可构建。
- operation/delivery/reservation ID 只作 opaque correlation，日志中不包含凭证。
- terminal usage 使用内核本地计数，不重新读取宿主账本。

### 需要先确认

- ABI v2 发布后再次改变其已确认的 public shape。
- 新增依赖、改 CI、改变 snapshot 持久化格式的破坏性部分。
- 将某种 store、数据库、网络或进程控制逻辑移入 core。

### 永不执行

- 在 Rust core 内实现 delivery claim/ack、budget reserve/settle 或外部 I/O cancellation。
- 把 lease token、API key、文件路径或可执行 handle 持久化进 kernel snapshot。
- 用 timeout 或 critical signal 冒充所有取消原因。
- 静默接受 ABI v1，或在 Node/Python 中保留绕过 v2 contract 的隐藏 fallback。
- 通过 observation 或 public mutable state-machine API 驱动宿主副作用。

## 成功标准

- `KERNEL_ABI_VERSION == 2`，ABI v1 input 一律返回 version mismatch。
- core、Node、Python public surface 不再包含 legacy `signal`、`signal_disposed` 或 `group_*_base`。
- 每个 input、step、action/result 均可沿 operation/event/effect identity 追溯；重复输入和结果幂等，冲突重放产生结构化 fault。
- public surface 不再暴露 `state_machine_mut()`；配置与事件顺序受 lifecycle 约束。
- memory/workflow/preempt/approval/spool/page-out 等宿主 I/O 全部使用 action/result，不再用 observation 充当命令。
- 每次 `deliver_signal` 都能得到带同一 `delivery_id` 的唯一 disposition，redelivery 可区分。
- kernel 对 `budget_grant` 的三个轴执行本地硬限制，并输出与 `reservation_id` 关联的实际用量。
- `cancel_operation` 在 Reason、ToolAwait、SubAgentAwait、Workflow 等 phase 中幂等地产生同一取消终态。
- snapshot/restore 不丢 delivery/reservation/operation correlation，恢复后的下一步与不中断执行等价。
- `KernelSnapshotV2` 可恢复真实内核状态；signal/event dedupe 状态有界。
- Node 与 Python 只公开 ABI v2，并对 v1 rejection 与 v2 parity 有契约测试；完整 Rust/Node/Python/docs 验证通过。
- core 中没有新增持久化、网络、文件、provider 或 process side effect。

## 非目标

- 不在本切片中提供生产级 Redis/PostgreSQL budget store 或 signal store。
- 不改变 ReactiveSession 的产品级 turn policy；它只消费新的内核裁决。
- 不统一所有 session event 命名，也不重写整个 scheduler。
- 不为 ABI v1 提供 adapter、shim 或 deprecation window。

## 已确认事项

1. 该分支作为基于 `codex/runtime-contract-refactor` 的 stacked slice。
2. 实现顺序为 signal correlation、budget grant、cancellation/replay。
3. 已确认：直接切换 ABI v2，不做向后兼容。
