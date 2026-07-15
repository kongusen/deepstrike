# ADR-003：内核可靠性参数的 SDK 配置边界

## 状态

Accepted

## 日期

2026-07-14

## 背景

Kernel ABI v2 将 replay、effect correlation、恢复与大结果 spool 收敛到内核后，部分原本分散在实现中的常量开始直接影响宿主的资源占用与故障策略。固定常量无法同时适配边缘设备、单进程 SDK 和长生命周期服务，但把所有内部阈值公开又会泄漏实现细节并扩大公共契约。

## 决策

在 `RunConfig.reliability` 下提供一个聚合的 `KernelReliabilityConfig`，只开放宿主需要承担资源或故障策略责任的参数：

- `event_replay_capacity`：输入事件幂等窗口；
- `completed_effect_replay_capacity`：已完成 effect 结果窗口；
- `provider_recovery_attempts`：provider 上下文溢出恢复次数；
- `output_recovery_attempts`：输出截断续写次数；
- `host_effect_retry_attempts`：spool/page-out 等宿主持久化 effect 的失败重试次数；
- `spool_threshold_bytes` 与 `spool_preview_bytes`：大结果外置阈值和上下文预览大小。
- `snapshot_input_limit`：`KernelSnapshotV2` 为确定性重建保留的已接受 ABI 事务上限。
- `max_input_bytes`：单个 ABI input 的 canonical JSON 字节上限，默认 16 MiB；typed 与 JSON 入口执行同一限制。
- `snapshot_journal_bytes_limit`：快照事务日志的累计 canonical JSON 字节上限，默认 64 MiB。

宿主存储位置不进入内核策略；各 SDK 必须提供显式的 spool 目录配置。Rust 通过
`RuntimeOptions.spool_dir` 配置，未设置时使用 `.spool`。

宿主侧同样影响资源消耗、但不由 core 执行的策略不伪装成 kernel 参数。workflow
结构化输出校验次数由 Node/WASM 的 `workflowSchemaValidationAttempts` 和 Python 的
`workflow_schema_validation_attempts` 配置，范围 `1..=16`，默认 `2`。

配置在 ABI 边界整体校验后原子应用。窗口容量限制为 `1..=65536`，snapshot 输入上限为 `1..=100000`（默认 `10000`），单输入限制为 `256 B..=64 MiB`，journal 字节限制为 `256 B..=1 GiB`，恢复次数最大为 `16`，spool preview 必须非零且不大于 threshold。降低限制时必须容纳已经提交的 journal 以及当前配置事务；字段缺省时保持内核默认值。

内核通过只读 `KernelDiagnostics` 暴露 input count/bytes、journal 高水位、replay/effect/pending 数量与生命周期。该投影不提供 setter，也不绕过 versioned input transaction。

`KernelSnapshotV2.initial_policy` 中的 64-bit budget 轴使用十进制字符串编码，避免 Node/WASM JSON 往返时发生 `Number` 精度丢失。

已有独立公共策略继续使用原入口：signal 队列属于 attention policy，repeat fuse、entropy watch、scheduler budget 和 resource quota 不重复放入 reliability bundle。

以下参数保持实现内部：序列化版本、熵公式常量、渲染 preview、任务状态展示条数、短诊断文本长度和安全截断算法细节。它们不代表宿主资源承诺，也不应成为 SDK 兼容契约。

## 备选方案

### 每个参数增加一个 `Set*` event

拒绝。它会恢复此前离散配置事件的问题，难以保证跨字段约束和原子应用。

### 暴露全部常量

拒绝。实现细节一旦可观察就会形成事实 API，并妨碍后续算法替换。

### 只允许编译期配置

拒绝。Node、Python 和 Rust SDK 的部署资源差异发生在运行时，编译期常量无法表达每个 run 的策略。

## 影响

- SDK 可按 run 调整可靠性内存上限和恢复策略；
- 条数与字节双重限制避免单个超大 payload 绕过 snapshot 资源边界；
- 非法组合在任何字段生效前返回 `invalid_config`；
- snapshot 必须保存已选策略及窗口内容，恢复后继续使用相同边界；
- Node/Python/Rust host 在 effect protocol cutover 时把各自 public options 映射到该 bundle。
