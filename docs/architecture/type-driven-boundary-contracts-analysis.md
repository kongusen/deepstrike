# 边界协议与语言边界分析（v0.2.74）

 companion to [type-driven-boundary-contracts-plan.md](type-driven-boundary-contracts-plan.md).
 本文基于 v0.2.73（`70166972`）代码现场盘点，作为协议注册表的依据。结论先行：

1. **语言边界只有三条**：public|host（lower）、host|kernel（project/decode）、host|provider（normalize/settle/plan）。runtime-internal 不是语言边界。
2. **主流程真实的跨层函数共 16 处**（下表），其中 4 处已强类型化，12 处返回 `Record<string, unknown>`；子系统深化另查明 Memory/Context/Workflow/Events/Signals 的 crossing（M1–M5、CT1–CT4、WF1–WF2、EV1、S1–S6，见第八节）。
3. **provider 线格式不是契约面**——encode/decode 是 adapter-local 的 vendor 特化（刻意设计）；契约落在类型化的 plan/normalize/settle 层。
4. **废弃分支发明的 `SkillSource` 四阶阶梯（SkillDeclaration→SkillRef→SkillRevision→SkillPackage）在 v0.2.73 不存在**，不予注册。协议只覆盖真实存在的 crossing。
5. **Boundary A 存在双降级分叉**：类型化的 `lowerAgent → AgentSpec → projectAgent*` 在运行时无消费方（仅 conformance），真实 run 路径手写内联降级且两链覆盖面已分叉。P2 必须先收敛再注册，否则契约守护旁路。**已裁决（2026-09-22）：舍弃旁路、实路线提取**（裁决全文见图三后）。
6. **Memory 的信任边界在过界瞬间盖章**：`MemoryProvenance`（author/trust）不在 wire 也不在 public 参数里，由宿主在数据过界瞬间按来源赋值（model→`untrusted`，public 直写→`user_asserted`）——信任级是**路径属性**不是数据属性；协议模型的 `derived` 字段族需支持 crossing-time derivation（第八节 8.1）。
7. **Context 渲染权威在 kernel**：`call_provider` effect 携带每 turn 渲染好的 context，host 解码后再 plan——⑩⑪ 是**每 turn 热路径**而非恢复/重放路径；`configure_run` 是**复合配置 crossing**（一次过界捆绑 governance/context_policy/reliability/signal_policy 四子政策）。
8. **"一协议多 adapter"是机制级需求**：capability 族（1 协议 5 adapter）、`configure_run`（4 子政策）、events（约 19 个 yield 点）三个真实现场都要求多 adapter 支持，应在 P3 前升格为前置机制任务（第八节 8.6）。
9. **信号输入机制是"单实现双消费"的防漂移样本**：`injectNote` 与 `signalSource` 两条入站通道汇入同一 drain（代码注释明言 "so they never drift"），`signalToKernelEvent` 被主循环 poll 与 workflow 抢占监视器共享；`deliver_signal ↔ signal_delivery_disposed` 的 (delivery_id, attempt) 恰一回执是字段形状之外的**会话级关联不变量**（第八节 8.6）。

## 一、四层语言 → 实际代码映射

| 层 | 权威 | 实际落点（node SDK） |
|---|---|---|
| public | public-agent | `Agent`/`AgentDefinition`（agent-facade.ts）、`Skill`（skill.ts）、`Tool` 公共面、`Eval`/`Dataset`/`Evaluator`（evals） |
| host | host-runtime | `AgentSpec`/`AgentLoweringInputs`（agent-ir.ts）、`RenderedContext`、`SkillMetadata`（skills/loader.ts）、`MemoryRecord`/`MemoryProvenance`/`MemoryTrustLevel`（memory/protocols.ts）、`ContextPolicy`/`ContextPolicyWire`（runtime/context-policy.ts）、`ProviderAttempt`/`ModelInvocation`/`ModelUsageSettlement`（execution-evidence.ts）、`NormalizedProviderUsage`/`ProviderRequestPlan`（providers/request-plan.ts） |
| kernel | kernel | Rust canonical ABI——TS 侧**按设计**是无类型 wire（`Record<string, unknown>`）；`KernelObservation`、`EntropySample` 是反向观察面 |
| provider | provider | vendor wire（adapter-local、opaque）；`LLMProvider.complete` 是膜本身 |

关键不对称：TS↔kernel 边界故意无类型（与 Rust ABI 解耦）。0.2.74 的转向机制正是给 kernel 面投影补**宿主侧命名目标类型**（`KernelSkillMetadata` 模式），让契约可被类型驱动，而不动 ABI。

## 二、真实 crossing 清单

### Boundary A：public → host（verb: lower）

| # | crossing | 函数 | 落点 | 类型化 |
|---|---|---|---|---|
| 1 | `Agent → AgentSpec` | `lowerAgent` | agent-ir.ts:168 | ✅ 已强类型 |

上表是最初设计的形式入口，不是实际运行入口；`lowerAgent` + 5 个 `projectAgent*` 投影（run/context/capabilities/governance/delegation）未被 facade 执行路径消费。2026-09-23 已提取真实配置转换到 `buildAgentRuntimeOptions`，进度见三点二。

**⚠️ 现场警示（双降级分叉）**：`lowerAgent`/`projectAgent*` 在运行时路径上**没有消费方**——只有 conformance 与 advanced/runtime 公共再导出引用它们。真实 run 路径（`AgentRuntimeImpl`/`AgentSessionImpl`）直接读 `this.definition`（30 处），在构造 `RuntimeOptions` 时**手写内联降级**：`instructions+outputSchema→systemPrompt`、`skills→skillCatalog`、`knowledge→knowledgeSource`、`guardrails→governancePolicy`、`handoffs→delegate()` 内联消费、`memoryStore/memoryScope/maxTokens/capabilityFilter` 直通。两条降级链覆盖面已经分叉（`projectAgent*` 不管 memory/maxTokens；内联链不用 AgentSpec），正是契约系统要防的静默漂移。**P2 注册前必须先收敛**——已裁决：舍弃旁路、实路线提取（见图三后裁决）。

### Boundary B：host → kernel（verb: project）

| # | crossing | 函数 | 落点 | 类型化 |
|---|---|---|---|---|
| 2 | `SkillMetadata → KernelSkillMetadata` | `skillMetadataToKernel` | kernel-step.ts:287 | ✅ **协议#1 已完成** |
| 3 | `ModelMessage → kernel wire` | `messageToKernelMessage` | kernel-step.ts:311 | Record |
| 4 | `ToolSchema → kernel wire` | `toolSchemaToKernel` | kernel-step.ts:269 | Record |
| 5 | `ToolExecutionResult → kernel wire` | `toolResultToKernel` | kernel-step.ts:350 | Record |
| 6 | `TaskUpdate → kernel wire` | `taskUpdateToKernel` | kernel-step.ts:364 | Record |
| 7 | capability 族（tool/skill/marker/mount/unmount）→ kernel wire | `capability*` 5 函数 | kernel-step.ts:375-419 | Record |
| 8 | content parts 编解码 | `encode/decodeCanonicalContentParts` | kernel-step.ts:43-47 | 编解码，非语义 crossing（见"不做"） |

### Boundary B 反向：kernel → host（verb: decode）

| # | crossing | 函数 | 落点 | 类型化 |
|---|---|---|---|---|
| 9 | `KernelObservation → EntropySample` | `entropySampleFromObservation` | kernel-step.ts:421 | ✅ |
| 10 | journal raw → `ModelMessage` | `kernelMessageToSdk` | kernel-step.ts:433 | Record→typed（**热路径**：canonical-kernel-step.ts:178/262 每 turn 解码） |
| 11 | journal raw → `RenderedContext` | `renderedContextToSdk` | kernel-step.ts:505 | Record→typed（**热路径**：`call_provider` effect 携带 kernel 渲染的 context，经 canonicalActionFromProjectionJson 解码） |

### Boundary C：host ↔ provider（verbs: normalize / settle / plan）

| # | crossing | 函数 | 落点 | 类型化 |
|---|---|---|---|---|
| 12 | `ProviderUsage → NormalizedProviderUsage` | `normalizeProviderUsage` | request-plan.ts:248 | ✅ |
| 13 | `NormalizedProviderUsage → ModelUsageSettlement` | `UsageAccountingPolicy.settle` | execution-evidence.ts | ✅ |
| 14 | `RenderedContext + tools → ProviderRequestPlan` | `createProviderRequestPlan` | request-plan.ts:66 | ✅ |
| 15 | vendor wire → `UsageEvent`/`ProviderUsage` | 各 adapter 内部（Template Method 钩子） | anthropic.ts / openai-chat.ts / … | **契约面外**（见边界判定） |
| 16 | skill frontmatter → `SkillMetadata` | `scanSkillDir`/`parseSkillFrontmatter` | skills/loader.ts:60 | 同权威转换，非语言 crossing |

## 三、语言边界判定

**Boundary A（public|host）**：声明与执行的分界。目标是由唯一、被真实运行路径消费的 adapter 将 public-agent 声明转换为 host-runtime 配置；原先以 `lowerAgent` 为唯一 crossing 的设计未落地，按图三后的裁决退役。旧 checker 的"public 源禁 import kernel/runtime"规则属于此边界，需在定义与运行时绑定分离后落实，不能当作当前 facade 已满足的性质。

**Boundary B（host|kernel）**：syscall 膜。两个方向语义不同：
- project（host→kernel）：数据下沉为 kernel 事实。**身份铸造规则属于此边界**——`EffectId` 只能由 kernel 投影铸造（废弃分支 identity.json 的这条政策值得保留为 checker 规则，而非逐函数 JSON）。
- decode（kernel→host）：观察面重建。投影是只读的，不得反铸身份。

**Boundary C（host|provider）**：vendor 膜。**这是本次分析最重要的判定**：线格式的 encode/decode 是 adapter-local 且 vendor 特化（各家钩子、方言、能力差异是特性不是债务）。契约不落在 wire 上，落在三个类型化语义点上：plan（请求前测量）、normalize（vendor→受控词表）、settle（测量→结算）。抽象这三者即可覆盖 provider 膜的语义；逐 vendor 函数不注册协议。

**Runtime-internal 不是语言边界**：`SkillMetadata` 的 frontmatter 解析是同一权威内的 materialize，数据没有跨权威移动。废弃分支为它发明了四阶阶梯和三个 crossing JSON——那是"为契约造架构"的偏差根源，不重犯。

## 三点一、真实运行路径审计（2026-09-22）

本节保留 2026-09-22 对 Node facade、`RuntimeRunner` 和 provider 调用的审计基线。原审计没有修改运行时代码；结论以源码和构建后 provider spy 的实际观察为准。后续修复状态见三点二，以下原始故障及行号不代表修复后的当前状态。

### 1. 真实执行路径与形式 lowering 路径分叉

真实的 `Agent.run()` 路径是：

```text
createAgent(definition)
  → AgentRuntimeImpl
  → run()/stream()
  → createRunner() 内联拼接 RuntimeOptions
  → RuntimeRunner.run()
  → kernel set_tools / runSpec
  → call_provider
  → provider request
  → SessionLog / RunResult
```

`createRunner()` 直接读取 `this.definition`，内联处理 `instructions`、`outputSchema`、`skills`、`knowledge`、`guardrails`、`memoryStore`、`memoryScope`、`maxTokens` 和 `capabilityFilter`（`node/src/agent-facade.ts:372-433`）。`lowerAgent()` 及其五个 `projectAgent*` 投影没有进入该路径，仍然只被 conformance、测试和 advanced/runtime 导出消费。

因此，`AgentSpec` 目前不是运行时的权威中间表示。Boundary A 存在两套实现，任何只检查 `lowerAgent()` 的契约都可能守护旁路。

### 2. 已复现的用户可见故障：声明的工具不会进入 provider schema

`RuntimeRunner` 将未设置的 `baselineToolIds` 解释为最小初始暴露面（`node/src/runtime/runner.ts:536-549`），并在根 run spec 中写入空的 `exposureBaseline`（`node/src/runtime/runner.ts:2247-2276`）。facade 没有把 `AgentDefinition.tools` 转换为 `baselineToolIds`。

`set_tools` 仍会把 execution plane 的全部 schema 装入 kernel（`node/src/runtime/runner.ts:2140-2143`），但 provider 看到的 schema 会经过 root exposure baseline 过滤。用构建后的 dist 创建一个带单个 `visible` 工具的 Agent，拦截 `ReplayProvider.stream()` 的第二个参数，实际结果为：

```text
[[]]
```

这说明公共 Agent 已注册工具，但 provider 请求中没有用户工具。现有 facade 测试只断言隐藏工具“不在列表中”，空数组也会通过（`node/tests/agent-facade.test.ts:30-49`、`:72-91`），所以测试目前无法发现这个回归。

### 3. 声明语义在真实路径中丢失或被隐式覆盖

- 历史上的 formal IR 曾将 `providerOptions` 保存为 `extensions`，但 facade 没有把 `definition.providerOptions` 放入 `RuntimeOptions.extensions`，也没有把它传给 `runner.run()`（`node/src/agent-facade.ts:405-432`、`:287-300`）。该旁路已在 0.2.74 退役，当前 live facade adapter 负责这条语义。
- `AgentOptions.memory` 会进入 IR，但真实 runner 只接收 `memoryStore` 和 `memoryScope`；声明式 memory 在公共 run 路径没有对应的绑定（`node/src/agent.ts:31-48`、`node/src/agent-facade.ts:421-422`）。
- `runtimeBinding.runtimeOptions` 在 facade 已经合并 guardrail、skill 和 knowledge 后整体展开（`node/src/agent-facade.ts:407-430`），可以静默覆盖这些值。配置优先级没有单独的类型或契约表达。

### 4. 设施 API 绕过统一 authority

`remember()` 和 `recall()` 直接调用 `MemoryStore.put/search`（`node/src/agent-facade.ts:188-224`），没有经过 RuntimeRunner 的 kernel memory syscall、治理检查和审计事件。现有执行模型文档明确规定 memory 操作必须经过验证和治理（`docs/en/architecture/execution-model.md:83-85`）。

`delegate()` 会校验 `request.target` 是否在 handoff allowlist 中，但随后仍通过当前 Agent 的 `workflow()` 创建 runner；target 没有成为实际的 provider 或 Agent 绑定（`node/src/agent-facade.ts:227-260`）。同样，`lowerWorkflowDefinition()` 会生成 `WorkflowNodeSpec.agent`，而 `workflowNodeSpecToKernel()` 不会发出这个字段（`node/src/workflow/definition.ts:21-43`、`node/src/types/agent.ts:567-589`）。如果目标语义是执行指定 Agent，这条绑定目前只停留在 host metadata。

### 5. Run/Session 状态没有按 session 隔离

`AgentRuntimeImpl` 只有一个 `activeRunner`（`node/src/agent-facade.ts:170-175`）。所有 `AgentSession.interrupt()` 最终调用 owner 的同一个 `interrupt()`，无法保证只中断当前 session（`node/src/agent-facade.ts:150-167`、`:352-354`）。`run()` 还会从整个 session log 反向寻找最近的 `run_started`、`context_prepared` 和 `provider_attempt`，没有按当前 run ID 关联证据（`node/src/agent-facade.ts:304-323`）。同一 session 并发运行时，这会造成中断目标和结果 evidence 的竞态。

### 6. 配置归属与对象边界不符合既有规范

`AgentDefinition` 同时携带声明式 Agent 字段和 provider、execution plane、session log、store 等运行时对象（`node/src/agent-facade.ts:18-35`）。构造函数只做浅层 `Object.freeze`（`:178-181`），嵌套数组、对象和运行时 binding 仍可变，也无法满足规范中“Agent 保持可序列化定义、执行状态放在 Session/Run”的目标（`docs/specs/unified-public-agent-model.md:167-176`、`:211-217`）。

### 严重性排序

| 等级 | 问题 | 影响 |
|---|---|---|
| P0/P1 | `baselineToolIds` 未从 facade 绑定，provider 收到空工具集 | Agent 的核心工具能力在公共入口失效 |
| P1 | `providerOptions`、声明式 memory 未进入真实 run | 类型化声明与实际 provider/kernel 行为不一致 |
| P1 | memory 直写、handoff 不切换 target | 绕过治理、审计和目标 Agent 语义 |
| P1 | 单一 `activeRunner`、evidence 不按 run 关联 | 并发 session 的中断和结果可能串线 |
| P2 | facade 内联降级、浅冻结、运行时对象混入 definition | 契约漂移、不可序列化和长期维护成本上升 |

### 修复前置顺序

1. 先补真实入口的行为测试：工具暴露、capability filter、provider extensions、guardrail 优先级、memory 审计、handoff target 和 session interrupt。
2. 将 `AgentDefinition`（纯声明）与 `AgentRuntimeBinding`（运行时依赖）分开，生成不可变的规范化 `AgentSpec`。
3. 提供唯一的 `bindAgent(spec, binding, runOptions)` 或等价 `RuntimePlan`，由真实 facade 消费；其中显式设置工具暴露面并传递 `extensions`。
4. 将 memory、handoff 和 workflow target 接入同一套 runtime/kernel authority；为每个 run 保存独立执行句柄，按 `runId` 关联 evidence。
5. Boundary A 收敛后再注册 `agent.public-to-host`。当前 `contracts/protocols/registry.ts` 只注册 Skill 协议，直接注册 Agent 会让 checker 守护未被运行时消费的旁路。

本次核验中，`npm run contracts:check`、`cd node && npm run build` 和四个目标测试套件（22 个测试）均为绿色；这些结果只能证明现有类型、构建和局部断言成立，不能证明 `createAgent().run()` 的端到端语义已经闭合。

## 三点二、实路线首批修复（2026-09-23）

已按“提取实路线、退役旁路”的裁决完成第一批可独立验证的改动。

| 提交 | 内容 | 运行时证据 |
|---|---|---|
| `a2ef8e39` | 提取 `node/src/runtime/agent-runtime-options.ts` 的 `buildAgentRuntimeOptions`，统一组装 `RuntimeOptions` | 提取前后 5 个目标套件、25 个测试通过；provider 解析与执行面连接仍由 facade 管理 |
| `cfeefd8e` | 公共 Agent 将已绑定工具设为初始 baseline；MCP 连接和工具发现完成后才创建 runner | 本地工具可见且执行；自定义执行面受 capability ceiling 限制；MCP 与本地工具可见，连续运行能执行 MCP 工具 |
| `c6b049b6` | `providerOptions → extensions` 透传；避免宿主策略覆盖已合并 guardrails；保留 `ask_user` 默认动作 | run、stream、session、workflow、resume 均收到 extensions；宿主与 Agent veto 同时生效；拒绝审批后工具不执行 |

真实路径现为 `createAgent → createRunner（解析 provider、准备 execution plane）→ buildAgentRuntimeOptions → RuntimeRunner`。本轮直接复用 `RuntimeOptions` 作为目标类型，没有新增一份不被执行消费的 IR。底层 RuntimeRunner 对缺省 baseline 的最小暴露语义保持不变，只有公共 Agent 显式选择已绑定的工具；capability ceiling 和治理过滤继续由既有执行链执行。

配置优先级保持明确的局部规则：宿主 `skillCatalog`/`knowledgeSource` 覆盖声明式默认来源；治理策略合并后写入，避免尾部展开覆盖；默认动作保留 deny 优先、其次 ask_user；run 的 `onPermissionRequest` 优先于宿主回调。此轮没有重定义已有治理规则列表内部的匹配顺序。

新增 `node/tests/agent-runtime-path.test.ts` 的 11 个运行路径测试，并将 facade 中两个只检查“不包含”的断言改为精确的可见工具集断言。工具暴露、extensions 和 guardrail 覆盖均先复现失败，再验证修复。校验结果为 Node 构建通过、181 个非在线测试套件 / 1139 个测试通过、`contracts:check` 与 `contracts:verify` 通过；六个需要在线 provider 的测试套件未运行。契约检查目前仍只覆盖已注册的 Skill 协议，不能据此宣称 Agent 契约已注册。

后续仍需完成定义与 binding 分离、声明式 memory 绑定与审计入口、Run/Session 隔离、handoff target、Agent 协议注册和旧 IR 退役。上述原审计缺陷中的这些部分保持待办，后续次序记录于 companion plan。

## 四、协议注册表路线

机制已由协议 #1（skill.host-to-kernel）证明。扩展顺序按**类型化成本**递增：

| 批次 | 协议 | 前置工作 |
|---|---|---|
| ✅ P1 | `skill.host-to-kernel` | 无（试点） |
| P2 | `agent.public-to-host` | **已裁决（2026-09-22）**：提取 facade 内联降级为唯一命名函数（行为等价）→ 补行为测试 → 修三点一 P0/P1 → 注册；`lowerAgent`/`projectAgent*` 走 deprecation 窗退役（裁决全文见图三后） |
| P3 | kernel 投影族：`message` / `tool-schema` / `tool-result` / `task-update` | 每个需先补 `Kernel*` 命名目标类型（复制 `KernelSkillMetadata` 模式）；capability 族作为一个协议、五个 adapter，需先扩展机制支持多 adapter |
| P4 | kernel 观察面：`entropy` / `kernel-message` / `rendered-context` decode | ⑩⑪ 已确认为**每 turn 热路径**（8.2），必须契约化；需补 `Kernel*` 命名目标类型（同 P3 模式） |
| P5 | provider 语义点：`usage-normalize` / `usage-settle` / `request-plan` | 已类型化；settle 的 forbidden（pricing_authority）直接沿用旧裁决 |
| P6 | 子系统族：`memory.kernel-effect`（M1–M3）/ `run-config.configure-run`（CT2–CT3）/ workflow（WF1–WF2）/ `signal.deliver`（S1–S2） | 前置 = 多 adapter 机制（与 P3 共享）+ `KernelMemory*`/`KernelSignalInput` 命名 wire 类型；M4 直写旁路先按三点一.4 收编 |

**每批的完成判据**：该批所有 crossing 通过 `contracts:check`（编译器验证推断）+ 生成验证器有泄漏/缺失/类型形状测试 + `contracts:verify` 绿。

## 五、checker 应吸收的全局规则（来自废弃分支的遗产）

以下三条是**边界级政策**，不属于任何单个协议，应作为 checker 的全局规则实现（替代旧的正则 JSON 检查）：

1. **身份铸造**：宿主源码不得出现 `effectId = randomUUID/uuid4` 式铸造（kernel 投影外禁铸 EffectId）。
2. **层级 import 禁令**：public 源（agent.ts/skill.ts/evals）不得 import runtime/kernel 权威；provider 源不得 import kernel 权威。
3. **动词词表**：crossing verb 必须属于 `lower/project/encode/decode/normalize/prepare/resolve/render/settle/bind/materialize`（已在 types.ts）。

## 六、明确不注册的

- **provider wire encode/decode**：vendor 特化是设计（见边界 C 判定）；契约= `LLMProvider` 接口本身。
- **`SkillSource` 阶梯**：v0.2.73 不存在，不预建架构。
- **`encodeCanonicalContentParts`**：内容编解码（base64 容器），无语义字段可推断。
- **`projectAgent*` 五投影**：同一 `AgentSpec` 内部的切片视图，权威未移动；若未来需要，走 runtime-internal 族再议。

## 七、附图

### 图一：四层架构与三条语言边界全景

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  PUBLIC 层（public-agent 权威）— 用户声明语言                                 │
│  Agent · Skill · Tool · Memory · Knowledge · MCPServer · Handoff · Eval      │
│  落点: agent-facade.ts(Agent/AgentDefinition) · skill.ts(Skill) · evals/     │
└──────────────────────────────────┬──────────────────────────────────────────┘
                    ═══════════════╪══════════════════════════════════════════
                    ║  BOUNDARY A: public|host · verb=lower · 唯一 crossing      ║
                    ╚═══════════════════════════════════════════════════════════╝
                                     │  ① Agent → AgentSpec  (lowerAgent ⚠️旁路)
┌──────────────────────────────────┴──────────────────────────────────────────┐
│  HOST 层（host-runtime 权威）— 执行语言                                       │
│  AgentSpec · RenderedContext · SkillMetadata · ProviderRequestPlan           │
│  NormalizedProviderUsage · ModelUsageSettlement · ProviderAttempt            │
│  落点: agent-ir.ts · runner.ts · skills/loader.ts · providers/request-plan.ts│
│       · runtime/execution-evidence.ts                                        │
│                                                                             │
│   ┌───────────── BOUNDARY B: host|kernel（syscall 膜）─────────────────┐     │
│   │                                                                    │     │
│   │  project 下沉（host→kernel）      decode 观察重建（kernel→host）    │     │
│   │  ② skillMetadataToKernel ✅       ⑨ entropySampleFromObservation ✅│     │
│   │  ③ messageToKernelMessage         ⑩ kernelMessageToSdk             │     │
│   │  ④ toolSchemaToKernel             ⑪ renderedContextToSdk           │     │
│   │  ⑤ toolResultToKernel                                             │     │
│   │  ⑥ taskUpdateToKernel                                             │     │
│   │  ⑦ capability* ×5（tool/skill/marker/mount/unmount）               │     │
│   │  ⑧ encodeCanonicalContentParts（编解码，非语义）                    │     │
└───┴──────────────────────────────────┬──────────────────────────────────┴─────┘
                                       │
                        ┌──────────────┴──────────────┐
                        │  KERNEL 层（kernel 权威）     │
                        │  Rust canonical ABI          │
                        │  TS 侧按设计无类型（wire      │
                        │  Record）；唯一身份铸造方      │
                        │  （EffectId 禁宿主铸造）       │
                        └─────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│  BOUNDARY C: host|provider（vendor 膜）— 契约在语义点，不在 wire              │
│                                                                             │
│   ⑫ normalizeProviderUsage ✅      （ProviderUsage → Normalized）             │
│   ⑬ UsageAccountingPolicy.settle ✅（Normalized → ModelUsageSettlement）      │
│   ⑭ createProviderRequestPlan ✅   （RenderedContext+tools → Plan）           │
│   ⑮ vendor wire 编解码 ────────── ✗ 不注册：adapter-local、vendor 特化是设计   │
│                                                                             │
│  ┌────────────────────────────────────────────────────────────────┐         │
│  │  PROVIDER 层（provider 权威）：anthropic.ts · openai-chat.ts ·  │         │
│  │  gemini.ts · ollama.ts（Template Method 钩子，wire opaque）      │         │
│  └────────────────────────────────────────────────────────────────┘         │
└─────────────────────────────────────────────────────────────────────────────┘

  ✅ = 已强类型化（4 处）    ⚠️ = 存在但运行时旁路    其余 = Record<string,unknown>
```

### 图二：一次真实 run 的数据流（实路线与旁路）

```
 user: agent.run("...")
   │
   ▼
┌─────────────────────────── PUBLIC ───────────────────────────┐
│ AgentRuntimeImpl.run()                                       │
└──────────────────────────┬───────────────────────────────────┘
                           │
            ┌──────────────┴───────────────┐
            │   BOUNDARY A 双链分叉 ⚠️      │
            ▼                              ▼
   ┌─ 实路线（内联手写）────────┐   ┌─ 旁路（已类型化，无人调用）─┐
   │ this.definition 直读×30   │   │ lowerAgent() → AgentSpec   │
   │ instructions+outputSchema │   │ projectAgentRun/Context/   │
   │   → systemPrompt          │   │   Capabilities/Governance/ │
   │ skills → skillCatalog     │   │   Delegation               │
   │ knowledge → knowledgeSrc  │   │ （仅 conformance 消费）     │
   │ handoffs → delegate() 内联│   └────────────────────────────┘
   └──────────────┬────────────┘
                  ▼
┌─────────────────────────── HOST ─────────────────────────────┐
│ RuntimeRunner 主循环（每 turn）：                             │
│                                                              │
│  kernel 渲染 context（effect）→ ⑪ decode → ⑭ request-plan    │
│            │                              │                  │
│            │            BOUNDARY C        ▼                  │
│            │        vendor wire（opaque，不注册）             │
│            │                              │                  │
│            │      UsageEvent/ProviderUsage ◀─┘                │
│            │                   │                              │
│            │        normalizeProviderUsage ⑫                 │
│            │                   │                              │
│            │        UsageAccountingPolicy.settle ⑬ → Settlement│
└────────────┼───────────────────┼──────────────────────────────┘
             │ BOUNDARY B        │
             ▼ project 下沉      ▼
   ┌──────────────────────────────────────────────┐
   │ kernel step（syscall）：                      │
   │  messageToKernelMessage ③  每条消息           │
   │  toolSchemaToKernel ④     executionPlane 全量│
   │  skillMetadataToKernel ②  激活时              │
   │  taskUpdateToKernel ⑥ / capability* ⑦        │
   └──────────────┬───────────────────────────────┘
                  ▼
           ┌───────────── KERNEL（Rust ABI）──────┐
           │ 铸 EffectId · 记 Journal · 观察       │
           └──────────────┬───────────────────────┘
                                 │ KernelObservation
                                 ▼
                  entropySampleFromObservation ⑨ → EntropySample
                  （⑩⑪ journal→ModelMessage/RenderedContext
                    = 每 turn 热路径 decode，非仅恢复/重放）
```

### 图三：Boundary A 双降级分叉（P2 前置收敛的目标）

```
            AgentDefinition（public 声明）
                    │
        ┌───────────┴────────────┐
        ▼                        ▼
  实路线：内联降级          形式链：lowerAgent
  （agent-facade.ts:395-434）（agent-ir.ts:168，已类型化）
        │                        │
        │ systemPrompt 拼接       │ AgentSpec{instructions,
        │ skillCatalog            │   outputSchema, tools, ...}
        │ knowledgeSource         │        │
        │ governancePolicy        │        ▼
        │ memoryStore/Scope ✓     │ 5× projectAgent*
        │ maxTokens ✓             │ （memory ✗ maxTokens ✗）
        │ capabilityFilter ✓      │
        │ handoffs: delegate 内联  │ handoffs: projectAgentDelegation
        ▼                        ▼
     RuntimeOptions          AgentLoweringInputs
        │                        │
        │  实际执行 ✓             │  无人调用 ✗（仅 conformance）
        └────────────────────────┘
         两链覆盖面已分叉 = 静默漂移温床
         这正是契约系统存在要防的事
```

**P2 收敛裁决（2026-09-22 已拍板）：舍弃旁路，从实路线着手**——不吸收 `lowerAgent`，而是退役它。

退役面（已核实）：

| 面 | 内容 |
|---|---|
| node | agent-ir.ts（`lowerAgent`/`projectAgent*`/`AgentSpec`）+ conformance.ts:5 再导出 + runtime/public.ts:18 与 advanced/public.ts:22 五投影再导出 + runtime-language.ts:15 / runtime-classification.ts:22 词表登记 + 5 个测试文件（agent-ir / agent-ir-conformance / agent-projections / agent-definition-contract / runtime-classification） |
| wasm | agent-ir.ts 移植副本 + index.ts:133/135 再导出 |
| rust | 仅 sdk-conformance.rs 夹具镜像 |
| python | 无 formal 链，零动作 |

实施顺序：

1. **提取**：把 `createRunner()` 内联降级块（agent-facade.ts:372-433）提取为唯一命名函数（行为等价纯重构），run 路径消费它——它同时成为 `agent.public-to-host` 的注册 adapter。
2. **补行为测试**（三点一修复顺序第 1 步落在该缝上）：工具暴露 / capability filter / providerExtensions / guardrail 优先级 / memory / handoff target。
3. **修审计缺陷**：P0（baselineToolIds 未绑定→provider 空工具集）、providerOptions 透传、声明式 memory 绑定——在新函数处集中修复。
4. **注册** `agent.public-to-host`（新函数为 adapter）。
5. **退役旁路**：按 0.2.67 双写窗纪律——doc 级 `@deprecated`（conformance 保留至窗末），下一 minor 按 DEL 清单删除上表全部面；runtime/advanced 再导出与两处词表登记同步标注。

与三点一修复顺序的衔接：第 2-3 步"分离 definition/binding、生成不可变规范化计划、唯一 `bindAgent`"的**概念保留**，但规范化计划类型由新函数新建承载，不复用 agent-ir.ts 的 `AgentSpec`——避免新权威与退役面纠缠同名。

裁决理由：漂移源是"存在两个实现"，不是行进方向。删除未用实现使漂移在**构造上不可能**；吸收式收敛反而要先扩 `lowerAgent` 覆盖面（memory/maxTokens/capabilityFilter/handoffs 内联）再改写热路径，是侵入性最高的路。保留死的形式 IR 即"为契约造架构"，正是本次重做要根除的偏差模式；且 P0 级缺陷（三点一.2）证明修复精力应全部投在实路线。

**退役已落地（2026-09-23）**：Node/WASM 的 `agent-ir` 模块、五个投影 helper、conformance/runtime/advanced 再导出、Agent IR 专用测试和共享 conformance fixture/domain 已一并删除。运行时分类把 host 侧语义改为不可变 `AgentDeclaration` 快照；内核仍保留正在使用的 `LogicalAgentSpec` wire DTO，它不是被删除的 host formal IR。

## 八、子系统深化：Memory / Context / Workflow / Eval / Events（2026-09-22）

主流程（run 循环）之外逐子系统盘点。总判定：**Memory 与 Context 是真实跨边界子系统**（新增 M1–M5、CT1–CT4）；Workflow 是 B 边界的批量 syscall（WF1–WF2，随 P3 收编）；**Eval 留在 runtime-internal**；Events 是 B 反向的宽面 decode（EV1，多 adapter 机制的极限用例）；Signals 是外部世界的入站正门（S1–S6，单实现双消费的防漂移样本）。

### 8.1 Memory：信任边界在过界瞬间盖章

记忆的权威分工是**内核决策、宿主执行、过界盖章**：

| # | crossing | 落点 | 方向 | 类型化 |
|---|---|---|---|---|
| M1 | kernel effect 请求（`persist_memory`/`query_memory`）→ host 解码执行 | runner.ts:2792-2860 | B 反向 decode | wire=Record；宿主侧已命名 `MemoryRecord` |
| M2 | 执行回执（`memory_persist_result`/`memory_query_result`）→ kernel | runner.ts:2824/2852 | B 正向 project | Record |
| M3 | 过界盖章：canonical wire → `MemoryRecord` 时铸 provenance | `author:"model", trust:"untrusted"`（runner.ts:2805）+ 宿主铸 `record_id = memory:${uuid}` | M1 内嵌 | 语义明确、无类型守护 |
| M4 | public `agent.remember()` → `MemoryRecord`（`author:"host", trust:"user_asserted"`） | agent-facade.ts:188-224 | A 邻接（**直写旁路**） | ⚠️ 见三点一.4 |
| M5 | memory recall → context 桥接（`applyHostMemoryRecallLifecycle` + token 计账，key=`memory:${record_id}`） | runner.ts:838-848 | runtime-internal | 不注册 |

类型权威在 `memory/protocols.ts`：`MemoryKind / MemoryAuthor / MemoryTrustLevel / MemoryScope / MemoryProvenance / MemoryRecord`。

**关键发现：provenance 是 crossing-time derived**。`MemoryProvenance` 既不在 kernel wire 里、也不在 public 参数里——它在数据过界的瞬间由宿主按**来源**赋值（model 来源→`untrusted`；public/宿主→`user_asserted`）。信任级不是数据的属性，是**路径的属性**。这对协议模型提出新需求：`derived` 字段族需支持"过界时派生"语义（值不由 source 字段决定，由 crossing 位置决定）。M4 直写旁路（不过 kernel syscall、不过治理、不过审计）正是这个模型的反例：同一 `MemoryRecord` 类型、不同路径、不同信任语义——契约应把"经 kernel 的 memory"与"public 直写"分成两个协议看待，而不是一个类型两种待遇。

```
  kernel 决策                host 执行（盖章）              回执
┌────────────┐  M1 decode  ┌──────────────────┐ M2 project ┌───────────────┐
│ persist_   │ ──────────▶ │ wire→MemoryRecord │ ─────────▶ │ memory_        │
│ memory     │  Record     │ provenance 盖章：  │  result    │ persist_result │
├────────────┤             │  model→untrusted  ├───────────▶│ memory_        │
│ query_     │ ──────────▶ │ +铸 record_id     │            │ query_result   │
│ memory     │             └────────┬─────────┘            └───────────────┘
└────────────┘                      │ M5 桥接（runtime-internal）
      ▲                             ▼
      │ kernel syscall        context（memory 条目 + token 计账）
      │
      │       ⚠️ M4 直写旁路（三点一.4，P1）：public remember()/recall()
┌─────┴──────┐       直接 MemoryStore.put/search，不过 kernel/治理/审计
│ PUBLIC     │ ─────────────────────────────────────────────────────┘
└────────────┘
```

### 8.2 Context：渲染权威在 kernel，host 是每 turn 热路径解码方

**主流程分析的重大修正**：B 反向 decode 不是"恢复/重放专属"。kernel 拥有每 turn 上下文窗口的组装权——`call_provider` effect 直接携带渲染好的 `context`（kernel-step.ts:87），host 每 turn 经 `canonicalActionFromProjectionJson` → `renderedContextToSdk`（canonical-kernel-step.ts:178）解码成 `RenderedContext`，再走 ⑭ request-plan。**⑩⑪ 是每 turn 热路径**（图二尾注已同步修正）。

| # | crossing | 落点 | 方向 | 类型化 |
|---|---|---|---|---|
| CT1 | `call_provider.context` raw → `RenderedContext` | = crossing ⑪ | B 反向 decode | Record→typed，每 turn 热路径 |
| CT2 | run 初始化配置束 `configure_run`（K2） | runner.ts:1031-1123 | B 正向 project（**复合**） | Record |
| CT3 | `ContextPolicy → wire`（ratio→ppm） | `normalizeContextPolicy`（context-policy.ts，CT2 内嵌） | CT2 内嵌 | rename+derived（×10⁶ 单位换算） |
| CT4 | host `ContextManager`（6 类条目 + ledger 事件） | context-manager.ts | runtime-internal | 不注册 |

**CT2 是复合 crossing**：`configure_run` 一次过界捆绑四个子政策——governance（`governancePolicyToKernelEvent`，去 kind 后挂 `config.governance`）、`context_policy`（CT3）、reliability、signal_policy。加上 ⑦ capability 族（1 协议 5 adapter）与 EV1（约 19 个 yield 点），**"一协议多 adapter"已有三个真实现场**，应从 P3 的附带说明升格为机制级前置任务。

### 8.3 Workflow：B 边界的批量 syscall（WF1–WF2）

`submit_workflow_nodes` 与 `start_workflow` 的 action 现在使用命名的 `KernelWorkflowSpawnNode` / `KernelWorkflowBudget` DTO。runner 通过 `workflowSpawnNodeFromKernel` 与 `workflowBudgetFromKernel` 显式投影到 `WorkflowSpawnInfo` / `WorkflowBudget`，并在契约中记录 kernel 的 task/attempt/launch/node bookkeeping 丢弃策略；这把原先的匿名 `Record` action 面收敛为可检查的 kernel→host crossing。workflow target 绑定缺陷见三点一.4。

### 8.4 Eval：全在 runtime-internal（不注册）

`buildEvalMessages`（eval.ts:62）、`parseVerdict`（:73）、`verdictOutputSchema`（:84）——judge 的消息拼装与裁决解析都在 host 权威内部完成，无权威移动。与 `projectAgent*` 同判：不注册。

### 8.5 Events：观察面 → public StreamEvent（EV1，宽面 decode）

kernel observation → public `StreamEvent` 约 19 个 yield 点（runner.ts）。方向上是 B 反向 decode 的变体，但目标直达 public 可见面、表面极宽。它是多 adapter 机制的**极限测试用例**——若机制在 capability(5)/configure_run(4)/events(19) 上都成立，才算真正通用。建议排在 P3 多 adapter 落地之后，不与本轮绑定。

### 8.6 Signals：信号输入机制（S1–S6，"单实现双消费"的防漂移样本）

信号是外部世界进入运行中 agent 的唯一正门（对应 Agent OS 路线的 signals→interrupts 相位）。**权威分工与 Memory 正好对称：host 决定入队，kernel 决定处置**。

| # | crossing | 落点 | 方向 | 类型化 |
|---|---|---|---|---|
| S1 | host `RuntimeSignal` → kernel 输入事件 `deliver_signal` | `signalToKernelEvent`（runner.ts:3970） | B 正向 project | Record wire；renames×6 + derived×4 |
| S2 | kernel 处置回执 `signal_delivery_disposed` → host ack/nack | `consumeInboundSignal`（runner.ts:1870） | B 反向 decode→执行 | (delivery_id, attempt) 恰一关联 |
| S3 | `SignalSource.claim/ack/nack` 租约协议（public 可实现接口；SDK 默认 `SignalGateway`：FIFO+lease+cron+ingest+broadcast） | signals/types.ts:21、signals/gateway.ts:38 | public→host 接口 | ✅ 类型化 |
| S4 | public `injectNote(text, urgency)` → `RuntimeSignal{payload:{goal:text}}` | runner.ts:1822 | A 邻接 push | ✅ |
| S5 | `SignalPolicy`（queueMax/ttlMs/deadlineEscalation）→ `configure_run.signal_policy` | runner.ts:1050-1052 | CT2 成员 | renames（queue_max/ttl_ms） |
| S6 | kernel `preempt_sub_agents` action → host 抢占子 agent | kernel-step.ts:80 | B 反向（kernel 驱动的宿主动作） | 部分 |

**关键发现一：防漂移动机已自我陈述在代码注释里**。`injectNote`（host push）与 `signalSource`（lease pull）两条入站通道汇入同一 `nextInboundSignal` drain，注释明言 "Keeps the two inbound channels on one code path so they never drift"（runner.ts:1839）；`signalToKernelEvent` 被主循环每 turn poll（:1609）与 workflow-batch 抢占监视器（:2359）共享，注释同样写明 "so the two never drift"（runner.ts:3968）。这是契约系统动机的活标本：人工纪律（共享实现）在重构压力下正是会漂移的东西——注册后由 checker 守护。

**关键发现二：会话级关联不变量超出字段形状词表**。`deliver_signal` 提交后 kernel 必须返回**恰好一条**匹配 `(delivery_id, attempt)` 的 `signal_delivery_disposed`（0 或 >1 直接抛错，runner.ts:1877-1881）；ack 前租约丢失同样抛错；任何失败走 nack 释放租约重投递。这类"恰一回执"关联是协议层不变量，不是任何单字段的 preserves/renames——协议模型（或全局规则）需要新增**关联不变量**表达，与 EffectId 铸造禁令同级。

**关键发现三：urgency 处置阶梯是 kernel 所有权**。`normal`→下 turn 边界排队（渲染一次 `[SIGNAL] <text>` 进 volatile state turn）、`high`→软中断、`critical`→抢占（injectNote docstring）；实际路由走 kernel attention policy——`signal_delivery_disposed` 注释即 "the correlated routing decision"（kernel-step.ts:171）。另有一个 derived 怪癖应显式登记：`summary` 由 `payload.goal` 派生（`String(payload.goal ?? "signal")`），goal 字段兼任摘要。

```
 外部世界                    host（入队权）                  kernel（处置权）
┌─────────┐ ingest/broadcast ┌────────────────────┐
│ webhook │ ───────────────▶ │ SignalGateway FIFO  │─┐
│ cron    │ schedule()       │ + lease 30s         │ │ claimSignal(session)
└─────────┘ ───────────────▶ │ (runtime-internal)  │ │
                             └────────────────────┘ ▼
                               injectNote()   ┌────────────────────┐
 PUBLIC ─────────────────────▶│ nextInboundSignal   │ 统一 drain
 （S4 push）                   │ （两通道防漂移汇合） │
                               └─────────┬──────────┘
                                         │ S1 signalToKernelEvent
                                         │   renames×6 + derived×4
                                         ▼
                            kernel 输入事件 deliver_signal
                                         │
                             kernel attention policy（urgency 阶梯）
                             normal→下边界排队 / high→软中断 / critical→抢占
                                         │
                             ▼ 恰一回执 signal_delivery_disposed
                            （delivery_id, attempt）关联
                                         │
                             host ack（租约确认）/ 失败 nack（重投递）
```

### 8.7 对路线与模型的增量修订

1. 结论新增 6–9；P4 行修正（⑩⑪ 热路径，原"视消费方决定"作废）；路线表新增 P6 子系统族（含 `signal.deliver`）。
2. 协议模型 `derived` 语义扩展：支持 crossing-time derivation（M3 盖章：值由 crossing 位置决定）与单位换算（CT3：ratio×10⁶→ppm）。
3. 多 adapter 从 P3 附带说明升格为机制级前置任务（三个真实现场：⑦、CT2、EV1）。
4. M4 直写旁路与 workflow target 绑定缺陷回链三点一.4，随 P2/审计修复顺序收编后再注册对应协议。
5. 关联不变量（S2 恰一回执）列为协议/全局规则模型的新表达需求，与 EffectId 铸造禁令同级。
