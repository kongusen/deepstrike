# DeepStrike Node.js SDK — API 使用指南

> Runtime v1：公共入口为 `RuntimeRunner` + `SessionLog` + `ExecutionPlane`。详见 `node/README.md` 与 `docs/spec-runtime-v1.md`。

## 目录

1. [快速开始](#1-快速开始)
2. [Provider 配置](#2-provider-配置)
3. [RuntimeRunner 基础](#3-runtimerunner-基础)
4. [工具调用 (Tools)](#4-工具调用-tools)
5. [技能 (Skills)](#5-技能-skills)
6. [知识检索 (Knowledge)](#6-知识检索-knowledge)
7. [记忆系统 (Memory)](#7-记忆系统-memory)
8. [治理管线 (Governance)](#8-治理管线-governance)
9. [信号系统 (Signals)](#9-信号系统-signals)
10. [评估框架 (Harness)](#10-评估框架-harness)
11. [协作层 (Collaboration)](#11-协作层-collaboration)

---

## 1. 快速开始

```bash
npm install deepstrike
```

```typescript
import {
  OpenAIProvider,
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  collectText,
} from "deepstrike"

const provider = new OpenAIProvider({
  apiKey: "sk-your-key",
  model: "gpt-5-mini",
  baseUrl: "https://api.openai.com/v1",
})

const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 4096,
})

const result = await collectText(runner.run({
  sessionId: "demo",
  goal: "用一句话解释什么是 TypeScript",
}))
console.log(result)
```

---

## 2. Provider 配置

```typescript
import {
  OpenAIProvider,
  AnthropicProvider,
  QwenProvider,
  DeepSeekProvider,
  MiniMaxProvider,
  OllamaProvider,
  KimiProvider,
} from "deepstrike"

// OpenAI 或兼容代理
const provider = new OpenAIProvider({
  apiKey: process.env.OPENAI_API_KEY!,
  model: "gpt-5-mini",
  baseUrl: "https://xiaoai.plus/v1",
})

// 快捷构造
const qwen     = new QwenProvider({ apiKey: "key" })
const deepseek = new DeepSeekProvider({ apiKey: "key" })
const anthropic = new AnthropicProvider({ apiKey: "key" })
const ollama   = new OllamaProvider({ model: "llama3" })
```

### 自定义 Provider

实现 `LLMProvider` 接口：

```typescript
import type { LLMProvider, Message, ToolSchema, StreamEvent } from "deepstrike"

const myProvider: LLMProvider = {
  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncGenerator<StreamEvent> {
    // 实现流式调用
    yield { type: "text_delta", delta: "Hello" }
    yield { type: "done" }
  }
}
```

---

## 3. RuntimeRunner 基础

### 3.1 同步运行

```typescript
const result = await collectText(runner.run({ sessionId: "s1", goal: "Say hello" }))
```

### 3.2 流式运行

```typescript
let text = ""
for await (const event of runner.run({ sessionId: "s1", goal: "What is 2+2?" })) {
  switch (event.type) {
    case "text_delta":
      process.stdout.write(event.delta)
      text += event.delta
      break
    case "tool_call":
      console.log(`Tool: ${event.name}`)
      break
    case "tool_result":
      console.log(`Result: ${event.content}`)
      break
    case "done":
      console.log(`\n--- ${event.iterations} turns, ${event.totalTokens} tokens`)
      break
    case "error":
      console.error(`Error: ${event.message}`)
      break
  }
}
```

### 3.3 带 Criteria 运行

```typescript
for await (const event of runner.run({
  sessionId: "s1",
  goal: "打个招呼",
  criteria: ["必须包含 hello", "不超过 20 字"],
})) {
  // ...
}
```

### 3.4 Extensions

```typescript
const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 4096,
  extensions: { temperature: 0.1, top_p: 0.9 },
})
```

### 3.5 中断

```typescript
setTimeout(() => runner.interrupt(), 5000)
const result = await collectText(runner.run({ sessionId: "s1", goal: "Write a long essay..." }))
```

### 3.6 会话恢复

```typescript
// 同一 sessionId 会从 SessionLog 重放历史
await collectText(runner.run({ sessionId: "chat-1", goal: "My name is Ada." }))
await collectText(runner.run({ sessionId: "chat-1", goal: "What is my name?" }))

// 崩溃后从中断点继续（不重复插入 run_started）
for await (const e of runner.wake("chat-1")) { /* ... */ }
```

### 3.7 RuntimeOptions

```typescript
interface RuntimeOptions {
  provider: LLMProvider
  sessionLog: SessionLog
  executionPlane: ExecutionPlane
  maxTokens: number
  maxTurns?: number
  timeoutMs?: number
  extensions?: Record<string, unknown>
  skillDir?: string
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  dreamStore?: DreamStore
  agentId?: string
  governance?: Governance
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown>
}
```

---

## 4. 工具调用 (Tools)

### 4.1 使用 `tool()` 装饰器

```typescript
import { tool } from "deepstrike"

const add = tool({
  name: "add",
  description: "Add two integers and return the sum.",
  parameters: {
    type: "object",
    properties: {
      x: { type: "integer", description: "First number" },
      y: { type: "integer", description: "Second number" },
    },
    required: ["x", "y"],
  },
  execute: async (args) => {
    return String(args.x + args.y)
  },
})

const plane = new LocalExecutionPlane().register(add)
const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: plane,
  maxTokens: 4096,
})
```

### 4.2 内置 readFile 工具

```typescript
import { readFile } from "deepstrike"

plane.register(readFile())
```

### 4.3 取消注册

```typescript
plane.unregister("add")
```

---

## 5. 技能 (Skills)

```typescript
import { scanSkillDir } from "deepstrike"

const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 4096,
  skillDir: "./skills",   // 内核自动注入 `skill` meta-tool
})

// 手动扫描（可选）
const skills = await scanSkillDir("./skills")
console.log(skills) // [{ name: "summarize", description: "..." }, ...]
```

---

## 6. 知识检索 (Knowledge)

```typescript
import type { KnowledgeSource } from "deepstrike"

const myKnowledge: KnowledgeSource = {
  async retrieve(query: string, topK: number): Promise<string[]> {
    // 向量搜索、API 调用等
    return ["DeepStrike 是一个 Agent 框架。"]
  },
}

const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 4096,
  knowledgeSource: myKnowledge,
})
// 内核注入 `knowledge` meta-tool，LLM 按需检索
```

---

## 7. 记忆系统 (Memory)

### 7.1 WorkingMemory

```typescript
import { WorkingMemory } from "deepstrike"

const mem = new WorkingMemory()
mem.set("user_name", "Alice")
mem.get("user_name") // "Alice"
mem.clear()
```

### 7.2 DreamStore

```typescript
import type { DreamStore, SessionData, MemoryEntry, CurationResult } from "deepstrike"

class MyDreamStore implements DreamStore {
  async loadSessions(agentId: string): Promise<SessionData[]> { ... }
  async loadMemories(agentId: string): Promise<MemoryEntry[]> { ... }
  async commit(agentId: string, result: CurationResult, existing: MemoryEntry[]): Promise<void> { ... }
  async search(agentId: string, query: string, topK: number): Promise<MemoryEntry[]> { ... }
}

const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 4096,
  dreamStore: new MyDreamStore(),
  agentId: "my-agent-1",
})

// 触发记忆整合
const result = await runner.dream("my-agent-1", Date.now())
console.log(`${result.sessionsProcessed} sessions, ${result.insightsExtracted} insights`)
```

---

## 8. 治理管线 (Governance)

### 8.1 SDK PermissionManager

```typescript
import { PermissionManager, PermissionMode } from "deepstrike"

const pm = new PermissionManager(PermissionMode.Default)
pm.grant("fs", "read")
pm.grantWithApproval("db", "write", "需要 DBA 审批")
pm.revoke("db", "drop")

pm.evaluate("fs", "read")    // { allowed: true, ... }
pm.evaluate("fs", "write")   // { allowed: false, reason: "not granted" }
```

### 8.2 内核 Governance

```typescript
import { Governance } from "deepstrike"

const gov = new Governance("allow")  // 默认策略: "allow" | "deny"
gov.addPermissionRule("danger.*", "deny")
gov.blockTool("rm_rf")
gov.setRateLimit("api_call", 10, 60_000)

const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 4096,
  governance: gov,
})
// 每次 LLM 调用工具前，自动经过 Permission → Veto → RateLimit → Constraint 管线
```

---

## 9. 信号系统 (Signals)

```typescript
import { SignalGateway, ScheduledPrompt } from "deepstrike"

const gw = new SignalGateway()

// 定时调度
gw.schedule(new ScheduledPrompt("daily standup", Date.now() + 3600_000))

// 订阅
const rx = gw.subscribe()

// 注入外部信号
gw.ingest({ kind: "interrupt", payload: {}, priority: 10 })

// RuntimeRunner 集成
const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 4096,
  signalSource: rx,
})
// kind="interrupt" → 立即中断运行

gw.destroy()
```

---

## 10. 评估框架 (Harness)

### 10.1 SinglePassHarness

```typescript
import { SinglePassHarness } from "deepstrike"

const harness = new SinglePassHarness(runner)
const outcome = await harness.run({ goal: "Say hello" })
console.log(outcome.passed)  // true
console.log(outcome.result)
```

### 10.2 EvalLoopHarness

```typescript
import { EvalLoopHarness } from "deepstrike"

const harness = new EvalLoopHarness(runner, {
  gate: async (request, outcome) => outcome.result.includes("hello"),
  maxAttempts: 3,
})
const outcome = await harness.run({ goal: "Greet the user" })
```

### 10.3 HarnessLoop（LLM-as-Judge）

```typescript
import { HarnessLoop } from "deepstrike"

const harness = new HarnessLoop(runner, {
  evalProvider: evalProvider,
  maxAttempts: 3,
  skillDir: "./skills",  // 通过时自动提取技能
})

const outcome = await harness.run({
  goal: "Write a haiku about the ocean",
  criteria: ["Must be exactly 3 lines"],
})
console.log(outcome.passed, outcome.feedback)
```

---

## 流式事件类型

| 事件 | type 字段 | 主要字段 |
| --- | --- | --- |
| 文本片段 | `text_delta` | `delta: string` |
| 思维链 | `thinking_delta` | `delta: string` |
| 工具调用 | `tool_call` | `id, name, arguments` |
| 工具结果 | `tool_result` | `callId, content, isError` |
| 完成 | `done` | `iterations, totalTokens, status` |
| 错误 | `error` | `message` |
| 权限请求 | `permission_request` | `toolName, reason` |

---

## 11. 协作层 (Collaboration)

协作层提供多 Agent 协调能力。完整 API 参见 [collaboration.md](./collaboration.md)。

### 11.1 VerificationContract — 验证契约

```typescript
import { ContractBuilder } from "@deepstrike/sdk"

const contract = new ContractBuilder("report-v1", "撰写关于 X 的研究报告")
  .criterion("has-sources",      "报告引用至少 3 个来源", { weight: 0.4 })
  .criterion("no-hallucination", "所有结论均可追溯至引用", { weight: 0.6 })
  .antiPattern("不得伪造引用")
  .evidence("最终报告正文")
  .build()
```

### 11.2 AgentPool — 角色隔离的代理池

```typescript
import { AgentPool } from "@deepstrike/sdk"

function makeRunner(opts: Partial<RuntimeOptions> = {}) {
  return new RuntimeRunner({
    provider,
    sessionLog: new InMemorySessionLog(),
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 4096,
    ...opts,
  })
}

const pool = new AgentPool()
  .add("executor", makeRunner({ maxTokens: 32_000, skillDir: "./skills" }))
  .add("verifier", makeRunner({ maxTokens: 8_000 }))
```

### 11.3 CreatorVerifierMode — 双 Agent 协作

```typescript
import { CreatorVerifierMode, HandoffBus } from "@deepstrike/sdk"

const mode = new CreatorVerifierMode(pool, { maxAttempts: 3 })
const outcome = await mode.run(contract)

console.log(outcome.success)           // true / false
console.log(outcome.attemptsUsed)      // 实际尝试次数
console.log(outcome.checkResults)      // ContractCheckResult[] — 每条标准的审核结果
console.log(outcome.handoff)           // HandoffArtifact — 可传递给下一个 sprint

// 漂移监控
const metrics = mode.getMetrics()      // { total, failed, driftRate }
if (mode.isDrifting(0.05)) {
  // driftRate > 5% — 暂停自动委派，升级人工审核
}

// 交接协议
if (HandoffBus.requiresEscalation(outcome.handoff)) {
  console.log("Blocked on:", outcome.handoff.blockedOn)
}
const note = HandoffBus.toContextNote(outcome.handoff)
// 注入下一轮 Agent 的 working 分区
```

### 11.4 OrchestrationMode — 三角色完整流

编排者（orchestrator）从原始目标生成 VerificationContract，然后由 CreatorVerifierMode 执行。

```typescript
import { AgentPool, OrchestrationMode } from "@deepstrike/sdk"

const pool = new AgentPool()
  .add("orchestrator", new RuntimeRunner({ provider: reasonerProvider, sessionLog: new InMemorySessionLog(), executionPlane: new LocalExecutionPlane(), maxTokens: 8_000 }))
  .add("executor",     new RuntimeRunner({ provider: executorProvider, sessionLog: new InMemorySessionLog(), executionPlane: new LocalExecutionPlane(), maxTokens: 32_000 }))
  .add("verifier",     new RuntimeRunner({ provider: verifierProvider, sessionLog: new InMemorySessionLog(), executionPlane: new LocalExecutionPlane(), maxTokens: 8_000 }))

const mode = new OrchestrationMode(pool)
const { outcome, contract } = await mode.run("为新能源汽车行业撰写市场分析")

console.log(contract.id, outcome.success)
```

### 11.5 HandoffBus — 统一交接面

```typescript
import { HandoffBus } from "@deepstrike/sdk"

// 从 ContractDrivenHarness 结果构建
const handoff = HandoffBus.fromContractOutcome({ contract, checkResults, artifact, success })

// 从子 Agent 最终消息构建
const handoff = HandoffBus.fromSubAgentResult({ goal, finalMessage, sprint: 2 })

// 从 dream 整合结果构建
const handoff = HandoffBus.fromDream({ goal, dreamResult })

// 渲染为上下文注入字符串
const note = HandoffBus.toContextNote(handoff)

// 检查是否需要升级
if (HandoffBus.requiresEscalation(handoff, { driftThreshold: 0.05 })) { ... }
```
