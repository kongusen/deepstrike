# Node / Python / Rust / WASM SDK 体系化回看

## 结论

四个 SDK 已经共享同一套 `deepstrike-core` 状态机、session event、canonical journal、execution plane、memory protocol 和 workflow wire contract。对齐的目标应是 **能力和语义对齐**，而不是把 TypeScript/Python API 原样翻译成 Rust。

本轮回看确认：WASM 已基本覆盖 Node/Python 的 Agent facade；Rust 现在覆盖稳定 session、运行选项、memory、workflow 状态机和 signal claim/ack。Rust 继续保留显式 executor、trait 和 ownership 语义。

## 公共能力矩阵

| 能力 | Node | Python | WASM | Rust | 判定 |
|---|---:|---:|---:|---:|---|
| Agent / Session 对象模型 | ✅ | ✅ | ✅ | ✅ | 四端都有稳定入口；Rust `stream_session` 保证流式调用保留 session identity |
| run / stream | ✅ | ✅ | ✅ | ✅ | 事件均落到共享 runtime |
| per-run attachments / provider options | ✅ | ✅ | ✅ | ✅ | Rust 为 `AgentRunOptions` |
| durable resume / history | ✅ | ✅ | ✅ | ✅ | session log + journal |
| interrupt / cancellation | ✅ | ✅ | ✅ | ✅ | Rust 使用显式 cancellation reason |
| structured output validation | ✅ | ✅ | ✅ | ✅ | Rust 通过 `AgentRunOptions::output_schema` 返回 typed validation |
| usage / evidence result projection | ✅ | ✅ | ✅ | ✅ | Rust 从 `ContextPrepared` 和 terminal 事件投影 usage/evidence |
| remember / recall | ✅ | ✅ | ✅ | ✅ | Rust 支持 typed `MemoryRecord` / `MemoryQuery` |
| workflow driver | ✅ | ✅ | ✅ | ⚠️ | Rust 暴露 `AgentWorkflow` 手动 executor，不强行引入 async runtime |
| workflow trace / replay | ✅ | ✅ | ✅ | ⚠️ | Rust 已有 typed session replay；workflow 专用 trace 仍待 typed event contract |
| delegate / handoff | ✅ | ✅ | ✅ | ✅ | Rust 通过显式 `AgentResolver` 注入 host 查找能力 |
| listen / signal lease | ✅ | ✅ | ✅ | ✅ | Rust 已补 claim → run → ack/nack |
| MCP / execution plane | ✅ | ✅ | ✅ | ✅ | Rust 以 trait 和 proxy plane 暴露 |
| close / resource lifecycle | ✅ | ✅ | ✅ | ⚠️ | Rust 的 plane 生命周期由宿主持有 |

## 语义边界

### Rust

Rust 的 `RuntimeRunner` 是强类型、可组合的 runtime driver。`AgentSession` 和 `AgentWorkflow` 只做对象模型封装，不复制状态机。workflow 不自动创建 tokio task，也不假设并发执行器；调用方按 `ready_batch`、`spawn_info`、`record_completion` 推进，这与 Rust 的 ownership 和宿主控制模型一致。

剩余差距集中在 host contract，而不是内核能力缺失：delegate 需要先定义 Rust resolver trait；专用 workflow trace/replay 也应建立在已有 `SessionEvent` 上。

### WASM

WASM 的 Agent facade 已覆盖 Node/Python 的主要 public contract，包括 output schema、evidence、memory、delegate、listen、close、workflow 和 session replay。WASM 通过 `AbortSignal`、`AsyncIterable`、结构化对象保持浏览器/Worker 习惯，不应复制 Rust 的 trait API。

## 下一步顺序

1. Rust 添加 workflow trace/replay 的 typed projection。
2. Rust 的 close 只在 execution plane/MCP trait 提供明确 shutdown contract 后实现；当前不伪造 close。

每一步都应先补共享 fixture 或 Rust 行为测试，再更新矩阵，避免只增加同名 API。
