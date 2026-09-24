# DeepStrike Node.js SDK：数据流与 API 完整分析

> 分析基线：main 分支，SDK 版本 0.2.73。
> 分析范围：node/ 目录的公开 API、运行时数据流、Canonical Kernel、SessionLog、Provider、工具执行平面、Memory、Skill、Workflow 以及当前实现与文档之间的差异。
> 代码位置：本文件位于 node/，下文的源码链接均相对于 node/ 目录。

## 1. 结论先行

DeepStrike Node.js SDK 可以看成三层系统：

1. **Node.js API 层**：负责 createAgent()、Session、Provider、工具和各个子路径的开发者体验。
2. **Runtime 层**：负责请求编排、上下文、工具执行、Provider 调用、Memory、Workflow、事件流和恢复。
3. **Canonical Kernel 层**：负责可恢复的动作调度、策略检查、预算、生命周期和规范化状态转移。

从源码边界看，这三层还可以细化为五个协作层：Intent、Runtime、Kernel ABI、Provider 和 Effect。Intent 层对应根入口与声明，Runtime 层对应 RuntimeRunner 和 CanonicalRunnerRuntime，Kernel ABI 层对应 NAPI 暴露的 CanonicalKernel，Provider 层负责厂商协议，Effect 层承载 ExecutionPlane、MemoryStore 和各类宿主平面。五层是职责划分，三层是阅读运行时的简化视图。

一次普通的 Agent 运行大致经过以下路径：

    应用代码
      │
      ▼
    createAgent() / Agent.run() / Agent.stream()
      │
      ▼
    RuntimeRunner
      │
      ▼
    CanonicalRunnerRuntime + CanonicalKernelHost
      │
      ▼
    @deepstrike/core Canonical Kernel
      │
      ├─ call_provider       ──► Provider adapter ──► 外部模型服务
      ├─ execute_tool        ──► ExecutionPlane ──► 本地工具 / MCP / 沙箱 / VPC
      ├─ request_approval     ──► 审批与权限治理
      ├─ persist_memory       ──► MemoryStore
      ├─ query_memory         ──► MemoryStore
      ├─ spawn_workflow       ──► Workflow driver / 子 Agent
      ├─ archive_page_out     ──► ArchiveStore
      ├─ load_payload         ──► PayloadStore
      └─ done                 ──► 终态
      │
      ▼
    SessionLog 投影 + StreamEvent 事件流 + RunResult

核心分工如下：

| 组件 | 主要职责 | 持久化语义 |
| --- | --- | --- |
| Canonical Kernel | 产生下一步动作、执行策略、管理生命周期与预算 | 通过 KernelJournal 保存规范化状态链 |
| RuntimeRunner | 执行 Kernel 动作并接入 Node.js I/O | 将业务事件投影到 SessionLog |
| Provider | 将统一请求转换为厂商协议并解析响应 | 通过 provider attempt 记录请求证据 |
| ExecutionPlane | 执行工具、MCP、进程、工作树或远程任务 | 通过工具请求与结果事件记录 |
| MemoryStore | 保存、检索、更新 Memory | 由调用方提供内存或持久化实现 |
| SessionLog | 提供业务侧事件、恢复入口和审计读取 | InMemory 或 JSONL |

SDK 的关键设计是将 **Kernel 决定做什么** 与 **Runtime 执行 I/O** 分开。这样可以在不同 Provider、工具平面和存储实现之间复用相同的运行时控制逻辑，也可以在崩溃后从规范化日志恢复未完成动作。

## 2. 包结构与公开入口

### 2.1 包信息

node/package.json 当前版本为 0.2.73，要求 Node.js >=18，核心依赖为 @deepstrike/core@0.2.73。原生核心通过平台可选依赖提供；开发环境中如果存在本地 Rust 构建产物，kernel.ts 会优先尝试加载本地 ../../crates/deepstrike-node/index.js，失败后回退到 @deepstrike/core。

根包和子路径由 node/package.json 的 exports 定义：

| 导入路径 | 作用 |
| --- | --- |
| @deepstrike/sdk | Agent、工具、Provider、Workflow、Eval 与根级便捷 API |
| @deepstrike/sdk/providers | Provider、模型、端点、凭证、请求计划与适配器 |
| @deepstrike/sdk/workflow | Workflow 定义、编排、持久化与动态工作流能力 |
| @deepstrike/sdk/planes | Worktree、沙箱、MCP、远程 VPC 等执行平面 |
| @deepstrike/sdk/memory | MemoryStore、WorkingMemory、检索、保留策略与抽取 |
| @deepstrike/sdk/harness | Attempt loop、judge、nudge、carry 与停止策略 |
| @deepstrike/sdk/os | 原生 OS profile、信号、权限、重放与快照能力 |
| @deepstrike/sdk/advanced | 根 API 与 Runtime、事件、Journal、Session 等高级能力的组合入口 |
| @deepstrike/sdk/runtime | RuntimeRunner、SessionLog、KernelJournal、上下文与运行时 facade |
| @deepstrike/sdk/evals | Eval 数据集、评估器、judge、trace 与结果类型 |

根级导出见 [src/index.ts](src/index.ts)。各子路径使用 public.ts 作为稳定 barrel，例如 [src/providers/public.ts](src/providers/public.ts) 和 [src/runtime/public.ts](src/runtime/public.ts)。

### 2.2 根级 API

根级 API 的主要组成：

    createAgent(definition: AgentDefinition): Agent
    tool(definition: ToolDefinition): ToolDefinition
    streamingTool(definition: StreamingToolDefinition): ToolDefinition
    safeTool / ok / fail / ToolError / formatToolError
    createTextKnowledgeSource(text: string, options?): KnowledgeSource
    createProvider(options): LLMProvider
    createProviderAsync(options): Promise<LLMProvider>
    createWorkflow(definition): WorkflowDefinition
    evaluate(agent, options): Promise<EvalRun>

根级类型包括 Agent、AgentSession、AgentDefinition、RunOptions、RunResult、StreamEvent、ToolDefinition、MemoryRecord、MemoryStore、KnowledgeSource、WorkflowSpec 和 Provider 相关类型。

runAgent、runFanout、RuntimeRunner、LocalExecutionPlane 等能力可以从相应子路径访问。API surface 测试明确验证了它们不属于根级导出，详见 [tests/api-surface.test.ts](tests/api-surface.test.ts)。

## 3. Agent 构建过程

### 3.1 createAgent() 的入口流程

createAgent() 位于 [src/agent-facade.ts](src/agent-facade.ts)，主要过程如下：

    AgentDefinition
      │
      ▼
    captureAgentDeclaration()
      ├─ 校验 memoryStore 与 memoryScope 的成对关系
      ├─ 复制并冻结可序列化声明
      ├─ 提取可执行工具 handler 到 host bindings
      ├─ 保存知识 retriever 映射
      └─ 生成稳定的声明快照
      │
      ▼
    AgentRuntimeImpl
      ├─ 保存 declaration
      ├─ 创建默认 InMemorySessionLog
      ├─ 延迟创建 RuntimeRunner
      └─ 绑定 run / stream / session / memory / workflow API

声明捕获阶段不会把函数直接塞进 Kernel 的可序列化状态。工具的 JSON Schema、名称和描述进入运行时声明，真正的 handler 保留在 Node.js host binding 中。这样 Kernel Journal 可以只处理 JSON 数据，工具执行仍由宿主进程完成。

memoryStore 和 memoryScope 必须一起配置。只设置其中一个会在声明捕获阶段失败。自定义命名空间会经过校验，并参与 Memory 查询与写入隔离。

### 3.2 createRunner() 的绑定规则

Agent 首次运行时，createRunner() 根据 runtimeBinding 创建运行时：

1. 如果传入 runtimeBinding.provider，直接使用该 Provider。
2. 如果 model 是字符串且没有直接 Provider，则调用 providerFor(model)。
3. 如果提供 runtimeBinding.executionPlane，使用自定义执行平面。
4. 没有自定义执行平面且声明了 mcpServers 时，创建 McpProxyExecutionPlane。
5. 其余情况创建 LocalExecutionPlane，并绑定声明中的工具。

以下情况会在 facade 层被拒绝：

- 自定义 executionPlane 与 mcpServers 同时设置。
- 本地 Agent facade 使用 MCP http 或 sse transport。
- MCP server 需要认证，但没有显式的 credential vault。

MCP stdio 连接会在工具 schema 快照前完成初始化和 tools/list，因此 Kernel 看到的 baseline tools 已经包含 MCP 工具。

### 3.3 运行时选项映射

[src/runtime/agent-runtime-options.ts](src/runtime/agent-runtime-options.ts) 将公开声明映射成 RuntimeRunner 选项，主要包含：

- systemPrompt：由 instructions、outputSchema 和治理规则合并生成。
- baselineToolIds：执行平面的全部工具 schema。
- maxTokens：默认值为 32,000。
- guardrails：合并声明级 governance 与运行时级 governance。
- knowledge：文本知识转成 createTextKnowledgeSource()。
- workflowAgentResolver：将工作流节点的 Agent 名称解析为运行时 Agent。
- memoryStore、memoryScope：注入 Memory 能力。
- sessionLog、kernelJournal：注入业务投影和规范化 Journal。

## 4. Agent 公共对象模型

### 4.1 Agent

    interface Agent {
      readonly name: string;
      readonly declaration: AgentDeclaration;

      run(goal: string, options?: RunOptions): Promise<RunResult>;
      stream(goal: string, options?: RunOptions): AsyncIterable<StreamEvent>;
      session(id?: string): AgentSession;

      remember(input: RememberInput): Promise<MemoryRecord>;
      recall(query: string, options?: RecallOptions): Promise<MemoryRecall[]>;

      delegate(request: DelegationRequest): Promise<DelegationResult>;
      workflow(spec: WorkflowSpec, options?: WorkflowOptions): Promise<WorkflowOutcome>;
      listen(options?: ListenOptions): Promise<RunResult | null>;

      close(): Promise<void>;
    }

### 4.2 AgentSession

    interface AgentSession {
      readonly id: string;

      run(goal: string, options?: RunOptions): Promise<RunResult>;
      stream(goal: string, options?: RunOptions): AsyncIterable<StreamEvent>;
      resume(options?: RunOptions): AsyncIterable<StreamEvent>;
      interrupt(reason?: "user" | "deadline" | "lease_lost" | "host_shutdown"): void;
    }

Session 是同一条对话与 Journal 链的入口。Agent 默认使用 InMemorySessionLog，进程退出后内容不会保留。需要跨进程恢复时，应注入 FileSessionLog 或自定义 SessionLog，并确保它提供匹配的 KernelJournal。

### 4.3 RunResult 与 StreamEvent

Agent.run() 内部消费 stream() 的事件，累加 text_delta，读取最近一次 run_started、context_prepared、prompt_measured 和 provider_attempt，最后生成 RunResult。

常用 RunResult 字段：

| 字段 | 含义 |
| --- | --- |
| status | completed、cancelled、failed 或 partial |
| text / output | 文本或经过 output schema 验证的结构化结果 |
| usage | 当前实现取最后一次 Provider usage |
| evidence | Provider attempt、工具、上下文和终止原因等证据 |
| sessionId | 所属 Session |
| outputValidation | 声明 outputSchema 时的校验结果 |

流事件包括 run_started、context_prepared、prompt_measured、provider_attempt、text_delta、tool_requested、tool_delta、tool_result、tool_completed、memory_retrieved、workflow_node_completed、run_terminal 和 done 等类型。

outputSchema 校验发生在 run() 汇总阶段。直接消费 stream() 时，需要调用方根据事件自行处理最终结构化输出。

## 5. 一次完整运行的数据流

### 5.1 运行启动与 Session 恢复

入口是 [RuntimeRunner.run()](src/runtime/runner.ts)。启动阶段的逻辑可以概括为：

    RunRequest(goal, sessionId, options)
      │
      ▼
    读取 SessionLog 与 KernelJournal
      │
      ├─ 新运行：创建 run_started
      └─ 恢复运行：读取 journal tail、未完成 action、outbound envelope
      │
      ▼
    确定 session/run/group/op 标识
      │
      ▼
    进入 execute()

Runner 会根据 Journal 判断当前请求是新运行还是中断后的继续运行。恢复时不会重复播种已经写入 Journal 的附件，也不会把已确认的工具结果再次写入业务投影。

### 5.2 execute() 的初始化顺序

execute() 会完成以下工作：

1. 重置本次运行的 transient state。
2. 创建或恢复 CanonicalKernel、Journal 和 operation id。
3. 配置 tokenizer、plan tool、工具集合和 system prompt。
4. 注入 initialMemory、skills、stable tools、knowledge 和 milestone contract。
5. 重放已有事件，构建 AgentRunSpec。
6. 创建 RunGroup 并应用预算、治理、审批和停止策略。
7. 预取 Memory。
8. 播种附件、附件引用和外部信号。
9. 启动 Kernel Agent。

初始化完成后，Runtime 进入动作循环。Kernel 每次返回一个 action，Runtime 执行动作并把 observation 交回 Kernel。

### 5.3 主循环

主循环位于 [src/runtime/runner.ts](src/runtime/runner.ts)，每一轮通常按以下顺序执行：

    写入 pending observations
      │
      ▼
    过期 skill context
      │
      ▼
    处理 interrupt / cancellation
      │
      ▼
    读取 inbound signal 与注入 note
      │
      ▼
    读取 Kernel action
      │
      ├─ call_provider
      ├─ execute_tool
      ├─ request_approval
      ├─ persist_memory
      ├─ query_memory
      ├─ archive_page_out
      ├─ load_payload
      ├─ evaluate_milestone
      ├─ spawn_workflow
      ├─ unsupported_effect
      └─ done

每个 action 都有对应的事件、Journal transition 和下一轮 observation。Runtime 不直接修改 Kernel 的内部状态，所有状态变化都通过规范化 transition 完成。

### 5.4 Provider 回合

当 Kernel 产生 call_provider 时，数据流如下：

    call_provider(action)
      │
      ▼
    上下文治理与工具过滤
      │
      ▼
    prepareProviderRequest()
      ├─ RenderedContext
      ├─ Provider 工具 schema
      ├─ Runtime options
      └─ request plan fingerprint
      │
      ▼
    prompt_measurement
      ├─ Provider 原生 countTokens
      └─ tokenizer / heuristic fallback
      │
      ▼
    Canonical context prepare
      │
      ▼
    Provider.prepareRequest()
      │
      ▼
    provider.stream()
      │
      ├─ text_delta ─────────► StreamEvent
      ├─ usage ──────────────► usage / evidence
      ├─ tool_call ──────────► tool action observation
      ├─ reasoning / cache ──► provider evidence
      └─ stop / error ───────► terminal or retry decision
      │
      ▼
    provider_attempt + provider_result
      │
      ▼
    llm_completed observation

Provider 的中间 transport error 会先经过 classifyProviderError()。可恢复错误会记录 provider_attempt 并交回 Kernel，Kernel 可能执行 compact、retry 或终止。已经被恢复的中间错误不会直接暴露为最终 RunResult 错误。

### 5.5 工具回合

当 Kernel 产生 execute_tool 时：

    execute_tool(action)
      │
      ▼
    tool_requested
      │
      ▼
    权限、治理、审批与 onToolCall hook
      │
      ▼
    ExecutionPlane.executeAll()
      │
      ├─ 参数 cast / default / JSON Schema 校验
      ├─ 工具 handler 或远程 tools/call
      ├─ tool_delta / tool_suspend
      └─ tool_result
      │
      ▼
    onToolResult hook
      ├─ 替换输出
      └─ 注入 note / signal
      │
      ▼
    tool_completed
      │
      ▼
    tool_results observation

一次 execute_tool 必须为每个请求返回恰好一个结果。普通 Local tool 可以并发执行；计划工具、Workflow 工具和需要宿主控制的工具会被特殊拦截。

### 5.6 终态与收尾

Kernel 返回 done 后，Runner 会：

1. 写入 run_terminal。
2. 结算 RunGroup budget 和使用量。
3. 保存或抽取本次 Session Memory。
4. 关闭未完成的工具或 Provider stream。
5. 发出 done。
6. 由 Agent facade 汇总为 RunResult。

最终状态是 Runtime 的粗粒度映射。RunResult 不提供独立的 termination 字段；context_overflow、no_progress、token_budget、max_turns、milestone_pending 和 invalid_arg 等细节需要从 SessionLog 的 run_terminal、operation_cancelled 及 evidence 中读取。

## 6. Canonical Kernel 与持久化

### 6.1 Canonical transition

[CanonicalKernelHost.transition()](src/runtime/canonical-kernel-step.ts) 是可靠状态推进的核心：

    1. 将输入 observation 编码成 JSON envelope
    2. stageOutboundEnvelope()
    3. kernel.prepare()
    4. journal.compareAndAppend(core bytes / digest)
    5. kernel.commit()
    6. 投影 planned step、action 与 observations
    7. 清理 outbound envelope

如果 Journal append 成功但 kernel.commit() 失败，Host 会从 Journal 恢复并抛出 rebuild-required。若遇到 compare-and-append 的 CAS 冲突，则恢复并重试。这样可以处理进程崩溃、重复提交以及多个恢复者竞争同一条状态链的情况。

### 6.2 wake() 与崩溃恢复

wake() 会：

1. 从 checkpoint 和 Journal tail 恢复 Kernel。
2. 排空尚未提交的 outbound envelope。
3. 继续未完成 action。
4. 只把确认过的 observation 投影到 SessionLog。

Kernel Journal 是规范化链，SessionLog 是业务投影，两者的序号空间不同：

| 日志 | 序号 | 用途 |
| --- | --- | --- |
| SessionLog.seq | 业务事件序号 | UI、审计、运行结果与用户可见历史 |
| KernelJournal.step_seq | Kernel step 序号 | 状态恢复、CAS、动作去重与重放 |

默认实现：InMemorySessionLog 搭配 InMemoryKernelJournal；FileSessionLog 使用 JSONL，并搭配 FileKernelJournal。

### 6.3 为什么需要两套日志

业务事件包含文本增量、工具结果、Provider usage、Memory retrieval 等开发者关心的信息。Canonical Journal 只保存可重放的规范化 Kernel bytes 和 digest。将两者分开可以减少 Kernel 状态与厂商响应细节的耦合，并允许不同的业务投影格式。

## 7. 上下文与 Token 数据流

### 7.1 RenderedContext 槽位

Runtime 将上下文整理成 [RenderedContext](src/types.ts)：

| 槽位 | 内容 | 生命周期 |
| --- | --- | --- |
| systemStable | Agent 身份、规则、稳定工具说明 | 长期稳定 |
| systemKnowledge | Skills、initial memory、固定知识 | 可按预算裁剪 |
| stateTurn | 当前任务状态、信号、临时治理信息 | 当前回合 |
| turns | 历史用户、模型、工具与观察 | 可压缩、可分页 |
| frozenPrefixLen | 已冻结的前缀边界 | Provider cache 边界 |
| budgetOverflow | 超预算时被裁剪或压缩的信息 | 当前准备阶段 |

### 7.2 历史、知识与检索结果

- initialMemory 和 pushKnowledge() 进入 knowledge 槽位。
- memory(query) 和 knowledge(query) 的检索结果作为历史 observation 进入 turns。
- 历史可以压缩，knowledge 有单独的预算和 eviction 逻辑。
- Skill 文件读取结果以 skill:<name> 为 key pin 到 knowledge 中。

### 7.3 Provider 适配差异

Anthropic 使用独立的 system cache blocks。OpenAI family 通常将 stateTurn 追加到稳定前缀后。不同厂商的 wire message 结构由 adapter 处理，Kernel 只看到规范化的 RenderedContext 和动作。

### 7.4 Prompt measurement

Runner 会优先调用 Provider 的原生 token 计数能力；没有原生能力时使用 tokenizer 或 heuristic fallback。测量事件会记录输入 footprint、cache read、cache write 和预算裁剪信息。

Provider attempt 的 usage 可能包括：

- input tokens
- output tokens
- cache read tokens
- cache write tokens
- reasoning tokens（如果厂商提供）
- 总 token 与费用估算字段

当前 RunResult.usage 取最后一次 Provider usage；跨多个回合的累计使用量应从 provider_attempt 事件或 DoneEvent.totalTokens 读取。

## 8. Provider 数据流与 API

### 8.1 Provider 创建

Provider 构建入口见 [src/providers/catalog.ts](src/providers/catalog.ts)、[src/providers/registry.ts](src/providers/registry.ts) 和 [src/providers/endpoints.ts](src/providers/endpoints.ts)：

    model string / ProviderOptions
      │
      ▼
    模型前缀与 catalog 匹配
      │
      ▼
    provider / endpoint / protocol 解析
      │
      ▼
    credential sync / async
      │
      ▼
    PROVIDER_REGISTRY maker
      │
      ▼
    adapter + capabilities + runtimePolicy + requestPlanIdentity

模型注册表会根据模型族选择协议。例如 GPT-5、GPT-4.1 和 o 系列默认走 openai.responses；GPT-4o 等默认走 Chat Completions。具体选择可以被显式 endpoint 或 provider 选项覆盖。

### 8.2 LLMProvider 接口

Provider 的统一接口位于 [src/types.ts](src/types.ts)，关键方法包括：

    interface LLMProvider {
      prepareRequest(request: ProviderRequest): Promise<PreparedProviderRequest>;
      createRunState?(request: ProviderRequest): ProviderRunState;
      descriptor(): ProviderDescriptor;
      runtimePolicy?(): ProviderRuntimePolicy;
      replay?(state: ProviderRunState, request: ProviderRequest): Promise<void>;
      countTokens?(request: ProviderRequest): Promise<TokenCount>;
      complete?(request: ProviderRequest): Promise<ProviderResponse>;
      stream?(request: ProviderRequest): AsyncIterable<ProviderChunk>;
      signal?(request: ProviderSignal): Promise<void>;
    }

Runner 先构建 ProviderRequestPlan，其中包含清洗后的 endpoint、上下文、工具 schema、运行时选项和 fingerprint。prepareRequest() 返回冻结的 request、run state、stream 入口以及可选的 countTokens 方法。

### 8.3 适配层

统一输入首先通过 normalizeCanonicalAdapterInput()：

- 检查文本、图片等 modality。
- 检查内容的 source affinity。
- 将工具结果投影为 Provider 能理解的形式。
- 注入 Provider replay 信息。
- 根据能力拒绝不支持的组合。

之后由 adapter 序列化成厂商 wire request，并把流式响应解码为统一的 StreamEvent。适配器负责 stop reason、usage、cache、reasoning 和错误字段的归一化。

当前主要 Provider：

| Provider | 协议与特征 |
| --- | --- |
| AnthropicProvider | Anthropic Messages、thinking、replay、cache、官方 countTokens |
| OpenAIChatProvider | Chat Completions 与 OpenAI-compatible dialect |
| OpenAIResponsesProvider | Responses API、previous response continuation、原生计数 |
| GeminiProvider | Gemini 请求与响应格式 |
| OllamaProvider | 本地 Ollama endpoint |
| AnthropicCompatibleProvider | 兼容 Anthropic Messages 的第三方服务 |

工厂包含 DeepSeek、Kimi、Qwen、GLM、MiniMax、Gemini、Ollama 等常用 provider。

### 8.4 Provider 错误处理

    adapter / transport error
      │
      ▼
    classifyProviderError()
      │
      ├─ 可重试：provider_attempt(transport_exhausted) → Kernel retry
      ├─ 可压缩：context compaction → Kernel retry
      ├─ 认证 / 参数：直接失败
      └─ 不可恢复：provider_error → terminal

Runner 记录每次 attempt 的 endpoint、request fingerprint、重试类别、usage 和错误证据。这样可以区分“服务暂时失败后恢复”和“最终运行失败”。

## 9. 工具、ExecutionPlane 与 MCP

### 9.1 工具定义

tool() 与 streamingTool() 位于 [src/tools/index.ts](src/tools/index.ts)。工具定义必须使用 JSON Schema object 作为根类型。SDK 会保存 schema、description、handler、是否 streaming 等元数据。

### 9.2 参数处理

工具参数进入 handler 前会经过以下处理：

1. 根据 schema cast boolean、number 和 integer。
2. 应用 default。
3. 将可接受的数组输入归一化。
4. 校验 additionalProperties。
5. 按 oneOf / anyOf 选择分支。
6. 生成规范化参数或 invalid_arg observation。

工具 handler 不应假设模型返回的 JSON 已经满足 schema。参数归一化是 Runtime 的职责，最终权限与治理仍由 Kernel 和 ExecutionPlane 共同决定。

### 9.3 LocalExecutionPlane

Local plane 会特殊处理 skill、memory 和 knowledge 工具，普通本地工具则可以并发执行。每个工具调用都必须返回一个结果，并支持工具事件流、挂起和恢复。

计划工具、提交 workflow node 和启动 workflow 的工具不会直接调用普通 handler，而是被 Runtime 识别并转成相应 Kernel action。

### 9.4 各类执行平面

| ExecutionPlane | 用途 |
| --- | --- |
| LocalExecutionPlane | 当前 Node.js 进程内的工具 handler |
| FilteredExecutionPlane | 按工具 ID、策略或 skill 过滤暴露集合 |
| ProcessSandboxExecutionPlane | 隔离进程、超时、资源和环境变量 |
| WorktreeExecutionPlane | 在 Git worktree 中执行代码任务 |
| McpProxyExecutionPlane | 连接 MCP stdio server 并代理 tools/list、tools/call |
| RemoteVpcExecutionPlane | 通过远程 VPC 执行受控任务 |

### 9.5 MCP 流程

    MCP server declaration
      │
      ▼
    spawn stdio process
      │
      ▼
    initialize
      │
      ▼
    tools/list
      │
      ▼
    schema snapshot → Kernel baselineToolIds
      │
      ▼
    execute_tool
      │
      ▼
    tools/call
      │
      ▼
    structured contentParts → tool result

本地 Agent facade 当前只接受 stdio transport。公共 MCP 类型虽然描述了更多 transport 形式，但 http、sse 和需要认证的 server 需要通过更底层的 plane 与 vault API 接入。

## 10. Memory、Knowledge 与 Skill

### 10.1 Memory API

Agent 级 API：

    agent.remember(input): Promise<MemoryRecord>
    agent.recall(query, options?): Promise<MemoryRecall[]>

remember() 创建带 host provenance 的 MemoryRecord，通过 Runner 写入 MemoryStore。recall() 调用 Runner 查询 Memory，并保留 scope、namespace、score、source 和 pinned 等信息。

MemoryStore 主要能力：

    put(record): Promise<MemoryRecord>
    get(id): Promise<MemoryRecord | null>
    delete(id): Promise<void>
    search(query, options?): Promise<MemoryRecall[]>
    saveSession(session): Promise<void>
    recordRecall(recall): Promise<void>
    setPinned(id, pinned): Promise<void>

SDK 提供 InMemoryMemoryStore、DurableMemory、WorkingMemory、retention、ranking 和 extraction 相关实现。

### 10.2 运行时 Memory 流程

    run start
      │
      ▼
    prefetch(query = goal)
      │
      ▼
    命中结果进入上下文 history
      │
      ▼
    Kernel query_memory
      │
      ▼
    MemoryStore.search()
      │
      ▼
    memory_retrieved + observation
      │
      ▼
    Kernel persist_memory
      │
      ▼
    配额、schema、scope、namespace 校验
      │
      ▼
    MemoryStore.put()

运行完成后，Runtime 可以保存 Session 并执行 Memory extraction。语义 page-out 会先调用 summarizer，再经过写入门控，最终才落入 MemoryStore。

### 10.3 KnowledgeSource

KnowledgeSource 的 retrieve() 结果进入历史上下文；pushKnowledge() 结果进入持久的 knowledge 槽位。文本知识可以用 createTextKnowledgeSource() 快速创建，复杂场景可以实现带查询、权限和来源元数据的自定义 retriever。

### 10.4 Skills

Skill 在运行开始时加载 metadata。激活 Skill 后：

1. 通过 skill 工具读取 SKILL.md。
2. 读取结果 pin 到 knowledge，key 为 skill:<name>。
3. 由 allowed_tools 缩小当前暴露的工具集合。
4. 使用 skillLeaseTurns 控制激活时长。
5. Skill filter、stable core tool ids 和 deactivate 逻辑共同维护工具边界。

Skill 的文件内容属于运行时知识，Skill metadata 和 allowed tools 属于治理输入，两者的生命周期不同。

## 11. Workflow 与 Sub-agent 数据流

### 11.1 两种编排方式

SDK 中有两种容易混淆的委托模式：

| API | 机制 | 适合场景 |
| --- | --- | --- |
| agent.delegate() | Host 层直接调用目标 Agent | 简单的一次性委托 |
| agent.workflow() | Kernel 管理 DAG、依赖、预算和恢复 | 多节点、并发、循环与可审计编排 |

runFanout 和 runAgent 位于 runtime facade，可从子路径访问；它们不属于根级导出。

### 11.2 Workflow 启动

agent.workflow() 调用 runWorkflow()：

    WorkflowSpec
      │
      ├─ standalone：自动创建 Kernel、追加 run_started、应用策略
      └─ active parent：复用当前 Kernel 与 RunGroup
      │
      ▼
    Workflow driver
      │
      ▼
    SubAgentOrchestrator

### 11.3 DAG 执行

Kernel 管理依赖图，driver 批量调度可运行节点。节点可以并行执行，依赖输出通过 observation 回传。核心事件包括：

- workflow_node_submitted
- workflow_batch_spawned
- workflow_node_completed
- workflow_node_failed
- workflow_completed

Workflow 支持：

- DAG 依赖与并行 batch。
- reducer、fan-in、loop、classify、tournament。
- output schema 失败后的 retry。
- quota、worktree、quarantine 和动态节点提交。
- FileWorkflowStore、动态 artifact、process 与 replay。

Workflow 的执行结果仍通过父运行的事件和 Journal 记录，便于恢复与审计。

## 12. API 子路径清单

### 12.1 providers

包括：

- Provider factory：DeepSeek、Kimi、Qwen、GLM、MiniMax、Gemini、Ollama 等。
- OpenAIChatProvider、OpenAIResponsesProvider、AnthropicProvider 和兼容 Provider。
- adapters、capability router、circuit breaker、provider errors。
- endpoints、credentials、model catalog、model registry。
- content normalization、request plan、prepared request。

适合需要自行构造 Provider、覆盖 endpoint、配置凭证或读取能力矩阵的应用。

### 12.2 workflow

包括：

- Workflow definition、contracts、handoffs、modes。
- SubAgentOrchestrator、reducers、FileWorkflowStore。
- dynamic workflow artifacts、process、replay。
- skill/tool helper：scanSkillDir、readSkillFile、executeTools、readFile、validateToolArguments。

### 12.3 planes

包括：

- GitWorktreeManager、Worktree plane。
- Filtered、ProcessSandbox、McpProxy、RemoteVpc plane。
- ArchiveStore、PayloadStore、credential vault 和相关协议。

### 12.4 memory

包括：

- WorkingMemory、DurableMemory、InMemoryMemoryStore。
- retention、ranking、extraction、Memory protocol。
- store interface、scope、namespace、recall 与 pinning 类型。

### 12.5 harness

包括：

- AttemptLoop、RuntimeAttemptBody。
- carry、stop policy、judge、judge runner。
- Manifest、Nudge API 和 harness result。

### 12.6 os

包括：

- Native OS profiles、signals、permission manager。
- replay provider、OS snapshot、native primitives。
- verifiable report 和平台能力描述。

### 12.7 runtime

包括：

- RuntimeRunner、runAgent()、runFanout()。
- SessionLog、FileSessionLog、InMemorySessionLog。
- KernelJournal、FileKernelJournal、InMemoryKernelJournal。
- ContextManager、context evolution、execution evidence。
- runtime facade、reactive session、checkpoints、event stream 与 reliability。

### 12.8 evals

包括：

- Dataset、DatasetCase、Evaluator、EvalResult、EvalRun。
- buildEvalMessages()、judge、parse verdict、schema。
- trace 与评估运行记录。

### 12.9 advanced

这是面向高级集成的组合入口，重导出根 API，并额外提供：

- RuntimeRunner、LocalExecutionPlane。
- runtime facade、types/agent。
- RunGroup、event stream、reliability、turn policy。
- reactive session、checkpoint、Journal、diagnostics。

## 13. 取消、错误与恢复语义

### 13.1 取消

取消可以来自：

- AgentSession.interrupt()。
- AbortSignal。
- RunGroup budget 或 max turns。
- 用户拒绝审批。
- Provider 或工具层主动中止。

Runner 会尝试停止当前 Provider stream 和工具执行，再把取消原因交给 Kernel。RunResult.status 会映射为 cancelled，详细原因保存在 SessionLog 的取消或终态事件中。

### 13.2 错误分类

| 错误类别 | 典型来源 | 处理方式 |
| --- | --- | --- |
| 参数错误 | Tool schema、运行时选项、模型名 | 启动阶段或 action 阶段直接失败 |
| 认证错误 | Provider credential、MCP vault | 记录 evidence，通常终止 |
| Context overflow | Provider token limit、预算策略 | compact、裁剪或终止 |
| Transport error | 网络、超时、连接断开 | classify 后 retry 或终止 |
| Tool error | handler、MCP tools/call、沙箱 | 形成 tool result，交回 Kernel 决策 |
| Journal conflict | 并发恢复、CAS 冲突 | restore 后重试 |
| Commit failure | Kernel commit 异常 | 从 Journal rebuild |

### 13.3 观察与审计

要排查一次运行，建议依次查看：

1. run_started：入口参数、模型、Session 和 policy。
2. context_prepared：上下文槽位、裁剪和 frozen prefix。
3. prompt_measured：输入 token 与缓存 footprint。
4. provider_attempt：endpoint、fingerprint、usage、重试和错误。
5. tool_requested / tool_completed：参数、权限、输出和执行耗时。
6. memory_retrieved：查询、scope、命中和来源。
7. workflow_*：节点、批次、依赖和 reducer。
8. run_terminal：原始终止原因。

## 14. 当前实现中的注意点与文档漂移

以下结论来自当前源码检查，集成时应优先以源码和 API surface 测试为准：

1. **RunResult.usage 不是整次运行的累计 usage**。当前实现使用最后一次 Provider usage；需要总量时读取 provider_attempt 或 DoneEvent.totalTokens。
2. **RunResult.status 是粗粒度状态**。要区分 context overflow、no progress、token budget、max turns、milestone pending 或 invalid argument，需要读取 SessionLog/evidence。
3. **ModelRequirement 是声明式信息**。createRunner() 对字符串模型才调用 providerFor()，不会自动根据 capability 做完整路由。
4. **MCP 类型与 Agent facade 支持范围不完全一致**。类型可以表达 http、sse 和自定义 transport，但本地 facade 只降低 stdio；认证需要显式 vault。
5. **自定义 execution plane 不能与 mcpServers 同时使用**。需要将 MCP 接入自定义 plane 时，应由该 plane 自己负责连接和 schema 暴露。
6. **默认 SessionLog 是内存实现**。需要跨进程或崩溃恢复时，必须注入 FileSessionLog 或自定义持久化实现。
7. **自定义 SessionLog 需要对应 KernelJournal**。只实现业务事件日志而没有规范化 Journal，无法提供完整 Kernel 恢复语义。
8. **根级 Agent 不会自动创建 Provider**。应用需要提供 runtimeBinding.provider，或者提供可解析的字符串模型以使用 providerFor()。
9. **README 中的部分高级示例可能落后于实现**。例如 README 曾展示 runner.spawnSubAgent()，当前 RuntimeRunner 没有该方法，应使用 workflow 或 delegate API。
10. **Runtime facade 的部分能力没有根级导出**。runAgent、runFanout、RuntimeRunner 等应从 @deepstrike/sdk/runtime 或 @deepstrike/sdk/advanced 导入。
11. **原生 addon 与 bundler 需要单独处理**。部署时应确认平台可选依赖、Node ABI、打包器 external 配置和本地 Rust fallback 行为。

## 15. 推荐阅读顺序

想快速理解数据流，可以按以下顺序阅读源码：

1. [src/index.ts](src/index.ts)：确认根级公开 API。
2. [src/agent-facade.ts](src/agent-facade.ts)：理解 Agent 声明、Session 和 facade。
3. [src/runtime/agent-declaration.ts](src/runtime/agent-declaration.ts)：理解声明捕获与 host binding。
4. [src/runtime/agent-runtime-options.ts](src/runtime/agent-runtime-options.ts)：理解 facade 到 Runtime 的选项映射。
5. [src/runtime/runner.ts](src/runtime/runner.ts)：跟踪主循环、Provider 和工具 action。
6. [src/runtime/canonical-kernel-step.ts](src/runtime/canonical-kernel-step.ts)：理解 Journal、prepare、commit、recovery。
7. [src/runtime/session-log.ts](src/runtime/session-log.ts)：理解业务事件投影与持久化。
8. [src/runtime/context-manager.ts](src/runtime/context-manager.ts) 与 [src/runtime/context.ts](src/runtime/context.ts)：理解上下文预算和压缩。
9. [src/providers/request-plan.ts](src/providers/request-plan.ts) 与 [src/providers/prepared-request.ts](src/providers/prepared-request.ts)：理解 Provider 请求构造。
10. [src/providers/content-normalization.ts](src/providers/content-normalization.ts)：理解统一输入到厂商 wire 的边界。
11. [src/tools/index.ts](src/tools/index.ts) 与各 ExecutionPlane：理解工具参数、权限和远程执行。
12. [src/memory/public.ts](src/memory/public.ts) 与 Runtime memory action：理解 Memory 生命周期。
13. [src/workflow/public.ts](src/workflow/public.ts) 与 workflow driver：理解 DAG 和子 Agent。
14. [tests/api-surface.test.ts](tests/api-surface.test.ts)：确认哪些 API 是稳定根级入口，哪些只在子路径开放。

## 16. 集成建议

### 16.1 普通单 Agent

推荐使用根级 API：

    import { createAgent, createProvider, tool } from "@deepstrike/sdk";

    const agent = createAgent({
      name: "researcher",
      instructions: "收集信息并给出结构化结论。",
      tools: [
        tool({
          name: "lookup",
          description: "查询一个资源",
          parameters: {
            type: "object",
            properties: { query: { type: "string" } },
            required: ["query"],
          },
          execute: async ({ query }) => ({ query }),
        }),
      ],
      runtimeBinding: {
        provider: createProvider({ model: "openai:gpt-4o" }),
      },
    });

    const result = await agent.run("整理今天的研究目标");

生产环境应额外配置 FileSessionLog、MemoryStore、凭证来源、tool timeout、审批策略和资源预算。

### 16.2 自定义 Provider

从 @deepstrike/sdk/providers 导入 Provider 接口或适配器，确保：

- prepareRequest() 返回稳定、冻结的请求。
- descriptor() 能准确描述 modality、tool、stream 和 countTokens 能力。
- stream chunk 能映射为统一事件。
- usage、cache、stop reason 和错误可以重放或审计。
- request plan identity 在协议或模型切换时会变化。

### 16.3 自定义执行平面

从 @deepstrike/sdk/planes 或 @deepstrike/sdk/advanced 导入 ExecutionPlane 相关类型。自定义 plane 需要负责 schema 暴露、参数接收、权限检查、超时、取消、流事件和稳定的单结果约定。

### 16.4 可恢复运行

建议组合：

    FileSessionLog
      + FileKernelJournal
      + DurableMemoryStore
      + 明确的 provider credential resolver
      + 稳定的 tool id 与 schema
      + 固定的 session id

工具 schema、Provider protocol、Memory scope 和工作流节点名称都应保持稳定，否则重放时可能无法重建原始 action。

## 17. 总结

DeepStrike Node.js SDK 的数据流核心是：

    声明捕获
      → Runtime 组装
      → Canonical Kernel 产生活动
      → Provider / Tool / Memory / Workflow 执行
      → observation 回写
      → Journal 确认
      → SessionLog 投影
      → StreamEvent / RunResult 输出

在应用层，最常用的是 createAgent()、run()、stream()、session()、remember()、recall()、delegate() 和 workflow()。在平台层，最重要的是理解 RuntimeRunner、CanonicalKernelHost、KernelJournal、ExecutionPlane、LLMProvider 和 MemoryStore 之间的边界。

如果需要定位一次运行的真实行为，应沿着 run_started → context_prepared → provider_attempt / tool_requested → observation → run_terminal 的事件链查看；如果需要定位恢复问题，应沿着 KernelJournal step_seq → prepare → compareAndAppend → commit → wake 的规范化链查看。


## 18. 基于补充报告的源码核验与补充

本节把外部补充报告与当前 main 分支源码逐项对照。结论以源码和测试中的实际入口为准，补充报告中与当前实现不一致的名称已在本节修正。

### 18.1 五层架构的精确落点

补充报告将 SDK 分成 Intent、Runtime、Kernel ABI、Provider、Effect 五层，这个划分与当前目录和调用方向一致：

| 层 | 当前源码落点 | 主要职责 |
| --- | --- | --- |
| Intent | src/index.ts、src/agent-facade.ts、src/tools、src/workflow/definition.ts、src/evals/public.ts | 声明 Agent、工具、Provider、Workflow 和 Eval |
| Runtime | src/runtime/runner.ts、src/runtime/facade.ts、src/runtime/context.ts | 编排循环、上下文、证据、事件、恢复和工作流驱动 |
| Kernel ABI | src/kernel.ts、src/runtime/canonical-kernel-step.ts、@deepstrike/core | prepare、commit、abort、checkpoint、restore 和 action 产生 |
| Provider | src/providers/* | 模型路由、凭证、请求 fingerprint、协议适配、流解析和 usage |
| Effect | src/runtime/execution-plane.ts、各类 plane、MemoryStore、ArchiveStore、PayloadStore | 执行 Kernel 请求的宿主 I/O |

这五层与前文三层模型可以同时使用。三层适合解释整体控制权，五层适合解释 API 边界和源码责任。

### 18.2 根包入口核验

当前包名是 @deepstrike/sdk。根入口除了 Agent、工具和 Provider，还明确导出：

- createWorkflow 和 WorkflowDefinition、WorkflowStep、WorkflowResult。
- evaluate、Dataset、Evaluator、EvalRun 和 EvalTrace。
- safeTool、ok、fail、ToolError、formatToolError。
- createProvider、createProviderAsync、resolveProviderRuntime、resolveProviderRuntimeAsync。
- SESSION_EVENT_KINDS、InMemoryReactionCheckpointStore。
- ModelMessage、RuntimeMessage、StoredMessage、WireMessage 和 StreamEvent 系列。

因此，根包承担 Intent 层和稳定类型入口；RuntimeRunner、KernelJournal、ProviderRequestPlan 等底层能力继续通过 runtime、providers、advanced 等子路径访问。

### 18.3 Kernel action 到宿主处理器

当前 KernelRunnerAction 是一个 12 分支联合类型：

| Action | 宿主处理器 | 结果或后续动作 |
| --- | --- | --- |
| call_provider | prepare request、计量、stream Provider | provider_result 或 provider_error |
| execute_tool | hook、权限、ExecutionPlane | tool_completed |
| request_approval | onPermissionRequest | approval_resolved |
| spawn_workflow | workflow driver 启动或继续节点 | workflow_spawn_result |
| preempt_sub_agents | 中止子 Agent 的 Provider 调用 | 由 workflow driver 收尾 |
| persist_memory | MemoryStore.put | memory_persist_result |
| query_memory | MemoryStore.search | memory_query_result |
| archive_page_out | ArchiveStore 或 PayloadStore | page_out_archive_result |
| load_payload | 读取 opaque payload 并校验 digest | payload_loaded 或 payload_load_failed |
| evaluate_milestone | verifier 或 onMilestoneEvaluate | milestone_check_result |
| unsupported_effect | fail-closed | 直接失败 |
| done | 结束主循环 | run_terminal 和 done |

工具层还有三个重要的宿主边界：

1. update_plan 是宿主侧特判，会转换成 update_task 并提交回 Kernel。
2. submit_workflow_nodes 和 start_workflow 是 Kernel syscall。它们如果错误地以 execute_tool 到达宿主，Runner 会直接抛出 canonical boundary drift，避免旁路执行。
3. 普通工具由 ExecutionPlane 执行，工具结果可以携带结构化 contentParts。结构化块在当前 operation 的 toolOutputOverlay 中保留，SessionLog 的 durable 投影仍保存规范化文本和块信息。

### 18.4 Provider 的身份、计量和恢复语义

Provider 回合还有四个需要单独记录的事实：

1. invocationId 取效果链上的第一个 effectId，由 Journal 因果链保证身份稳定。重试动作继续同一 invocation，不在宿主侧重新铸造身份。
2. ProviderRequestPlan.fingerprint 同时绑定请求、路由范围和计量事实。Runner 在本次运行开始时从 SessionLog 建立 recordedMeasurements，重放时优先复用同 fingerprint 的事实。
3. preflight 计量优先使用 Provider 原生 countTokens；没有原生能力时才使用 heuristic。heuristic 计量不会单独触发 context_overflow 终止，避免估算误差导致假阳性终止。
4. Provider usage 到达后会写入 source 为 postflight、confidence 为 exact 的 prompt_measured 事实。该观测值覆盖同 fingerprint 的 preflight 估算，并成为后续重放的权威输入。

Provider 错误的可见性也由 Kernel 结果决定：

- transport ladder 耗尽后，Runner 追加 provider_attempt(status=transport_exhausted)。
- Runner 把 provider_error 交给 Kernel。
- Kernel 返回 call_provider 时，表示正在恢复，错误事件对外隐藏。
- Kernel 返回 done 时，Runner 才向 StreamEvent 发出 error。

### 18.5 四层消息体系与内容边界

当前类型体系可以按数据生命周期分为四层：

| 类型 | 所属层 | 说明 |
| --- | --- | --- |
| ModelMessage | 语义层 | Agent 与 Provider 共同理解的 role、content、contentParts、toolCalls |
| StoredMessage | 持久层 | 在语义消息上增加 messageId、createdAt 等存储信息 |
| RuntimeMessage | 执行层 | 保留 Runtime 需要的结构化 contentParts 和工具关联 |
| WireMessage | 协议层 | Provider adapter 私有的厂商请求形状 |

ContentPart 与 ToolOutputBlock 有意保持不对称。输入侧可以携带 text、image、audio、tool_result；工具输出侧可以携带 text、image、audio、video、file。Provider adapter 负责把这些内容转成厂商 wire message，根 API 不暴露厂商私有结构。

MediaSource 会携带 URL、base64、fileId 或对象句柄等来源信息。fileId 需要保留 endpoint affinity，不能在不同 Provider route 之间盲目复用。

### 18.6 SessionLog、KernelJournal 与 effect_id 关联

SessionLog 和 KernelJournal 分别承担证据投影与规范化状态链：

- KernelJournal 保存 staged outbound envelope、CAS append 的 record、checkpoint 和 acked prefix。
- SessionLog 保存 run_started、context_prepared、prompt_measured、provider_attempt、tool_requested、tool_completed、compressed、page_out、budget、entropy 和 run_terminal 等业务事件。
- step_seq 属于 KernelJournal，不复制到 SessionLog。
- 两套数据通过 effect_id、invocation_id 和 request_fingerprint 关联，SessionLog 不承担 Kernel 调度真相。

因此，SessionLog 可以用于 UI、审计和 EvalTrace；KernelJournal 才是恢复、幂等和 CAS 的权威来源。

### 18.7 AgentSession、RunResult 与 Evidence 的核验修正

当前 AgentSession 的实际接口是：

    run(goal, options?): Promise<RunResult>
    stream(goal, options?): AsyncIterable<StreamEvent>
    resume(options?): AsyncIterable<StreamEvent>
    interrupt(reason?): void

interrupt 的 reason 受限于 user、deadline、lease_lost、host_shutdown。它置位当前 Runner 的取消状态，随后由 Kernel 接收 cancel_operation。

RunResult 的实际稳定字段是：

- output
- runId
- sessionId
- status
- usage
- outputValidation
- evidence

RunResult 没有独立的 termination 字段。终止原因通过 SessionLog 的 run_terminal、operation_cancelled 和 evidence 查询。evidence 由最近一次 active run 的 context_prepared、provider_attempt、prompt_measured 和 runtime artifact binding 组装，因此复用 Session 时必须按 run_started 边界读取，不能取到旧运行的证据。

### 18.8 Harness、Nudge 与 Evals 的反馈闭环

Runtime、Harness 和 Evals 之间没有第二套执行协议，边界由稳定事件和 evidence 连接：

    RuntimeRunner
      → StreamEvent / SessionLog / RunResult.evidence
      → RuntimeAttemptBody
      → AttemptLoop
      → AttemptJudge
      → CarryPolicy 或 NudgeEngine
      → injectNote 信号
      → 下一次 Kernel transition

核心组件：

| 组件 | 代码位置 | 作用 |
| --- | --- | --- |
| RuntimeAttemptBody | src/harness/harness.ts | 把 Runtime stream 转成 token、tool、workflow、done 事件 |
| AttemptLoop | src/harness/harness.ts | 管理 attempt、carry、judge、stop 和累计预算 |
| AttemptJudge | src/harness/judge.ts | VerdictFnJudge、LlmEvalJudge、HybridJudge |
| NudgeEngine | src/harness/nudge.ts | 将 tool_error、tool_denied、turns、tool calls、entropy alert 折叠成 note |
| HarnessManifest | src/harness/manifest.ts | 管理可哈希的 instruction、nudge 和受限 Runtime patch |
| evaluate | src/evals/public.ts | 对 Dataset 执行 Agent.run，并可输出 EvalTrace |

默认 continueSession 保留同一 session transcript，Judge feedback 通过 runner.injectNote() 进入信号通道。freshWithFeedback 和 freshWithDigest 则显式创建新 Session。这样 CarryPolicy 不需要重新拼接内部 prompt，反馈仍能进入 Journal 和恢复链。

HarnessManifest 的 Runtime patch 只能修改白名单字段，工具和 Skill 面使用交集收窄。Manifest 可以收紧宿主能力，不能在运行时扩权。

Evals 的 evaluate() 直接消费 Agent.run() 返回的 output 和 evidence。includeTrace 开启后，EvalTrace 记录 executedInput、contextBinding、route、measurement 和 artifactSet。Judge 原语来自同一套 Kernel eval message、parseVerdict 和 verdictOutputSchema，因此一次性评估与 AttemptLoop 重试共享 Verdict 语义。

### 18.9 动态 Workflow 的当前边界

动态 Workflow 已经分成三个层次：

1. 预定义 DAG：RuntimeRunner.runWorkflow() 调度 WorkflowSpec。
2. 动态提交：RuntimeRunner.runDynamicWorkflow() 通过 DynamicWorkflowController 接收脚本提交，并调用 appendDynamicWorkflowNodes()。
3. 动态脚本：DynamicWorkflowVmExecutor 在受限 node:vm 中执行 DynamicWorkflowScript 或 artifact。

动态 Workflow 支持 agent、parallel、parallelAgents、pipeline、phase 和 log。默认限制是并发 16、单次运行最多 1000 个 Agent、一次 batch 最多 4096 个项目。artifact 使用 SHA-256 digest 绑定 replay identity。

当前 VM 只适合 trusted script：

- 禁止 require、import、process、global、Buffer、fetch、WebSocket、child_process、eval、Function 等能力。
- 禁止字符串和 WebAssembly code generation。
- 有 source bytes、单轮 timeout 和总执行时间限制。
- untrusted script 必须交给 OS sandbox，不能把 node:vm 当成租户隔离边界。

动态 Workflow 的生命周期事件包含 approval、pause/resume、phase、agent started/reused/completed、log、completed、failed 和 cancelled。失败时 Runtime 会提交 canonical cancel/preempt 序列，避免脚本在宿主退出但 Kernel 仍保持 active。

### 18.10 补充报告与源码的差异说明

补充报告整体与当前源码一致，但以下表述需要按当前实现理解：

- 根包名称是 @deepstrike/sdk，deepstrike 是概念简称。
- update_plan 可以由宿主处理；submit_workflow_nodes 和 start_workflow 到达宿主 execute_tool 分支属于 boundary drift，会 fail-closed。
- RunResult 没有 termination 字段，终止细节在 SessionLog 和 evidence。
- resume() 返回 AsyncIterable<StreamEvent>，interrupt() 是同步方法。
- 动态 Workflow 的 node:vm 只允许 trusted script；untrusted 执行必须使用 OS sandbox。
