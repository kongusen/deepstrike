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

宿主存储位置不进入内核策略；各 SDK 必须提供显式的 spool 目录配置。Rust 通过
`RuntimeOptions.spool_dir` 配置，未设置时使用 `.spool`。

配置在 ABI 边界整体校验后原子应用。窗口容量限制为 `1..=65536`，恢复次数最大为 `16`，spool preview 必须非零且不大于 threshold。字段缺省时保持内核默认值。

已有独立公共策略继续使用原入口：signal 队列属于 attention policy，repeat fuse、entropy watch、scheduler budget 和 resource quota 不重复放入 reliability bundle。

以下参数保持实现内部：序列化版本、熵公式常量、渲染 preview、任务状态展示条数和文本截断细节。它们不代表宿主资源承诺，也不应成为 SDK 兼容契约。

## 备选方案

### 每个参数增加一个 `Set*` event

拒绝。它会恢复此前离散配置事件的问题，难以保证跨字段约束和原子应用。

### 暴露全部常量

拒绝。实现细节一旦可观察就会形成事实 API，并妨碍后续算法替换。

### 只允许编译期配置

拒绝。Node、Python 和 Rust SDK 的部署资源差异发生在运行时，编译期常量无法表达每个 run 的策略。

## 影响

- SDK 可按 run 调整可靠性内存上限和恢复策略；
- 非法组合在任何字段生效前返回 `invalid_config`；
- snapshot 必须保存已选策略及窗口内容，恢复后继续使用相同边界；
- Node/Python/Rust host 在 effect protocol cutover 时把各自 public options 映射到该 bundle。
