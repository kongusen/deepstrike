# WASM SDK 公共 API 对齐矩阵

WASM 与 Node/Python 共用 Canonical Kernel 和 SessionEvent vocabulary，API 采用 TypeScript/浏览器习惯命名。

| 能力 | WASM | Node | Python | 说明 |
| --- | --- | --- | --- | --- |
| Agent declaration | `Agent` / `AgentOptions` | `Agent` / `AgentDefinition` | `Agent` / `AgentDefinition` | 三者均在宿主边界解析 provider |
| Session | `AgentSession` | `AgentSession` | `AgentSession` | 绑定同一个 `SessionLog` |
| Run result | `AgentRunResult` | `RunResult` | structured dict | status、run id、usage、validation |
| Streaming | `stream()` | `stream()` | `stream()` | 事件来自同一 RuntimeRunner |
| Resume | `resume()` | `resume()` | `resume()` | 通过 canonical journal 恢复 |
| Memory | `remember()` / `recall()` | `remember()` / `recall()` | `remember()` / `recall()` | WASM 通过宿主 `Memory` adapter |
| Handoff | `delegate()` | `delegate()` | `delegate()` | 目标必须在 declaration 中声明 |
| Signals | `listen()` | `listen()` | `listen()` | claim → run → ack/nack |
| Workflow | `workflow()` / replay | `workflow()` | `workflow()` / replay | 生命周期事件持久化到 SessionLog |
| Cancellation | `AbortSignal` / interrupt | `AbortSignal` / interrupt | `asyncio.Event` / interrupt | 语言运行时适配，不改变 Kernel 语义 |
| MCP | HTTP/SSE/custom factory | stdio/host transports | stdio + custom factory | WASM 禁止依赖 subprocess/filesystem |
| Durable session | `SessionLog` adapter | `SessionLog` adapter | `SessionLog` adapter | WASM 默认内存实现，宿主可注入持久化实现 |
| Payload | `PayloadStore` driver | payload store | payload store | driver 由宿主提供 |

## 当前限制

- Rust `wasm32-unknown-unknown` binding 和 golden fixture sweep 由 CI 验证。
- WASM MCP 的实际 HTTP/SSE 协议实现由宿主 factory 提供，SDK 只负责连接生命周期和 ExecutionPlane 语义。
