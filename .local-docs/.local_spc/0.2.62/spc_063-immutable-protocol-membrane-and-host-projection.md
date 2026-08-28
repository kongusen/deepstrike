# SPC-063 - Immutable Protocol Membrane 与统一 Host Projection

## 文档状态

- 状态：Proposed
- 日期：2026-08-28
- 基线：DeepStrike `0.2.61`（包含当前 `Unreleased` multi-effect 修复的工作树）
- 目标版本：DeepStrike `0.2.62`
- 优先级：P0 架构正确性
- 任务标记：`P0-0.2.62-IMMUTABLE-PROTOCOL-MEMBRANE`
- 前置：SPC-024，以及 `Unreleased` 中的 multi-effect、pending publication order、journal diagnosability 改动

## 1. 决策摘要

本版本直接重构，不维护错误旧实现的兼容路径。兼容边界只保留在仍然有效的协议事实：模型/provider wire protocol、正确 journal 的确定性 replay、effect identity、publication order 和 digest 语义。

框架采用四层边界：

```text
Provider/Model protocol（外部不可变事实）
        ↓ adapter + RequestPlan + ResponseNormalizer
Kernel Wire（PlannedStep / Effect / Envelope）
        ↓ deepstrike-core 纯投影
Canonical Execution IR（当前要执行的 action）
        ↓ binding
SDK host runtime（I/O、journal、权限、生命周期）
```

核心原则：协议只解释一次，宿主只执行一次；SDK 不再各自实现协议判断。

## 2. 问题与目标

### 2.1 当前问题

Node、WASM、Python、Rust 各自复制了 planned-step 到 host-action 的投影逻辑。本轮 multi-effect 修复再次证明以下规则容易漂移：

- terminal 判断
- pending effect 的 publication order
- 多 effect 时取首 effect
- unknown effect 的错误/unsupported 语义
- `step_published_effects` manifest 提取

0.2.61 同时证明 provider routing、usage、capability 和 replay 都需要把协议身份与证据作为持久事实，不能依赖 SDK 内的隐式推断。

### 2.2 目标

- core 维护唯一的 canonical execution semantics。
- 四个 SDK 只保留绑定、I/O 和宿主状态机。
- 不改变模型/provider wire protocol。
- 正确历史 journal 仍可确定性 replay；已知错误的旧 multi-effect journal 不提供迁移保证。
- 任何新增 effect kind 只修改 core、fixture 和绑定生成/解析层。
- 通过 TDD 卡片逐步落地，每张卡都能独立验证和回滚。

### 2.3 非目标

- 不把宿主 I/O、网络、时钟、随机数或 journal 写入下沉到 core。
- 不在本版本重写 `CanonicalRunnerRuntime` 状态机。
- 不把 provider-specific 字段塞进模型协议。
- 不为错误的旧 HostAction 形状增加长期兼容层。
- 不在没有官方证据和 adapter 实现时宣称新的 provider capability。

## 3. 不可变性边界

### 3.1 必须保持不变

- provider/model 请求与响应的 wire shape。
- Kernel Wire 的字段含义、effect ID、digest 输入和 publication order。
- 同一 wire 输入的 replay 结果。
- session 使用过的 provider、protocol、endpoint identity 和 model identity。

### 3.2 允许直接不兼容

- SDK 内部的 `HostAction` 类型名称和字段访问方式。
- 各 SDK 的旧投影函数。
- 只支持单 effect 的调用约定。
- 已经无法正确恢复的错误 journal。
- 未承诺为公共契约的偶然错误消息和字段顺序。

## 4. 目标接口

### 4.1 Core canonical action

core 新增 `runtime::projection` 模块，定义稳定语义类型。canonical JSON 使用固定 snake_case；语言绑定可在最外层映射为本语言命名风格。

```rust
pub enum CanonicalHostAction {
    CallProvider {
        effect_id: EffectId,
        context: ProjectedContext,
        tools: Vec<ProjectedTool>,
    },
    ExecuteTool {
        effect_id: EffectId,
        calls: Vec<ProjectedToolCall>,
    },
    RequestApproval {
        effect_id: EffectId,
        requests: Vec<ProjectedApproval>,
    },
    SpawnWorkflow {
        effect_id: EffectId,
        nodes: Vec<ProjectedNode>,
        budget: Option<BudgetGrant>,
    },
    UnsupportedEffect {
        effect_id: EffectId,
        effect_kind: String,
    },
}
```

具体 effect 以现有 wire contract 为准，新增 kind 必须同时补 core fixture 和 conformance fixture。

### 4.2 Current projection

不使用 `Option<Action>` 混淆 idle、terminal 和非法输入：

```rust
pub enum CurrentProjection {
    Idle,
    Action(CanonicalHostAction),
    Terminal(CanonicalTerminal),
}

pub fn project_current_action(
    planned_step: &PlannedStep,
) -> Result<CurrentProjection, ProjectionError>;
```

该入口统一完成 terminal 检查、publication-order 排序、取当前 effect 和 effect 投影。

### 4.3 Published manifest

```rust
pub fn published_effects_manifest(
    planned_step: &PlannedStep,
) -> Vec<PublishedEffectRef>;
```

manifest 是宿主写入事件日志的纯事实。Kernel 不主动生成 observation，避免改变既有 digest/replay 输入。

### 4.4 错误语义

- 结构损坏、缺失必需字段、非法排序信息：`ProjectionError`。
- 已知结构但宿主暂不支持的 effect：`UnsupportedEffect` action。
- 未知 observation 字段：保留为 opaque fact，不擅自解释。
- provider usage 缺失：`unavailable`，不伪造为零。

## 5. 模块职责

| 层 | 负责 | 禁止 |
|---|---|---|
| Provider adapter | protocol 路由、请求/响应转换、usage normalizer | 修改 Kernel Wire 语义 |
| RequestPlan | create/count/replay 共用序列化与 fingerprint | 为 count 再维护一套 body builder |
| deepstrike-core projection | 排序、选择、语义归一化、manifest | 网络、文件、时钟、随机数 |
| SDK binding | JSON/type bridge、错误映射、字段命名 | 重写 effect switch |
| Host runtime | provider dispatch、tool 执行、journal、权限、生命周期 | 自行推断当前 action |

## 6. TDD 卡片

每张卡严格执行 `RED → GREEN → REFACTOR`。RED 必须先证明旧实现或缺失契约确实失败；GREEN 只写使测试通过的最小实现；REFACTOR 后不得改变 golden 输出。

### Card 063-00：冻结重构契约

- 状态：Done（契约首片已冻结）；优先级：P0 Contract；依赖：无。
- RED：新增类型/fixture schema 测试，确认 `CurrentProjection`、`CanonicalHostAction`、`PublishedEffectRef` 的必需字段和 unknown 语义。
- GREEN：写入本 SPC 对应的 core 类型草案与 JSON schema/serde 约束。
- REFACTOR：确认 canonical JSON 使用 snake_case，语言命名映射只在 binding 层。
- 验收：任何实现者只读本卡和接口定义即可开始后续卡片；无未决的字段语义。
- 验证：core unit tests；schema round-trip tests。
- 实现记录（2026-08-28）：新增 `runtime::kernel::wire::projection` 模块，确定 projection 只读 wire、无 I/O/时钟/随机数；canonical manifest 使用 `effect_id + kind`，canonical JSON 保持 snake_case。

### Card 063-01：建立 host-action golden

- 状态：Done（首版 action serialization golden 已完成）；优先级：P0 Guardrail；依赖：063-00。
- RED：把 `multi_effect_step.json`、terminal、empty、unknown、各主要 effect kind 转成 `planned_step → expected_current_projection` fixtures。
- GREEN：先由现有实现生成候选结果，再人工审查并固定 expected JSON。
- REFACTOR：去除 SDK 私有字段，fixture 只描述 canonical 语义。
- 验收：fixture 覆盖多 effect 首 effect、publication order、terminal、unsupported 和 manifest。
- 验证：fixture schema lint；候选输出与人工批准输出逐条比较。
- 实现记录（2026-08-28）：已用 `multi_effect_step.json` 固定多 effect manifest 顺序、terminal 空 manifest，以及 canonical action 的 kind/effect identity/payload 序列化字段；core projection tests 5 passed。

### Card 063-02：实现 core effect projection

- 状态：Done（首版 typed projection 已完成）；优先级：P0 Core；依赖：063-01。
- RED：core 对每个 fixture 投影失败或输出不一致。
- GREEN：实现 `project_effect` 与 `CanonicalHostAction`，不接入 SDK。
- REFACTOR：抽取 context、message、usage、termination、tier 等纯归一化辅助函数。
- 验收：core golden 全绿；无 I/O/时间/随机依赖。
- 验证：`cargo test -p deepstrike-core projection`。
- 实现记录（2026-08-28）：新增 `CanonicalHostAction`，覆盖 11 个现有 effect kind；`project_effect()` 只复用 wire payload 类型，不引入 provider/SDK I/O。core focused tests 4 passed。

### Card 063-03：实现 current projection 与 manifest

- 状态：Done（首版 transaction 接线已完成）；优先级：P0 Core；依赖：063-02。
- RED：多 effect、step 编号 `9/10`、terminal 和空 effect 场景证明旧逻辑不满足统一规则。
- GREEN：实现 `project_current_action`、`published_effects_manifest`。
- REFACTOR：让排序规则只存在于 core 一个位置。
- 验收：首 effect 始终按 publication order；terminal 不被误报为 idle；manifest 与实际 effect 集一致。
- 验证：core property tests + golden。
- 实现记录（2026-08-28）：新增显式 `CurrentProjection::{Idle, Action, Terminal}` 与 `project_current_action()`；`CanonicalKernel::current_projection()` 直接消费 transaction 的 `pending_effects_in_order()`，排序仍由 transaction 单点拥有。core focused tests 4 passed。

### Card 063-04：Rust 生产运行时换接

- 状态：Done；优先级：P0 Binding；依赖：063-03。
- RED：Rust conformance 先比较旧 HostAction 与 core 输出，记录所有差异。
- GREEN：Rust `HostAction` 改为 core 类型别名/re-export，runtime 只消费 `CurrentProjection`。
- REFACTOR：删除 `protocol_action_from_wire` 和重复 switch。
- 验收：Rust 行为与 core golden byte-level 一致；宿主 I/O 路径不变。
- 验证：`cargo test --workspace`。
- 实现记录（2026-08-28）：Rust `action_from_core_step()` 已统一消费 core `CurrentProjection`，terminal/idle/首 effect 选择不再由 Rust 自行判断；全部可执行 effect 分支（含 `ArchivePageOut`）已改为消费 core `CanonicalHostAction` 后再映射到 Rust DTO，characterization test 通过。保留的 `MeasurePrompt` 分支仍按当前 reserved 语义拒绝。当前环境的 Git index 写入受限，新增代码暂以工作树形式保留。

### Card 063-05：Node/WASM binding 换接

- 状态：Done；优先级：P0 Binding；依赖：063-04。
- RED：Node/WASM binding conformance 对旧投影和 core golden 做差异矩阵。
- GREEN：新增 `projectHostActionJson(plannedStepJson)`，TS/WASM 只做解析和类型映射。
- REFACTOR：删除 `canonicalActionFromPlannedStep` 中的协议 switch 与首 effect逻辑。
- 验收：Node/WASM 输出与 core golden 一致；938 类现有投影消费路径保持执行结果一致。
- 验证：Node focused tests/build；WASM conformance。
- 实现记录（2026-08-28）：Node/WASM binding 新增并强制使用 `currentProjectionJson()` 与 `projectPlannedStepJson()`；运行时删除旧 planned-step fallback，仅保留 projection JSON 到宿主 DTO 的适配。共享 fixture `tests/fixtures/abi/current_projection_multi_effect.json` 已建立；Node/WASM conformance selector tests 均通过，WASM build 与 171 项全量测试通过。

### Card 063-06：Python binding 换接

- 状态：Done；优先级：P0 Binding；依赖：063-04。
- RED：Python fixture 断言旧 dict 投影与 canonical JSON 存在已知差异。
- GREEN：Python 使用 core projection binding，`json.loads` 后只做公共字段映射。
- REFACTOR：删除 `_context_from_kernel` 等已下沉的 action 投影辅助。
- 验收：Python 与 Node/Rust/WASM 对同一 fixture 输出等价；unknown/unsupported 语义一致。
- 验证：Python focused tests + parity tests。
- 实现记录（2026-08-28）：Python binding 新增并强制使用 `current_projection_json()` 与 `project_planned_step_json()`；运行时删除旧 dict planned-step fallback，仅保留 projection JSON 到宿主 DTO 的适配。Python conformance selector 与 focused tests 均通过（12 passed）。

### Card 063-06R：Archive presentation 回归隔离（方案修订）

- 状态：Done；优先级：P0 Regression；依赖：063-05、063-06。
- 背景：`archive_page_out` 的 `action`、`summary`、`tier` 来自同一 `planned_step` 的 `compressed` observation，不属于 effect payload。切换到 core projection 后，`currentProjectionJson()` 只携带单 effect payload，旧 runtime 若继续从 action DTO 读取这些字段就会丢失语义页出路径。
- 架构决策：不把 observations 回填到 core projection；不在四个 SDK 中复制一套 observation-aware projection。core `CanonicalHostAction::ArchivePageOut` 保持 wire payload-only。
- RED：新增 Node/Python/WASM/Rust characterization test，构造带 `archive_page_out + compressed observation` 的 step，证明 projection action 不包含 `action`、`summary`、`tier`，同时证明 runtime 仍必须从 observation 流恢复这些事实。
- GREEN：runtime 在提交 transition 时，将与 page-out effect 关联的 observation 事实放入既有 `pendingObservations`/host observation 流，按 `effect_id` 关联；archive 执行路径只从该事实读取 `action`、`summary`、`tier`。`tier=semantic` 时继续触发长期记忆归档。
- REFACTOR：删除四语言 archive DTO 中由 observation 派生的 presentation 字段；保留 effect payload 的 `handle_id`、`payload`、digest 等 wire 事实；禁止 runtime 从 projection action 反推 pressure policy。
- 验收：
  - projection JSON 的 archive action 只包含 canonical wire payload。
  - semantic page-out 仍调用 `archiveSemanticPageOut`，durable page-out 不误写长期记忆。
  - archive observation 与 effect ID 一一对应，多 effect 和 publication order 下不串线。
  - 未知/缺失 observation 不伪造 tier，按既有安全默认处理。
- 验证：Node/Python/WASM/Rust archive characterization；`renewal-memory-requery`；`semantic-page-out-memory` 独立与受控串行运行；journal replay/restore 测试。
- 实现记录（2026-08-28）：Node/WASM/Python archive 执行路径统一从 `compressed` observation 关联的 pending archive metadata 读取 action/tier，不再读取 projection action；archive action DTO 删除 observation 派生字段。Rust 保持 observation-aware runtime 映射。Node `semantic-page-out-memory` 与 `renewal-memory-requery` 均通过，WASM build 与 projection/canonical focused tests 通过，Python focused/conformance 12 passed。

### Card 063-06S：Renewal 记忆链路实证

- 状态：Done；优先级：P0 Regression；依赖：063-06R。
- 目标：把“tier 缺失导致语义页出断裂”与 renewal 后 recall 是否进入 turns 的因果链拆开验证，不把 K4 的失败直接归因于单一原因。
- RED：分别关闭 semantic page-out、关闭 renewal re-query、固定 `memoryStore.search` 返回值，记录 `seed keyed entry → turns/knowledge → renewal recall` 的状态差异。
- GREEN：补齐 runtime observation 恢复后，验证 page-out 归档、长期记忆写入、renewal 查询、turn 渲染四个阶段的事件序列。
- REFACTOR：将测试断言从“最终看到 recall”拆为可定位的事件断言，避免 fixture 恒返 RECALL 掩盖中间链路。
- 验收：能明确区分 projection 回归、semantic archive 回归和 turns/knowledge 分类回归；任何失败都能定位到具体事件边界。
- 验证：K4 renewal test、semantic page-out test、受控单测及 replay test。
- 实现记录（2026-08-28）：renewal 测试增加事件级断言，明确 `compressed → context_renewed` 顺序，同时保留 renewal re-query 与 recall 落入新 sprint turns 的断言；测试通过。

### Card 063-07：宿主 manifest 与 journal 接线

- 状态：In Progress（四端 manifest 接线完成，Node native replay/ordering 覆盖完成）；优先级：P0 Runtime；依赖：063-03、063-04、063-06R。
- RED：证明各 SDK 手工提取 manifest 会在 map 顺序和多 effect 下漂移。
- GREEN：宿主只调用 core manifest，再把结果写入既有 observation/event log。
- REFACTOR：保持 Kernel 不生成该 observation，确保旧正确 journal digest 不变。
- 验收：事件日志含完整 effect id/kind；digest/replay 输入未意外增加字段。
- 验证：Rust/Node/Python journal replay tests。
- 实现记录（2026-08-28）：新增 `published_effects_manifest_json` core/binding 接口，Node/WASM/Python 以及 Rust runtime 的 `step_published_effects` observation 均改为直接消费 core manifest；Node native binding 已补充 restore/replay 后的 publication-order manifest 测试，各端 focused/build 测试通过。

### Card 063-08：RequestPlan 与协议身份护栏

- 状态：Todo；优先级：P1；依赖：063-07、SPC-024。
- RED：同一请求在 create/count/replay 三条路径序列化不一致，或切换 endpoint 后错误复用 measurement。
- GREEN：统一 `ProviderRequestPlan`、protocol identity 和 fingerprint；session 恢复校验 provider/protocol/endpoint/model。
- REFACTOR：删除 adapter 内第二套 request body 拼装。
- 验收：count/create/replay 使用同一语义 plan；协议不匹配在网络 dispatch 前失败。
- 验证：provider request-plan、replay mismatch、measurement durability tests。

### Card 063-09：Capability evidence 收口

- 状态：Done；优先级：P1；依赖：063-08。
- RED：registry 宣称 supported 但 adapter 无可调用方法时测试失败。
- GREEN：effective capability 由 endpoint evidence、SDK adapter 和 method availability 共同决定。
- REFACTOR：provider catalog、factory、runtime registry 共用 resolver。
- 验收：supported 一定可执行；未知保持 unknown；自定义 endpoint 不继承官方能力。
- 验证：Node/Python registry parity tests。
- 实现记录（2026-08-28）：补充 registry/adapter parity 门禁。native token counting 的 `supported` 由 endpoint evidence（静态表 anthropic.messages/gemini.google/openai.responses）+ adapter evidence 共同决定；parity 测试反向强制「凡 supported，adapter 必暴露 `countTokens`」，保证 supported 一定可执行。model-registry / token-measurement / provider-runtime parity tests 实测 Node 63 + Python 60 全绿。

### Card 063-10：删除旧实现与发布门禁

- 状态：In Progress；优先级：P0 Release；依赖：063-04..09。
- RED：静态 parity guard 仍能找到旧 switch、重复 manifest 提取或旧单 effect断言。
- GREEN：删除旧函数、旧 fixtures 和已错误的兼容测试；保留迁移说明。
- REFACTOR：更新 changelog、migration 和架构图，补充失败诊断文档。
- 验收：仓库中只有一份协议投影实现；全量测试、构建、conformance、docs drift、parity guard 全绿。
- 验证：`cargo test --workspace`；Node build/test；Python pytest；`npm run test:conformance`；`npm run docs:drift`；SDK parity guard。
- 实现记录（2026-08-28）：静态扫描已确认仓库不再存在旧 `canonicalActionFromPlannedStep`/`canonical_action_from_planned_step` 入口；`docs:drift` 通过（141/141 paths、222/222 symbols、47/47 zh/en parity）。`test:conformance` 当前被本机缺少 `wasm32-unknown-unknown` target 阻塞，非代码失败。

## 7. 执行顺序与检查点

## 8. 方案修订结论：Projection 与 Page Strategy

### 8.1 是否解决本次回归

该方案解决了已确认的架构错误：archive 的 `action`、`summary`、`tier` 不再被误认为 effect payload，projection 不再承担 observation 上下文职责。它能消除“切换到 core projection 后语义页出丢失”的直接回归。

但在 `063-06R/063-06S` 完成并通过 replay、renewal 和 semantic page-out 测试前，不能声称 K4 的整条记忆链已经闭合。当前仍需实证 `page-out → 长期记忆写入 → renewal re-query → turns/knowledge 分类` 的事件因果链。

### 8.2 Projection 不做整体推倒，改为三种明确投影

保留现有 core projection 的主体设计，并明确三种不同用途：

1. **EffectProjection**：wire effect → `CanonicalHostAction`。只含 effect payload、effect identity 和 causation identity。
2. **CurrentProjection**：从已排序 pending effects 选择当前可执行 action，或返回 idle/terminal。只回答“现在宿主该执行什么”。
3. **Observation/Facts stream**：承载 `compressed`、`page_out_archived`、`renewed` 等已提交事实。只回答“内核刚刚证明了什么”。

禁止把第 3 类事实拼回第 1、2 类 action DTO。SDK binding 只做 JSON/DTO 映射，runtime 负责把 observation facts 与 effect ID 关联。

后续可增加静态 parity guard，确保任何 SDK 不再自行完成首 effect 选择、effect 排序、pressure tier 推导或 observation-aware switch。

### 8.3 Page strategy 的目标结构

Page-out 应拆成四个阶段，避免把策略、搬运和展示混在一个 action 中：

```text
Kernel policy decision
  → compressed observation(action/summary/tier)
  → archive_page_out effect(payload/handle/digest)
  → host archive I/O
  → resolve effect + page_out_archived/page_out_archive_failed observation
```

具体规则：

- `compressed` observation 是 pressure policy 的权威来源。
- `archive_page_out` effect 是内容搬运的权威来源。
- `page_out_archived` observation 是归档结果的权威来源。
- semantic 长期记忆写入必须以稳定的 `effect_id` 或 archive correlation id 做幂等键。
- async summarizer 若继续保留，必须有 pending/completed 的可恢复事实；不能让进程退出造成“归档成功但长期记忆静默丢失”。
- archive range、compressed sequence、effect ID 的关联不能只依赖队列位置，必须可在多 effect、重试和 replay 后重建。
- page-in、renewal 和 turns/knowledge 渲染只读取已提交 observation/session event，不读取 action DTO 中的派生字段。

### 8.4 下一阶段卡片顺序

- **063-06R**：先修 archive presentation 回归，删除 action DTO 中的 observation 派生字段。
- **063-06S**：补齐 renewal 因果链的分段测试，确认 K4 的真实断点。
- **063-06P**：建立 page-out correlation/idempotency，覆盖多 effect、重试、restore（Node 已先切入稳定 effect key）。
- **063-06Q**：明确 semantic archive 的同步/异步 durability contract，并补 pending/completed replay 测试（Node 事件闭环已完成）。
- **063-07**：在上述边界稳定后，再接通 manifest 与 journal 的最终收口。

实现记录（2026-08-28）：Node semantic archive memory record name 改为基于稳定 `effect_id`，替代时间戳命名，降低 retry/replay 重复写入风险；完整 archive store 幂等接口和其余 SDK 迁移仍待完成。

### 8.5 进一步的 Projection 优化建议

在不改变模型/provider 协议的前提下，projection 还可以做以下增强：

- **收紧 live API**：运行时长期只依赖 `currentProjectionJson()`；`pendingEffectsJson()` 和 `projectPlannedStepJson()` 仅保留给诊断、迁移和 replay，避免宿主重新实现首 effect 选择。
- **加入 projection contract metadata**：binding 返回的 envelope 可带 projection ABI revision/schema fingerprint，便于旧 native addon、旧 WASM glue 或旧 Python wheel 被及时拒绝，而不是静默产生错误 DTO。
- **统一错误分类**：区分 malformed projection、unsupported effect、stale projection 和 binding ABI mismatch；不要把四类错误都降级成 `unsupported_effect`。
- **建立不变量测试**：同一 ordered pending-effect 集合在四语言中必须得到相同的 state、effect identity、kind 和 payload；projection 不得改变 journal digest，也不得读取时钟、随机数或 host state。
- **限制 action DTO 表面积**：只暴露宿主执行所需字段；summary、tier、usage 展示和策略建议一律走 observation/facts stream。
- **为扩展保留未知语义通道**：当未来 wire 增加 effect/observation kind 时，优先返回结构化 `unknown`/`unsupported`，不要因 SDK 版本落后而误判为 idle 或 terminal。

### 8.6 进一步的 Page Strategy 优化建议

- **策略、搬运、索引三段分离**：pressure decision 只决定压缩范围与 tier；archive effect 只负责内容搬运；session/memory index 只消费已提交归档事实。
- **显式关联与幂等**：page-out 任务必须能用 `effect_id + archive_range` 重建，不能只依赖 `pendingPageOutArchives` 的数组位置；重试、并发多 effect 和 restore 都必须得到同一 archive key。
- **滞回与背压**：进入 page-out 与退出 page-out 使用不同阈值，并设置最小保留尾部、最大单批大小和 provider 调用前的硬预算，避免在阈值附近反复压缩/恢复。
- **语义归档可恢复**：semantic archive 的 memory write 需要 pending/completed/fail 可观察状态；异步任务必须可重放、可去重、可补偿，不能只依赖进程内 task。
- **page-in 只读事实**：renewal/page-in 渲染从 `page_out_archived`、archive reference 和 knowledge events 重建，不从当前 action 猜测历史压缩策略。
- **增加运行指标**：记录压缩前后 token、page-out 延迟、archive retry、semantic write lag、page-in 命中率和 renewal recall 落点，用于区分策略问题与投影问题。

```text
063-00
  ↓
063-01 → 063-02 → 063-03
                    ├→ 063-04 → 063-05 → 063-06 → 063-06R → 063-06S
                    └→ 063-07
                               ↑
                             063-06R
                               ↓
                         063-08 → 063-09
                                      ↓
                                  063-10
```

### Checkpoint A：契约冻结

- canonical action/current projection/manifest 类型已评审。
- golden fixture 已覆盖当前 0.2.61 行为。
- 没有新增 provider 或模型协议字段。

### Checkpoint B：Core 绿

- core golden、property tests 全绿。
- Rust 尚未删除旧实现前，旧实现与 core 输出完成差异审计。

### Checkpoint C：四 SDK 一致

- Rust、Node、WASM、Python conformance 全绿。
- 任意 SDK 不再包含 effect switch、排序或首 effect选择逻辑。

### Checkpoint D：发布门禁

- journal replay、request plan、capability、usage、docs drift 和 parity guard 全绿。
- changelog 明确不兼容范围和不迁移的错误 journal 类型。

## 8. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| core 类型误带 SDK 命名 | 绑定被某语言反向约束 | canonical JSON 固定 snake_case，语言映射只在 binding |
| current projection 丢失 terminal/idle 信息 | 宿主状态机误执行 | 使用显式 `CurrentProjection` 枚举 |
| 删除旧实现后发现未覆盖行为 | 发布回归 | 先做 characterization golden，再删代码 |
| 错误 journal 被误当成可迁移 | 恢复语义不确定 | 明确拒绝迁移，提供 diagnose 结果 |
| provider protocol 与 host action 混层 | 协议不可控地膨胀 | adapter、Kernel Wire、Execution IR 三层隔离 |
| capability 宣称超出实际 SDK | 运行时硬失败 | evidence + adapter method 双重门禁 |
| 一次改动过大 | 难以定位回归 | 每张卡先 RED，checkpoint 后再删旧路径 |

## 9. 发布与回滚策略

- 该重构不以兼容旧错误 HostAction 为目标。
- 本次统一使用目标版本 `0.2.62`；由于包含 HostAction 和投影边界的 breaking change，changelog 必须使用显式 `Breaking` 段落。
- 不保留 feature flag 双轨实现。
- 回滚依赖 Git tag/release artifact，不依赖运行时兼容代码。
- 发布前必须保证同一 `PlannedStep` 在所有绑定中得到等价 canonical action。

## 10. 完成定义

本 SPC 只有在以下条件全部满足时才标记 Done：

1. core 是唯一的 host-action 协议解释器。
2. 四个 SDK 只保留 binding、宿主 I/O 和状态机。
3. multi-effect、terminal、unknown effect、manifest 都有共享 golden。
4. 正确历史 journal replay 结果不变；错误旧 journal 的拒绝行为有测试和文档。
5. provider request plan、protocol identity、usage authority、capability evidence 与 SPC-024 语义一致。
6. 全量构建、测试、conformance、docs drift 和 parity gate 通过。
