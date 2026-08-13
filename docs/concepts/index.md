# 概念索引

Concepts 解释那些会影响 Agent 行为的 **设计选择**。它们位于能力指南和 API 参考之间。

如果把 [Agent 如何运行](../architecture/) 看成整体运行形状，那么 Concepts 回答的是：

- 一个 sub-agent 的权限边界到底由哪些字段决定？
- 为什么 Context 不是 chat log？
- 为什么 prompt cache 需要 frozen prefix？
- 多个 Agent run 如何共享累计预算？

## 推荐阅读

| 文档 | 代码主入口 | 说明 |
|------|------------|------|
| [角色与隔离](./roles-and-isolation) | `types/agent.rs`、`orchestration/workflow/`、`scheduler/tcb.rs` | sub-agent / workflow node 的 role、isolation、capability、trust 如何变成内核可执行约束 |
| [Prompt Cache 设计](./prompt-cache-design) | `context/renderer.rs`、`context/manager.rs`、`mm/handle.rs` | 四槽位渲染、state_turn、handle projection、frozen prefix 如何共同保护 cache |
| [RunGroup 预算](./run-group-budget) | `python/deepstrike/runtime/run_group.py`、`node/src/runtime/run-group.ts`、`scheduler/state_machine/gate.rs` | 多个 stateless run 如何共享累计 token / spawn 治理域 |

## 与架构页的区别

| 层次 | 关注点 |
|------|--------|
| Architecture | Agent run、Session 和协作流程如何组合 |
| Concepts | 某项能力为什么这样工作，哪些字段会影响它 |
| Guides | 怎么使用这些机制完成具体任务 |
| Reference | 类型、参数、事件字段的完整说明 |

## 代码事实优先

Concepts 页面遵循三个规则：

1. **以 Agent 行为为事实源**：示例和 public type 定义开发者可依赖的行为。
2. **明确应用责任**：模型调用、工具、文件系统、SessionLog、RunGroup store 由集成应用负责。
3. **写清楚默认值**：默认 role、默认 inheritance、默认预算和默认 cache 行为会改变用户看到的结果。
