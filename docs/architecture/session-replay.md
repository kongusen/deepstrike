# Session 与恢复

Agent Session 是由 `sessionId` 标识的一条持久工作线程。它包含目标、对话 turn、工具活动、审批、Signal、Memory 活动、工作流进度，以及在中断后继续执行所需的恢复状态。

## Session 能解决什么

| 需求 | Session 行为 |
| --- | --- |
| 继续对话 | 复用同一个 `sessionId`，Agent 可以看到此前仍然有用的 Context。 |
| 恢复中断运行 | 再次启动同一个 Session，恢复未完成工作和最近的持久边界。 |
| 等待人或事件 | 在审批、子 Agent 完成或外部 Signal 处暂停，之后继续。 |
| 调试决策 | 查看模型输出、工具调用、策略决策和结果等结构化事件。 |
| 不接 Provider 测试 | 回放固定 Provider 响应，稳定验证 Agent 决策。 |

## Session 实现

| 类型 | 用途 |
| --- | --- |
| `InMemorySessionLog` | 本地实验和测试。 |
| `FileSessionLog` | 需要跨进程重启保持连续性的本地应用。 |
| 自定义 `SessionLog` | 将 Session 存入数据库或服务的应用。 |

## 恢复 Session

```ts
await collectText(runner.run({
  sessionId: "research-42",
  goal: "继续来源审查并完成 brief。",
}))
```

进程重启后继续使用相同的 `sessionId`。应用不需要手动重建对话，也不需要编造特殊的“resume” prompt。

## 持久 Memory 与 Session 的区别

Session 历史回答“这次运行发生了什么”。持久 Memory 回答“未来运行时 Agent 应该记住什么”。使用 [MemoryStore](../guides/memory) 保存值得跨运行携带的事实和偏好，把临时工具输出留在 Session 中。

## 延伸阅读

- [长时间运行 Session 指南](../guides/session-replay-and-recovery)
- [Context 与多模态输入](../guides/context-engineering)
- [评估与 Replay](../guides/harness-and-eval)
