# 边界协议与语言边界分析（v0.2.74）

 companion to [type-driven-boundary-contracts-plan.md](type-driven-boundary-contracts-plan.md).
 本文基于 v0.2.73（`70166972`）代码现场盘点，作为协议注册表的依据。结论先行：

1. **语言边界只有三条**：public|host（lower）、host|kernel（project/decode）、host|provider（normalize/settle/plan）。runtime-internal 不是语言边界。
2. **真实的跨层函数共 16 处**（下表），其中 4 处已强类型化，12 处返回 `Record<string, unknown>`。
3. **provider 线格式不是契约面**——encode/decode 是 adapter-local 的 vendor 特化（刻意设计）；契约落在类型化的 plan/normalize/settle 层。
4. **废弃分支发明的 `SkillSource` 四阶阶梯（SkillDeclaration→SkillRef→SkillRevision→SkillPackage）在 v0.2.73 不存在**，不予注册。协议只覆盖真实存在的 crossing。
5. **Boundary A 存在双降级分叉**：类型化的 `lowerAgent → AgentSpec → projectAgent*` 在运行时无消费方（仅 conformance），真实 run 路径手写内联降级且两链覆盖面已分叉。P2 必须先收敛再注册，否则契约守护旁路。

## 一、四层语言 → 实际代码映射

| 层 | 权威 | 实际落点（node SDK） |
|---|---|---|
| public | public-agent | `Agent`/`AgentDefinition`（agent-facade.ts）、`Skill`（skill.ts）、`Tool` 公共面、`Eval`/`Dataset`/`Evaluator`（evals） |
| host | host-runtime | `AgentSpec`/`AgentLoweringInputs`（agent-ir.ts）、`RenderedContext`、`SkillMetadata`（skills/loader.ts）、`ProviderAttempt`/`ModelInvocation`/`ModelUsageSettlement`（execution-evidence.ts）、`NormalizedProviderUsage`/`ProviderRequestPlan`（providers/request-plan.ts） |
| kernel | kernel | Rust canonical ABI——TS 侧**按设计**是无类型 wire（`Record<string, unknown>`）；`KernelObservation`、`EntropySample` 是反向观察面 |
| provider | provider | vendor wire（adapter-local、opaque）；`LLMProvider.complete` 是膜本身 |

关键不对称：TS↔kernel 边界故意无类型（与 Rust ABI 解耦）。0.2.74 的转向机制正是给 kernel 面投影补**宿主侧命名目标类型**（`KernelSkillMetadata` 模式），让契约可被类型驱动，而不动 ABI。

## 二、真实 crossing 清单

### Boundary A：public → host（verb: lower）

| # | crossing | 函数 | 落点 | 类型化 |
|---|---|---|---|---|
| 1 | `Agent → AgentSpec` | `lowerAgent` | agent-ir.ts:168 | ✅ 已强类型 |

唯一入口；`createAgent` 只是句柄包装，真正的语义降级全在 `lowerAgent` + 5 个 `projectAgent*` 投影（run/context/capabilities/governance/delegation）。

**⚠️ 现场警示（双降级分叉）**：`lowerAgent`/`projectAgent*` 在运行时路径上**没有消费方**——只有 conformance 与 advanced/runtime 公共再导出引用它们。真实 run 路径（`AgentRuntimeImpl`/`AgentSessionImpl`）直接读 `this.definition`（30 处），在构造 `RuntimeOptions` 时**手写内联降级**：`instructions+outputSchema→systemPrompt`、`skills→skillCatalog`、`knowledge→knowledgeSource`、`guardrails→governancePolicy`、`handoffs→delegate()` 内联消费、`memoryStore/memoryScope/maxTokens/capabilityFilter` 直通。两条降级链覆盖面已经分叉（`projectAgent*` 不管 memory/maxTokens；内联链不用 AgentSpec），正是契约系统要防的静默漂移。**P2 注册前必须先收敛**（见路线修订）。

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
| 10 | journal raw → `ModelMessage` | `kernelMessageToSdk` | kernel-step.ts:433 | Record→typed |
| 11 | journal raw → `RenderedContext` | `renderedContextToSdk` | kernel-step.ts:505 | Record→typed |

### Boundary C：host ↔ provider（verbs: normalize / settle / plan）

| # | crossing | 函数 | 落点 | 类型化 |
|---|---|---|---|---|
| 12 | `ProviderUsage → NormalizedProviderUsage` | `normalizeProviderUsage` | request-plan.ts:248 | ✅ |
| 13 | `NormalizedProviderUsage → ModelUsageSettlement` | `UsageAccountingPolicy.settle` | execution-evidence.ts | ✅ |
| 14 | `RenderedContext + tools → ProviderRequestPlan` | `createProviderRequestPlan` | request-plan.ts:66 | ✅ |
| 15 | vendor wire → `UsageEvent`/`ProviderUsage` | 各 adapter 内部（Template Method 钩子） | anthropic.ts / openai-chat.ts / … | **契约面外**（见边界判定） |
| 16 | skill frontmatter → `SkillMetadata` | `scanSkillDir`/`parseSkillFrontmatter` | skills/loader.ts:60 | 同权威转换，非语言 crossing |

## 三、语言边界判定

**Boundary A（public|host）**：声明与执行的分界。public 语言里唯一可执行的执行体是 `Agent`；`lowerAgent` 是唯一 crossing。权威从 public-agent 移交 host-runtime，此后 public 词不再出现在执行面（旧 checker 的"public 源禁 import kernel/runtime"规则属于此边界，应保留为 checker 全局规则）。

**Boundary B（host|kernel）**：syscall 膜。两个方向语义不同：
- project（host→kernel）：数据下沉为 kernel 事实。**身份铸造规则属于此边界**——`EffectId` 只能由 kernel 投影铸造（废弃分支 identity.json 的这条政策值得保留为 checker 规则，而非逐函数 JSON）。
- decode（kernel→host）：观察面重建。投影是只读的，不得反铸身份。

**Boundary C（host|provider）**：vendor 膜。**这是本次分析最重要的判定**：线格式的 encode/decode 是 adapter-local 且 vendor 特化（各家钩子、方言、能力差异是特性不是债务）。契约不落在 wire 上，落在三个类型化语义点上：plan（请求前测量）、normalize（vendor→受控词表）、settle（测量→结算）。抽象这三者即可覆盖 provider 膜的语义；逐 vendor 函数不注册协议。

**Runtime-internal 不是语言边界**：`SkillMetadata` 的 frontmatter 解析是同一权威内的 materialize，数据没有跨权威移动。废弃分支为它发明了四阶阶梯和三个 crossing JSON——那是"为契约造架构"的偏差根源，不重犯。

## 四、协议注册表路线

机制已由协议 #1（skill.host-to-kernel）证明。扩展顺序按**类型化成本**递增：

| 批次 | 协议 | 前置工作 |
|---|---|---|
| ✅ P1 | `skill.host-to-kernel` | 无（试点） |
| P2 | `agent.public-to-host` | **先收敛双降级**：把 facade 内联降级提取为唯一命名函数并让 run 路径真实消费它（或让 `lowerAgent` 成为唯一实现），再注册契约——否则契约守护的是旁路 |
| P3 | kernel 投影族：`message` / `tool-schema` / `tool-result` / `task-update` | 每个需先补 `Kernel*` 命名目标类型（复制 `KernelSkillMetadata` 模式）；capability 族作为一个协议、五个 adapter，需先扩展机制支持多 adapter |
| P4 | kernel 观察面：`entropy` decode；`kernelMessageToSdk`/`renderedContextToSdk` 视实际消费方决定 | 确认消费路径 |
| P5 | provider 语义点：`usage-normalize` / `usage-settle` / `request-plan` | 已类型化；settle 的 forbidden（pricing_authority）直接沿用旧裁决 |

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
│  render → RenderedContext ──→ createProviderRequestPlan ⑭   │
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
                    = 恢复/重放路径的 decode）
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

**P2 前置收敛的两个方向**（待拍板）：

1. **提取内联链**：把 facade 内联降级提取为唯一命名函数并让 run 路径真实消费（或让 `lowerAgent` 收编它）——改动集中在 agent-facade，中等工作量。
2. **先注册 B/C 边界**：P3（kernel 投影族）与 P5（provider 语义点）不受分叉影响，先推进，Boundary A 收敛单独立项。

倾向方向 1：契约系统的价值在守护真实路径，绕开最关键的 A 边界会让体系缺一角。
