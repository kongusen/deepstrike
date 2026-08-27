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

- 状态：In Progress（manifest golden 已完成，action golden 待补）；优先级：P0 Guardrail；依赖：063-00。
- RED：把 `multi_effect_step.json`、terminal、empty、unknown、各主要 effect kind 转成 `planned_step → expected_current_projection` fixtures。
- GREEN：先由现有实现生成候选结果，再人工审查并固定 expected JSON。
- REFACTOR：去除 SDK 私有字段，fixture 只描述 canonical 语义。
- 验收：fixture 覆盖多 effect 首 effect、publication order、terminal、unsupported 和 manifest。
- 验证：fixture schema lint；候选输出与人工批准输出逐条比较。
- 实现记录（2026-08-28）：已用 `multi_effect_step.json` 固定多 effect manifest 顺序，并新增 terminal 空 manifest 测试；current-action golden 将在下一切片随 typed projection 补齐。

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

- 状态：Todo；优先级：P0 Binding；依赖：063-03。
- RED：Rust conformance 先比较旧 HostAction 与 core 输出，记录所有差异。
- GREEN：Rust `HostAction` 改为 core 类型别名/re-export，runtime 只消费 `CurrentProjection`。
- REFACTOR：删除 `protocol_action_from_wire` 和重复 switch。
- 验收：Rust 行为与 core golden byte-level 一致；宿主 I/O 路径不变。
- 验证：`cargo test --workspace`。

### Card 063-05：Node/WASM binding 换接

- 状态：Todo；优先级：P0 Binding；依赖：063-04。
- RED：Node/WASM binding conformance 对旧投影和 core golden 做差异矩阵。
- GREEN：新增 `projectHostActionJson(plannedStepJson)`，TS/WASM 只做解析和类型映射。
- REFACTOR：删除 `canonicalActionFromPlannedStep` 中的协议 switch 与首 effect逻辑。
- 验收：Node/WASM 输出与 core golden 一致；938 类现有投影消费路径保持执行结果一致。
- 验证：Node focused tests/build；WASM conformance。

### Card 063-06：Python binding 换接

- 状态：Todo；优先级：P0 Binding；依赖：063-04。
- RED：Python fixture 断言旧 dict 投影与 canonical JSON 存在已知差异。
- GREEN：Python 使用 core projection binding，`json.loads` 后只做公共字段映射。
- REFACTOR：删除 `_context_from_kernel` 等已下沉的 action 投影辅助。
- 验收：Python 与 Node/Rust/WASM 对同一 fixture 输出等价；unknown/unsupported 语义一致。
- 验证：Python focused tests + parity tests。

### Card 063-07：宿主 manifest 与 journal 接线

- 状态：Todo；优先级：P0 Runtime；依赖：063-03、063-04。
- RED：证明各 SDK 手工提取 manifest 会在 map 顺序和多 effect 下漂移。
- GREEN：宿主只调用 core manifest，再把结果写入既有 observation/event log。
- REFACTOR：保持 Kernel 不生成该 observation，确保旧正确 journal digest 不变。
- 验收：事件日志含完整 effect id/kind；digest/replay 输入未意外增加字段。
- 验证：Rust/Node/Python journal replay tests。

### Card 063-08：RequestPlan 与协议身份护栏

- 状态：Todo；优先级：P1；依赖：063-07、SPC-024。
- RED：同一请求在 create/count/replay 三条路径序列化不一致，或切换 endpoint 后错误复用 measurement。
- GREEN：统一 `ProviderRequestPlan`、protocol identity 和 fingerprint；session 恢复校验 provider/protocol/endpoint/model。
- REFACTOR：删除 adapter 内第二套 request body 拼装。
- 验收：count/create/replay 使用同一语义 plan；协议不匹配在网络 dispatch 前失败。
- 验证：provider request-plan、replay mismatch、measurement durability tests。

### Card 063-09：Capability evidence 收口

- 状态：Todo；优先级：P1；依赖：063-08。
- RED：registry 宣称 supported 但 adapter 无可调用方法时测试失败。
- GREEN：effective capability 由 endpoint evidence、SDK adapter 和 method availability 共同决定。
- REFACTOR：provider catalog、factory、runtime registry 共用 resolver。
- 验收：supported 一定可执行；未知保持 unknown；自定义 endpoint 不继承官方能力。
- 验证：Node/Python registry parity tests。

### Card 063-10：删除旧实现与发布门禁

- 状态：Todo；优先级：P0 Release；依赖：063-04..09。
- RED：静态 parity guard 仍能找到旧 switch、重复 manifest 提取或旧单 effect断言。
- GREEN：删除旧函数、旧 fixtures 和已错误的兼容测试；保留迁移说明。
- REFACTOR：更新 changelog、migration 和架构图，补充失败诊断文档。
- 验收：仓库中只有一份协议投影实现；全量测试、构建、conformance、docs drift、parity guard 全绿。
- 验证：`cargo test --workspace`；Node build/test；Python pytest；`npm run test:conformance`；`npm run docs:drift`；SDK parity guard。

## 7. 执行顺序与检查点

```text
063-00
  ↓
063-01 → 063-02 → 063-03
                    ├→ 063-04 → 063-05 → 063-06
                    └→ 063-07
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
