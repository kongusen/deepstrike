# ADR-003：内核可靠性参数的 SDK 配置边界

## 状态

由 ADR-005 取代

## 日期

2026-07-14

## 背景

已废弃的单版本 Kernel ABI 曾把 replay、effect correlation、恢复与结果外置收敛到内核。ADR-005 已替换该 wire；本 ADR 只保留“宿主资源策略应保持窄接口”的决策依据。

## 决策

在 `RunConfig.reliability` 下提供一个聚合的 `KernelReliabilityConfig`，只开放宿主需要承担资源或故障策略责任的参数：

- `event_replay_capacity`：输入事件幂等窗口；
- `completed_effect_replay_capacity`：已完成 effect 结果窗口；
- `provider_recovery_attempts`：provider 上下文溢出恢复次数；
- `output_recovery_attempts`：输出截断续写次数；
- `host_effect_retry_attempts`：page-out 等宿主持久化 effect 的失败重试次数；
- `max_input_bytes`：单个 ABI input 的 canonical JSON 字节上限，默认 16 MiB；typed 与 JSON 入口执行同一限制。
- `tail_bounds`：logical checkpoint 之间 journal tail 的条数/字节 soft watermark 与 hard limit。

宿主存储位置不进入内核策略；canonical host 暴露 `PayloadStore`，locator 对 core 保持 opaque。

宿主侧同样影响资源消耗、但不由 core 执行的策略不伪装成 kernel 参数。workflow
结构化输出校验次数由 Node/WASM 的 `workflowSchemaValidationAttempts` 和 Python 的
`workflow_schema_validation_attempts` 配置，范围 `1..=16`，默认 `2`。

配置在 ABI 边界整体校验后原子应用。窗口容量限制为 `1..=65536`，单输入限制为 `256 B..=64 MiB`，恢复次数最大为 `16`。tail hard limit 必须非零且不低于对应 soft watermark；默认 soft/hard 分别为 512/2048 records 与 4/16 MiB。字段缺省时保持内核默认值。

内核通过只读 `KernelDiagnostics` 暴露 input count/bytes、journal 高水位、replay/effect/pending 数量与生命周期。该投影不提供 setter，也不绕过 versioned input transaction。

checkpoint 和 canonical record 中的 64-bit 数值使用 `WireU64` 十进制字符串编码，避免 Node/WASM JSON 往返时发生 `Number` 精度丢失。

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
- 条数与字节双重限制避免单个超大 payload 绕过 checkpoint tail 资源边界；
- 非法组合在任何字段生效前返回 `invalid_config`；
- checkpoint 必须保存已解析策略及窗口内容，恢复后继续使用相同边界；
- Node/Python/Rust host 在 effect protocol cutover 时把各自 public options 映射到该 bundle。
