# Rust SDK 能力对齐矩阵

Rust SDK 和 Node、Python、WASM 共用 `deepstrike-core` 的状态机、事件协议、执行平面和持久化模型。Rust 保留更底层的可组合能力，同时提供 `Agent` / `AgentSession` 作为稳定的高层入口。

| 能力 | Rust API | 对齐状态 | 说明 |
|---|---|---:|---|
| Agent 构造 | `Agent::new(RuntimeOptions)` / `Agent::with_runner` | ✅ | 不复制 runtime，直接持有 canonical `RuntimeRunner` |
| Session | `Agent::session(id)` | ✅ | session id 由调用方控制，可跨调用恢复 |
| 单次运行 | `Agent::run` / `AgentSession::run` | ✅ | 返回文本、run id、状态、迭代次数、usage、evidence 和 validation |
| 单次运行选项 | `AgentRunOptions` | ✅ | criteria、extensions、attachments 按运行传入 |
| 流式运行 | `Agent::stream` / `AgentSession::stream` | ✅ | 复用 `RunEvent`，保留 Rust `Stream` 语义 |
| 恢复运行 | `AgentSession::resume` | ✅ | 调用 durable session projection + canonical journal |
| 历史与序号 | `history` / `latest_seq` | ✅ | 读取 `SessionEntry`，支持审计和证据投影 |
| Replay projection | `replay_messages` / `recorded_messages` / `is_mid_run` | ✅ | 从 durable session projection 恢复上下文和 provider replay |
| 中断 | `AgentSession::interrupt` | ✅ | 委托给 runner 的 cancellation reason 机制 |
| 信号监听 | `Agent::listen` / `RuntimeRunner::listen` | ✅ | claim → run → ack；失败走 nack，保持租约语义 |
| Memory | `RuntimeRunner::write_memory` / `query_memory` | ✅ | 共享 core memory policy、校验、配额和审计 |
| Session Memory facade | `AgentSession::remember` / `recall` | ✅ | 绑定当前 session，仍由 runner 执行校验与审计 |
| MCP / 执行平面 | `ExecutionPlane`、`McpProxyPlane` | ✅ | Rust 侧保留 trait 与宿主控制能力 |
| Durable session log | `InMemorySessionLog` / `FileSessionLog` | ✅ | 可替换持久化实现 |
| Workflow 状态机 | `AgentWorkflow`、`WorkflowRun`、`WorkflowSpec` | ✅ | 提供 Agent 入口和手动 executor 组合 |
| Workflow batteries-included driver | 暂无 | 设计保留 | Rust 的 executor/并发模型与 Node/Python 不同；需要真实消费者后再加 typed driver |
| Provider / tool / governance | `RuntimeOptions` 及现有模块 | ✅ | provider、tool、治理、配额、信号均走同一内核 |

## 设计边界

`RuntimeRunner` 是完整的低层驱动，适合需要精细控制执行平面、日志、journal、provider 和治理策略的 Rust 宿主。`AgentSession` 只增加对象模型和常用操作，不绕过 runner，也不维护第二套状态。

Rust 的 workflow 继续暴露纯状态机和手动 executor 接口。Node、Python、WASM 的高层 workflow driver 依赖各自的异步任务模型；Rust 只有在并发、取消、错误传播和持久化语义有明确宿主需求时才增加对应的 typed driver。

## 验证

```bash
cargo check -p deepstrike-sdk
```

高层 facade 的行为最终应通过共享 conformance fixture 和 `SessionEvent` / `RunEvent` 投影验证，避免只验证类型是否能编译。
