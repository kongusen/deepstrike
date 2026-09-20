# 统一公开 Agent 模型

状态：已确认方向，进入实现规划

## 目标

让用户可以用一个稳定、可预测的模型理解 SDK：

```ts
const agent = createAgent({
  name: "researcher",
  provider: openai({ model: "gpt-5" }),
  instructions: "查证事实并给出来源。",
  tools: [search],
})

const result = await agent.run("研究这个问题")
```

公开模型必须明确区分四个概念：

| 概念 | 含义 | 生命周期 |
|---|---|---|
| `Agent` | 可复用的身份、指令、模型和能力声明，同时提供执行入口 | 应用配置生命周期 |
| `Run` | Agent 针对一个目标的一次执行 | 一次调用 |
| `Session` | 多次 Run 之间的连续状态与恢复边界 | 持久化生命周期 |
| `Runtime` | 执行宿主，负责 provider、策略、存储、工具执行和内核协调 | 应用或请求生命周期 |

`Workflow` 是多个 Agent/Run 的编排模型，不应伪装成单 Agent API。

## 当前问题基线

当前 Node API 存在以下结构性问题：

1. `Agent` 只有声明字段，没有 `run`、`stream` 或 `resume` 行为。
2. `runAgent` 不接收 `Agent`，而是接收 `provider + goal`，实际是一次性任务 facade。
3. `RuntimeRunner` 同时承担 Runtime、Run、Session、Workflow 宿主和大量基础设施职责。
4. `tools`、`model`、`memory`、`sessionLog` 等配置在 `AgentOptions`、`RunAgentOptions`、`RuntimeOptions` 和 workflow spec 之间重复出现。
5. `runFanout`、`runLoop`、`AgentPool`、`ReactiveSession` 属于不同编排模型，却和单 Agent 入口处于同一认知层级。
6. `Agent -> AgentSpec -> Kernel` 的 lowering 路径存在，但只是转换层，没有成为用户的主执行路径。
7. 根入口同时暴露意图 API、运行时 API、内核证据 API 和演进 API，根入口的“简单入口”定位与实际导出规模不一致。

## 目标公开层次

### Tier 1：Agent 意图 API

这是普通应用首先看到的 API。它只需要表达“定义一个 Agent 并运行它”。

```ts
export interface AgentDefinition {
  name?: string
  description?: string
  instructions?: string
  provider: LLMProvider
  tools?: RegisteredTool[]
  skills?: Skill[]
  memory?: AgentMemory
  knowledge?: Knowledge[]
  outputSchema?: JsonSchema
  guardrails?: Guardrail[]
  handoffs?: Handoff[]
  metadata?: Record<string, unknown>
}

export interface AgentRunOptions {
  session?: SessionRef
  maxTurns?: number
  signal?: AbortSignal
  metadata?: Record<string, unknown>
  onPermissionRequest?: PermissionHandler
}

export interface Agent {
  readonly name: string
  run(goal: string, options?: AgentRunOptions): Promise<RunResult>
  stream(goal: string, options?: AgentRunOptions): AsyncIterable<StreamEvent>
  resume(session: SessionRef, options?: AgentRunOptions): AsyncIterable<StreamEvent>
  session(id?: string, options?: SessionOptions): AgentSession
  remember(input: MemoryInput): Promise<MemoryRecord>
  recall(query: string, options?: RecallOptions): Promise<MemoryRecord[]>
  delegate(request: DelegationRequest): Promise<DelegationResult>
  workflow(spec: WorkflowSpec): Promise<WorkflowResult>
  listen(options?: { session?: SessionRef; leaseMs?: number }): Promise<RunResult | null>
}

export function createAgent(definition: AgentDefinition): Agent
```

`Agent` 是可执行对象。它的配置可以复用，具体一次执行通过 `run` 或 `stream` 创建。高级功能通过同一个 Agent 对象上的场景化方法进入，普通用户不需要理解 Kernel、ExecutionPlane 或 RuntimeRunner。

### Tier 2：Run 与 Session API

一次执行不再只返回裸字符串，而是返回结构化结果；保留便捷文本 helper。

```ts
export interface RunResult<T = string> {
  output: T
  runId: string
  sessionId?: string
  status: "completed" | "partial" | "failed" | "cancelled"
  usage?: TokenUsage
  events?: SessionEvent[]
}

export interface SessionRef {
  id: string
}

export function collectText(result: AsyncIterable<StreamEvent>): Promise<string>
```

`sessionId` 是恢复和连续性的边界，不属于 Agent 身份本身。`runId` 标识一次实际执行。

### Tier 3：Runtime 宿主 API

Runtime 是高级宿主配置，不再被普通用户误认为 Agent。

```ts
export interface RuntimeOptions {
  provider: LLMProvider
  executionPlane?: ExecutionPlane
  sessionStore?: SessionStore
  memoryStore?: MemoryStore
  governancePolicy?: GovernancePolicy
  signalSource?: SignalSource
  resourceQuota?: ResourceQuota
  payloadStore?: PayloadStore
}

`RuntimeOptions` 只作为 `createAgent` 的高级宿主配置或测试注入使用。它不作为普通用户的第一入口，也不要求用户直接实例化 Runtime。
```

现有 `RuntimeRunner` 只作为 facade 内部实现；它不属于普通用户公开模型。

### Tier 4：Workflow 编排 API

工作流显式接收 Agent 或 Agent 定义，避免使用字符串任务模拟 Agent。

```ts
export interface WorkflowNode {
  agent: Agent | AgentDefinition
  goal: string
  dependsOn?: string[]
  role?: KernelAgentRole
}

export interface Workflow {
  run(options?: WorkflowRunOptions): Promise<WorkflowResult>
}

export interface DelegationRequest {
  goal: string
  role?: KernelAgentRole
}

export interface AgentSession {
  readonly id: string
  run(goal: string, options?: AgentRunOptions): Promise<RunResult>
  stream(goal: string, options?: AgentRunOptions): AsyncIterable<StreamEvent>
  resume(options?: AgentRunOptions): AsyncIterable<StreamEvent>
  interrupt(reason?: string): void
}
```

原有 `runFanout` 不再作为公开入口；并行任务统一通过 `agent.workflow(...)` 表达。

## 配置归属规则

| 配置 | 归属 | 说明 |
|---|---|---|
| instructions、model、tools、skills、knowledge、guardrails | Agent | 定义 Agent 的身份和能力 |
| goal、session、signal、abort、run metadata | Run | 只影响一次执行 |
| provider、execution plane、stores、governance、quota | Runtime | 提供执行环境和策略 |
| nodes、dependencies、reducers、join policy | Workflow | 编排多个执行单元 |

同一配置不能同时作为 Agent 和 Runtime 的普通入口。兼容层可以接受旧字段，但规范 API 只保留一个归属。

## 兼容迁移策略

采用一次性替换，不保留旧的用户入口：

1. 新增并推广 `createAgent`、`Agent.run`、`Agent.stream`、`Agent.session` 和 `RunResult`。
2. 删除 `runAgent`、`runFanout`、`RuntimeRunner` 作为根入口的公开导出；底层实现可以保留在内部模块。
3. `Agent` 现有字段迁移到新的 `AgentDefinition`，`lowerAgent` 只作为内部 provider-neutral lowering，不再作为普通用户 API。
4. `runFanout` 迁移为 `agent.workflow(...)` 或 `agent.delegate(...)` 的实现，不再要求用户构造字符串任务 DAG。
5. 根入口只导出 Agent、Run、Session、工具、Provider 工厂和场景化高级能力；journal、kernel、evolution、harness 等保留在内部或明确的开发者 subpath。

## 高级能力的场景化封装

高级能力必须围绕用户任务表达，而不是暴露底层机制：

| 用户需求 | 公开封装 | 隐藏的实现 |
|---|---|---|
| 持续记住和回忆信息 | `agent.remember()`、`agent.recall()`、`agent.session()` | MemoryStore、memory syscall、page-out |
| 工具权限和人工审批 | `onPermissionRequest`、工具的 `risk`/`requiresApproval` | GovernancePolicy、PermissionManager、approval effect |
| 流式输出和中断 | `agent.stream()`、`session.interrupt()` | StreamEvent、cancel syscall、abort controller |
| 委托一个专门 Agent | `agent.delegate()` | sub-agent process、handoff、quota、join |
| 并行研究后综合 | `agent.workflow()` | WorkflowSpec、DAG、reducers、scheduler |
| 外部事件唤醒 | `agent.listen()` 或 `session.resume()` | SignalSource、signal router、durable wake |
| 结果可验证 | `agent.run({ verification })` | Harness、judge、milestone、evidence |

这些方法返回用户任务层面的结果。除 `stream()` 外，用户不需要处理 Kernel action、observation 或 SessionLog event。

## 不在本阶段解决的问题

- 不改变 Kernel ABI、SessionLog 事件格式或 provider wire 协议。
- 不把所有高级能力塞进 `Agent`，例如 journal、quota 和 scheduler 仍属于 Runtime。
- 不同时重命名全部历史 API。
- 不承诺跨语言 API 在一个版本内完全同名；先统一概念和归属，再做 Node/Python/Rust 映射。

## 实施顺序

### Phase 1：新契约与内部适配

- 定义 `AgentDefinition`、`AgentRunOptions`、`RunResult`、`AgentSession`、`DelegationRequest` 类型。
- 新建 Agent facade，将现有 RuntimeRunner 的执行能力收进 facade 内部。
- 让 Agent 保持可序列化定义，执行状态放在 AgentSession/Run 中。

### Phase 2：文档和示例

- 把 Node quick start 改成 `createAgent(...).run(...)`。
- 新增 Agent、Run、Session、Runtime 的概念文档。
- 将 workflow、harness、OS 和 kernel 能力从普通入门路径移到高级章节。

### Phase 3：导出替换

- 收缩根入口，建立 `@deepstrike/sdk/advanced` 供开发者扩展和诊断。
- 删除 `runAgent`、`runFanout`、裸 `RuntimeRunner` 的公开导出。
- 增加 API surface 测试，保证 Tier 1 不依赖内部 kernel 类型。

## 验收标准

- 用户可以只通过 `defineAgent`、`agent.run` 和 `agent.stream` 完成单 Agent 的定义、执行和流式调用。
- `Agent`、`Run`、`Session`、`Runtime`、`Workflow` 在文档和类型中各自只有一种含义。
- Agent 定义不需要直接了解 `RuntimeRunner`、`ExecutionPlane` 或 Kernel。
- 高级能力仍可访问，但不出现在普通 Agent 入门路径中。
- 根入口只保留统一 Agent 模型和普通用户需要的场景化封装。
- 高级能力可以通过 Agent 方法完成，且不要求用户理解 RuntimeRunner 或 Kernel。
- Node 根入口的导出可以按“意图 API”和“高级 API”解释，而不再依赖内部模块知识。

## 待评审问题

1. `Agent` 是否采用工厂函数 `createAgent`，还是保留 `new Agent` 作为主要写法？本提案采用工厂函数，class 作为内部实现细节。
2. `AgentDefinition.provider` 是否直接接收 `LLMProvider`，还是接收更高层的 `ModelRef` 并由 SDK 创建 Provider？本阶段直接接收 Provider，避免把凭据和路由隐式藏在 Agent 内。
3. `RunResult.output` 是否默认为 string，还是根据 `outputSchema` 返回泛型结构？本提案采用 `RunResult<T = string>`，由输出 schema 决定 T。
