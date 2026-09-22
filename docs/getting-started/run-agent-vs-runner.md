# 选择 Agent API

现在所有普通调用都从一个可执行 Agent 开始。Agent 定义身份和能力，Run 表示一次目标执行，Session 表示连续对话，Workflow 表示多个 Agent 的协作。

## 最小调用

```ts
import { createAgent, OpenAIResponsesProvider } from "@deepstrike/sdk"

const agent = createAgent({
  name: "researcher",
  model: "openai/gpt-5-mini",
  runtimeBinding: { provider: new OpenAIResponsesProvider(process.env.OPENAI_API_KEY!, "gpt-5-mini") },
  instructions: "查证事实并给出来源。",
})

const result = await agent.run("解释这个问题")
console.log(result.output)
```

## 流式输出

```ts
for await (const event of agent.stream("解释这个问题")) {
  if (event.type === "text_delta") process.stdout.write(event.delta)
}
```

## 连续 Session

```ts
const session = agent.session("chat-1")
await session.run("我叫 Ada")
const reply = await session.run("我叫什么？")
```

## 高级能力

高级能力仍然通过 Agent 或 Session 的场景方法使用：

| 需求 | API |
|---|---|
| 持久记忆 | `agent.remember()`、`agent.recall()` |
| 人工审批和取消 | `agent.run({ onPermissionRequest })`、`session.interrupt()` |
| 委托专门任务 | `agent.delegate()` |
| 并行研究和依赖编排 | `agent.workflow()` |
| 低层 Kernel、Journal、Harness | `@deepstrike/sdk/advanced` |

普通应用不需要创建 RuntimeRunner、ExecutionPlane 或 Kernel 对象。
