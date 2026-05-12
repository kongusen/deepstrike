# DeepStrike Node.js SDK — API 使用指南

## 目录

1. [快速开始](#1-快速开始)
2. [Provider 配置](#2-provider-配置)
3. [Agent 基础](#3-agent-基础)
4. [工具调用 (Tools)](#4-工具调用-tools)
5. [技能 (Skills)](#5-技能-skills)
6. [知识检索 (Knowledge)](#6-知识检索-knowledge)
7. [记忆系统 (Memory)](#7-记忆系统-memory)
8. [治理管线 (Governance)](#8-治理管线-governance)
9. [信号系统 (Signals)](#9-信号系统-signals)
10. [评估框架 (Harness)](#10-评估框架-harness)

---

## 1. 快速开始

```bash
npm install deepstrike
```

```typescript
import { Agent, OpenAIProvider } from "deepstrike"

const provider = new OpenAIProvider({
  apiKey: "sk-your-key",
  model: "gpt-5-mini",
  baseUrl: "https://api.openai.com/v1",
})

const agent = new Agent(provider, { maxTokens: 4096 })
const result = await agent.run("用一句话解释什么是 TypeScript")
console.log(result) // => "done in 1 turns (completed)"
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

## 3. Agent 基础

### 3.1 同步运行

```typescript
const agent = new Agent(provider, { maxTokens: 4096 })
const result = await agent.run("Say hello")
// => "done in 1 turns (completed)"
```

### 3.2 流式运行

```typescript
let text = ""
for await (const event of agent.runStreaming("What is 2+2?")) {
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
for await (const event of agent.runStreaming("打个招呼", {
  criteria: ["必须包含 hello", "不超过 20 字"],
})) {
  // ...
}
```

### 3.4 Extensions

```typescript
const agent = new Agent(provider, {
  maxTokens: 4096,
  extensions: { temperature: 0.1, top_p: 0.9 },
})
```

### 3.5 中断

```typescript
setTimeout(() => agent.interrupt(), 5000)
const result = await agent.run("Write a long essay...")
```

### 3.6 遥测

```typescript
// 运行期间实时查看
console.log(agent.turn)     // 当前轮次
console.log(agent.pressure) // 上下文压力 [0-1]
```

### 3.7 AgentOptions

```typescript
interface AgentOptions {
  maxTokens: number              // 上下文窗口大小
  maxTurns?: number              // 最大轮次（默认 25）
  timeoutMs?: number             // 超时毫秒
  extensions?: Record<string, unknown>  // LLM 参数
  skillDir?: string              // 技能目录
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  dreamStore?: DreamStore
  agentId?: string
  governance?: Governance
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

const agent = new Agent(provider, { maxTokens: 4096 })
agent.register(add)
```

### 4.2 内置 readFile 工具

```typescript
import { readFile } from "deepstrike"

agent.register(readFile())
```

### 4.3 取消注册

```typescript
agent.unregister("add")
```

---

## 5. 技能 (Skills)

```typescript
import { Agent, scanSkillDir } from "deepstrike"

const agent = new Agent(provider, {
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

const agent = new Agent(provider, {
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

const agent = new Agent(provider, {
  maxTokens: 4096,
  dreamStore: new MyDreamStore(),
  agentId: "my-agent-1",
})

// 触发记忆整合
const result = await agent.dream("my-agent-1", Date.now())
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

const agent = new Agent(provider, {
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

// Agent 集成
const agent = new Agent(provider, {
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

const harness = new SinglePassHarness(agent)
const outcome = await harness.run({ goal: "Say hello" })
console.log(outcome.passed)  // true
console.log(outcome.result)
```

### 10.2 EvalLoopHarness

```typescript
import { EvalLoopHarness } from "deepstrike"

const harness = new EvalLoopHarness(agent, {
  gate: async (request, outcome) => outcome.result.includes("hello"),
  maxAttempts: 3,
})
const outcome = await harness.run({ goal: "Greet the user" })
```

### 10.3 HarnessLoop（LLM-as-Judge）

```typescript
import { HarnessLoop } from "deepstrike"

const harness = new HarnessLoop(agent, {
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
|------|-----------|----------|
| 文本片段 | `text_delta` | `delta: string` |
| 思维链 | `thinking_delta` | `delta: string` |
| 工具调用 | `tool_call` | `id, name, arguments` |
| 工具结果 | `tool_result` | `callId, content, isError` |
| 完成 | `done` | `iterations, totalTokens, status` |
| 错误 | `error` | `message` |
| 权限请求 | `permission_request` | `toolName, reason` |
