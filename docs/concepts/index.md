# 概念索引

Concepts 解释 DeepStrike 代码里那些会影响系统行为的 **设计概念**。它们比 API 参考更高层，比架构页更贴近实现。

如果把 [Architecture](../architecture/) 看成 Agent OS 的整体形状，那么 Concepts 回答的是：

- 一个 sub-agent 的权限边界到底由哪些字段决定？
- 为什么 Context 不是 chat log？
- 为什么 prompt cache 需要 frozen prefix？
- RunGroup 为什么在 SDK 里，而不是在 kernel 里持久化？

## 推荐阅读

| 文档 | 代码主入口 | 说明 |
|------|------------|------|
| [角色与隔离](./roles-and-isolation) | `types/agent.rs`、`orchestration/workflow/`、`scheduler/tcb.rs` | sub-agent / workflow node 的 role、isolation、capability、trust 如何变成内核可执行约束 |
| [Prompt Cache 设计](./prompt-cache-design) | `context/renderer.rs`、`context/manager.rs`、`mm/handle.rs` | 四槽位渲染、state_turn、handle projection、frozen prefix 如何共同保护 cache |
| [RunGroup 预算](./run-group-budget) | `python/deepstrike/runtime/run_group.py`、`node/src/runtime/run-group.ts`、`scheduler/state_machine/gate.rs` | 多个 stateless run 如何共享累计 token / spawn 治理域 |

## 与架构页的区别

| 层次 | 关注点 |
|------|--------|
| Architecture | 为什么 DeepStrike 是 Agent OS 微内核，kernel / host 如何分层 |
| Concepts | 某个机制在代码中如何表达，哪些字段是事实源，哪些行为由 host 执行 |
| Guides | 怎么使用这些机制完成具体任务 |
| Reference | 类型、参数、事件字段的完整说明 |

## 代码事实优先

Concepts 页面遵循三个规则：

1. **以 core 类型为事实源**：Rust `deepstrike-core` 定义内核语义。
2. **明确 host 责任**：LLM、工具、文件系统、SessionLog、RunGroup store 都由 SDK 执行。
3. **写清楚默认值**：默认 role、默认 inheritance、默认预算和默认 cache 行为会改变用户看到的结果。

