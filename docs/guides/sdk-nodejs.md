# DeepStrike Node.js SDK — API 使用指南

> Runtime v1：公共入口为 `RuntimeRunner` + `SessionLog` + `ExecutionPlane`。  
> **0.2.6 Agent OS：** 默认加载 `governancePolicy` + in-kernel `attentionPolicy`；支持 Layer-1 spool、semantic page-out、`writeMemory` / `queryMemory`。概念总览见 [Agent OS](../concepts/agent-os.md) 与 `node/README.md`。

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
12. [进阶特性 (Milestones, Sub-agents, Artifacts)](#12-进阶特性-milestones-sub-agents-artifacts)
13. [动态工作流 (Dynamic Workflows)](#13-动态工作流-dynamic-workflows)

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

内核采用**四槽模型**（见 [context-slots-compression.md](../concepts/context-slots-compression.md)）。知识有两条路径：

| 路径 | 落地位置 |
|------|----------|
| `knowledge(query)` meta-tool | **history**（tool result，模型需在对话流中看到） |
| `initialMemory` / `add_knowledge_message` | **Slot 2**（`systemKnowledge`，Anthropic 可 cache） |

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

SDK 侧临时键值存储。**不是**内核已删除的 `working` 分区。结构化任务状态在 `task_state` 中，渲染进 Slot 3（`turns[0]`）。

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
  initialMemory: ["Prior session: user prefers JWT with RS256."],  // Slot 2
})

// memory(query) 检索结果 → history；initialMemory → Slot 2
// 触发记忆整合
const result = await runner.dream("my-agent-1", Date.now())
console.log(`${result.sessionsProcessed} sessions, ${result.insightsExtracted} insights`)
```

### 7.3 Phase-7 记忆 syscall（`writeMemory` / `queryMemory`）

主 tool loop 之外的长期记忆 I/O，经内核校验后写入 `DreamStore`：

```typescript
await runner.writeMemory({
  kind: "user",
  content: "User prefers chartreuse.",
  metadata: { source: "onboarding" },
})

const entries = await runner.queryMemory("color preferences")
```

Session 事件：`memory_written`、`memory_queried`、`memory_validation_failed`、`memory_retrieval_result`。

### 7.4 Layer-1 大结果 spool

单条 tool result 超过阈值时，内核保留预览 + spool 引用，完整内容写入 `.spool/`。`LocalExecutionPlane` 的 `read_file` 可透明读取 spool 路径。可选 `resultSpool` 自定义目录。

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

### 8.2 内核 Governance（推荐：`governancePolicy`）

**0.2.6 默认：** 每次 `run()` 加载 `DEFAULT_NATIVE_GOVERNANCE_POLICY`（allow-all）到内核，工具执行前经 in-kernel gate 裁决。

```typescript
import {
  DEFAULT_NATIVE_GOVERNANCE_POLICY,
  type GovernancePolicy,
} from "@deepstrike/sdk"

const policy: GovernancePolicy = {
  rules: [
    { pattern: "read_file", action: "allow" },
    { pattern: "write_file", action: "ask_user" },
    { pattern: "*", action: "deny" },
  ],
}

const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 4096,
  governancePolicy: policy,  // 省略时使用 DEFAULT_NATIVE_GOVERNANCE_POLICY
})
```

`Governance` 类用于 SDK 侧独立评估（测试/自定义门），**不会**自动接入 `RuntimeRunner` — 运行时使用 `governancePolicy`。

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

### 10.4 子 Agent × HarnessLoop

配置 `subAgentHarness` 后，`spawnSubAgent()` 会自动通过 `HarnessLoop`（内核评估原语 `buildEvalMessages` / `parseVerdict`）评估子 agent 输出；criteria 来自 `AgentRunSpec.milestones.phases[].criteria`。未配置时走原有直接运行路径。

```typescript
const runner = new RuntimeRunner({
  provider,
  sessionLog,
  executionPlane,
  maxTokens: 4096,
  subAgentHarness: { evalProvider, maxAttempts: 3 },
})
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
// 注入下一轮 Agent 的 State turn（Slot 3 / turns[0]）
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
if (HandoffBus.requiresEscalation(handoff, { driftThreshold: 0.05 })) {
  // ...
}
```

---

## 12. 进阶特性 (Milestones, Sub-agents, Artifacts)

### 12.1 里程碑合约 (Milestones)

里程碑合约可以将 Agent 的运行划分为多个阶段（Phases），并且每个阶段需要显式验证。

```typescript
import { RuntimeRunner, milestoneCheckPass } from "@deepstrike/sdk"

const runner = new RuntimeRunner({
  provider,
  sessionLog,
  executionPlane,
  maxTokens: 4096,
  milestonePolicy: "require_verifier", // 策略可选 "require_verifier" | "auto_pass" | "terminate"
  milestoneContract: {
    phases: [
      {
        id: "phase-1",
        criteria: ["生成符合规范的方案草案"],
        requiredEvidence: ["draft_design.md"],
        unlocks: ["write_file"], // 这一阶段通过后解锁 write_file 能力
      }
    ]
  },
  onMilestoneEvaluate: async (ctx) => {
    console.log(`正在验证阶段 ${ctx.phaseId}:`, ctx.criteria)
    // 返回评估结果
    return milestoneCheckPass(ctx.phaseId)
  }
})
```

如果未配置 `onMilestoneEvaluate` 并且策略是 `require_verifier`，当运行到达里程碑需要验证时，runner 运行会挂起并返回 `milestone_pending` 状态：
```typescript
for await (const evt of runner.run({ sessionId: "s1", goal: "write a design" })) {
  if (evt.type === "done" && evt.status === "milestone_pending") {
    // 运行挂起，可通过 wake 恢复
  }
}
```

### 12.2 子智能体隔离与生成 (Sub-agents)

Node.js SDK 支持完全隔离的子智能体生成，并遵循内核 Isolation Manifest 过滤其拥有的能力：

```typescript
const runner = new RuntimeRunner({
  provider,
  sessionLog,
  executionPlane,
  maxTokens: 4096,
  // 可选：子 agent 自动走 HarnessLoop 评估
  subAgentHarness: { evalProvider, maxAttempts: 3 },
})

const spec = {
  identity: { agentId: "sub-worker-1", sessionId: "sub-session-001" },
  role: "implement",
  goal: "写一份文件",
  isolation: "read_only", // 隔离级别
  milestones: { phases: [{ id: "p1", criteria: ["文件包含完整章节"] }] },
}

// 必须在父智能体运行的 context 中调用
for await (const evt of runner.spawnSubAgent(spec)) {
  if (evt.type === "done") {
    console.log(evt.status)
  }
}
```

### 12.3 产物推送 (Artifacts)

> **已移除。** 四槽重构后 artifacts 分区已移除。请使用 `initialMemory` → Slot 2，或依赖 history 压缩 tier 处理大输出。

---

## 13. 动态工作流 (Dynamic Workflows)

把一个声明式 DAG 交给内核,让它为每个节点 spawn 一个全新上下文的子 agent——内核掌握控制流(门控 · 预算 · join 挂起 · 恢复),你的 SDK 跑 agent。概念总览见 [Dynamic Workflows](../concepts/dynamic-workflows.md);ABI 见 [Kernel ABI — Workflow ABI](../reference/kernel-abi.md#workflow-abi-dynamic-workflows)。

```ts
// 每条规则一个全新上下文的 verifier(不继承作者上下文 → 无法自我背书),
// 再加一个 skeptic 复核它们的 flag。内核把 3 个 verifier 作为一个受门控的批次 spawn,
// 在 join 处挂起,等它们完成后再跑 skeptic。
const outcome = await runner.runWorkflow({
  nodes: [
    { task: "规则:金额是整数分 —— 代码里有没有违反?", role: "verify" },
    { task: "规则:所有 error 都向上传播 —— 有没有违反?", role: "verify" },
    { task: "规则:时间戳都是 UTC —— 有没有违反?",       role: "verify" },
    { task: "Skeptic:上面的 flag 里哪些是真违规?",      role: "verify", dependsOn: [0, 1, 2] },
  ],
})
// => { completed: ["wf-node0", "wf-node1", "wf-node2", "wf-node3"], failed: [] }
```

### 13.1 节点 kind(六种模式)

节点的 `kind` 选择控制流形状;同一个执行器驱动全部,每次 spawn 都过 syscall gate:

| `kind` | 行为 |
|---|---|
| `{ type: "spawn" }`(默认) | 跑一次该节点的 agent |
| `{ type: "loop", maxIters }` | 反复运行直到 agent 报告完成,以 `maxIters` 兜底 |
| `{ type: "classify", branches }` | 分类器的结果选中一个分支,其余分支在运行前被剪掉 |
| `{ type: "tournament", entrants }` | 生成 N 个参赛者,再跑两两对阵的 judge bracket 到一个冠军 |
| `{ type: "reduce", reducer }` | **无 token 的宿主计算** —— 一个纯函数(`dedupe_lines` / `merge_json_arrays` / `concat` / `count`,或经 `reducers` 选项自定义)作用于依赖输出 |

模板构造器:`fanoutSynthesize(workers, synth)`、`generateAndFilter(gens, filter)`、`verifyRules(rules, skeptic)`。

### 13.2 运行时扩展 DAG

把 `submitWorkflowNodesTool` 交给某个节点,它的 agent 就能在运行中向活动 DAG 追加节点(真正的 loop-until-done;为发现的每条 claim 派一个 verifier)。提交的 `dependsOn` 是**批次相对、仅向后**的;每个追加的 spawn 仍过同一道配额 / 深度 / 隔离门;提交会被记录并在 `resumeWorkflow` 时回放。

### 13.3 信任、schema 与预算

- **隔离无逃逸** —— 给读取不可信内容的节点设 `trust: "quarantined"`;它申请可写隔离会在内核被拒,且它运行时提交的任何节点都被强制降级为 quarantined(污点传递,无权限升级)。
- **结构化输出** —— 给节点设 `outputSchema`;runner 指示 agent、对结果做校验,不符合则带错误回喂、重跑一次。始终不符合的节点会失败(其依赖被饿死)。
- **预算即信号** —— 装上 `maxWorkflowNodes` / `maxConcurrentSubagents` 配额后,每个 spawn 出来的节点都会在 goal 里带上剩余余量,协调者据此定扇出规模。

### 13.4 恢复

`resumeWorkflow(spec)` 从 session log 恢复:已完成节点被跳过,运行时追加的节点被回放,内核从断点继续。
