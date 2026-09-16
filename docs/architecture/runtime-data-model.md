---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [WireEnvelope, KernelInput, KernelEffect, KernelTerminal, SyscallRequest, SessionEvent]
  python: [RuntimeRunner, KernelJournal]
---

# Canonical Runtime Data Model（运行时数据模型）

本文是 DeepStrike 运行时数据宪法的总纲：五层语义模型、Intent/Fact/Decision 三词闭环、六条工程原则与边界不变量 B1–B9。配套文件：[Authority 矩阵](./runtime-authority)、[持久契约](./runtime-persistence)、[因果与验证](./runtime-causality)。

> 出处：P0–P8 全弧调研（2026-09-15，存档于 `.local-docs/specs/runtime-data-model-p0..p8-*.md`）与迭代总谱（`.local-docs/specs/runtime-roadmap-0.2.63-plus-2026-09-15.md`）。本文为颁布态条文；取证过程与 file:line 证据见存档。

## 五层语义模型

层按**数据语义**划分，不按 package 划分。一个 struct 不得跨两个以上语义层。

```text
L0 · Protocol Evidence   Provider wire 原始字节、ProviderWireEvidence
                         Authority: Provider Adapter
L1 · Semantic State      CanonicalMessageState（现由 StoredMessageState 承载）、DurableContent
                         Authority: Canonical Semantic Model
L2 · Execution Evidence  ModelInvocation / ProviderAttempt / ResolvedProviderRoute /
                         ProviderUsage / UsageAccountingPolicy / TokenMeasurement
                         Authority: Host Execution Runtime
L3 · Kernel Control      KernelInput / KernelEffect / EffectOutcome / Task / Capability / Budget
                         Authority: Kernel
L4 · Durable Truth       Journal = State Truth / Checkpoint = State Snapshot /
                         SessionLog = Evidence Truth
```

## 三词闭环

跨内核边界的一切数据必是三者之一（条文 B8）：

| 词 | 方向 | 语义 |
|---|---|---|
| **Intent** | host → kernel | "我希望这件事发生"（ConfigureOperation / StartOperation / HostControl） |
| **Fact** | host → kernel | "实际发生了这件事"（ResolveEffect 的 outcome / DeliverExternalEvent） |
| **Decision** | kernel → host | "经裁决，允许/要求发生这件事"（KernelEffect / StepDisposition::Terminal） |

归不入三者之一、又不是已登记的 projection/observation 形态的跨界对象，即宪法违规。

**关键裁决：模型的 ToolCall 不是 Intent。** 它作为 Fact 的载荷（ProviderCompleted.message.tool_calls）到达内核；Intent 由内核在 `derive_provider_syscalls` 中还原，三个拒绝门全部 fail-closed：未知 effect / 未曝光 tool / 已消费 call_id。**host 永不声明 caller**（条文 B9）——身份一律由内核从（effect_id, call_id, 已发布表面）推导。

## 六条工程原则（A1–A6）

所有 spec 与 PR 先过这六条：

| 原则 | 含义 |
|---|---|
| **A1 Single Authority** | 同一事实只能有一个 authority |
| **A2 Explicit Representation** | 非 authority 只能是下表六种形态之一 |
| **A3 Explicit Identity** | ID 必须属于 kernel-minted / host-bound / provider-adopted / content-addressed 四类之一（见 [Authority](./runtime-authority)） |
| **A4 Explicit Causality** | 跨层关系必须有 ID 互链，禁止"前后顺序大概对应"（见 [Causality](./runtime-causality)） |
| **A5 Kernel Minimal Knowledge** | Provider/billing/transport 语义不得进 kernel，除非直接影响 control decision |
| **A6 Machine-Enforced Constitution** | 任何架构规则最终必须表现为 fixture / validator / compile-time exhaustive match / CI gate |

> **Deduplicate authority, not representation.**
> 可以为不同 purpose 存在不同 representation，但只能有一个语义 authority，且转换边界必须显式、穷尽、可测试。四个双生类型族（TerminationReason / PaceAction / ToolCall / ResourceQuota）即此原则的实例：wire 版 = ABI 权威，内部版 = 语义词表更富的 projection，唯一合法出入 = driver 的穷尽转换函数。

## 非权威表示的六种合法形态（A2 词表）

任何不是 authority 的数据表示，必须能归入下列之一并在 Authority 矩阵中登记：

| 形态 | 定义 | 允许的操作 |
|---|---|---|
| **reference** | 仅持有权威身份的指针（id/digest/handle） | 解引用、传递 |
| **projection** | 从权威状态派生的只读视图 | 读；重建 |
| **cache** | 权威值的暂存副本 | 读；失效重建；**必须可凭 fingerprint/key 自证有效** |
| **measurement** | 对权威对象的观测，带 provenance | 读；重新测量 |
| **evidence** | 发生过什么的原始记录，不再改变 | 读；归档 |
| **mirror** | ABI 边界的序列化投影（SDK 侧） | 编解码；**不得引入本侧独有语义** |

**违规四分**：双权威（两处都可写）/ 无权威（事实存在但无处认领）/ 权威错层（低层数据承担高层语义）/ 隐式权威（靠时序约定而非字段维持关联）。

## 消息表示的角色登记（L1）

`StoredMessageState`（crates/deepstrike-core/src/runtime/kernel/wire/checkpoint.rs）是 L1 持久语义权威；其余表示各有登记角色，不得越界使用：

| 表示 | 登记角色 |
|---|---|
| `StoredMessageState` / `StoredMessageBody` | **L1 持久权威**（checkpoint/journal 中的语义真值） |
| `DurableContent` 族 | **L1 内容词表**：`Text/Image/Audio/Video/File` × `DurableSource{Url, Base64, FileId, Object}` |
| `LogicalMessage` | **入边界 Intent 形态**：仅 StartOperation initial_context（不可带 tool_calls） |
| `ProviderMessage` | **渲染/事实边界形态**：render 输出与 ProviderCompleted 载荷 |
| `types::Message` / node `Message` | **遗留内部形态**（缓刑，按迭代总谱 0.2.67/68 收敛为 CoreMessage） |
| `ContentPart` | **遗留渲染期形态**（缓刑，inline base64 将删；大字节由 adapter 在 L0 物化） |

工具关联是**结构字段**（`tool_calls` 前向指针 + body 内 `tool_call_id` 后向指针），不是 content part。Reasoning 不进内容词表（条文 B3）。

**token 数字的分界线**：出现在 checkpoint 里 = 冻结记账锚（合法——restore 必须复现 budget 算术，即使 tokenizer 变了）；出现在运行期消息对象上 = 必须有 fingerprint provenance 的 TokenMeasurement，否则违规。

## 注册编码：content-parts-v1

wire 上 `ProviderMessage.content` / `LogicalMessage.content` 类型是 `String`；多模态 parts 经注册编码承载：

```text
content = "[[deepstrike-content-parts]]" + base64url(JSON(parts))
```

这是注册表 `content-parts-v1` 的定义，不是走私通道。编码必须自描述、可拒绝：**未知 prefix 的 content 按字面文本处理，永不猜测**（B5）。wire 一等化（`content: String | Parts`）留给 ABI rev 流程，其触发条件见迭代总谱 §8——不为"类型漂亮"升级 ABI。

## Opaque JSON 立场（F10 宪法）

> **SDK 侧 ABI 投影的 opaque JSON（`Record<string, unknown>` / `dict[str, Any]`）是宪法，不是欠债：core 拥有 schema 与 canonical bytes 的唯一实现权，typed mirror 是特许而非权利——每引入一个 typed 字段，即在某 SDK 内创造一份需要登记的本地语义，必须随 parity fixtures 钉死。**

## 边界不变量（B1–B9）

| # | 条文 |
|---|---|
| B1 | vendor 原文不入 kernel 状态；受控词表落地，`Other` 不透传 |
| B2 | 大对象字节不过 kernel：host store 持字节，kernel 只持 ref+digest+preview |
| B3 | reasoning 是 L0 evidence，不入 L1 Content |
| B4 | usage 进 kernel 仅 settlement 两数字；全字段 measurement 留 host |
| B5 | content 跨边界编码必须注册；未知编码按字面文本处理 |
| B6 | 消息语义唯一权威 = StoredMessageState；其余形态按角色登记表使用 |
| B7 | attempt 级事实（route/usage/证据/墙钟）全部 host 侧 evidence，永不为内核输入 |
| B8 | 过内核边界的数据必是 Intent/Fact/Decision 之一 |
| B9 | host 永不声明 caller；身份由内核从已发布表面推导 |

## 演进纪律

wire 层全量 `#[serde(deny_unknown_fields)]`，**additive-only 演进**；ABI 版本 = crate 版本，无 per-record schema_version。新增对象必须带六标签登记（Domain / Authority / Durability / Identity / Causation / Replay）；新增 id 必须登记四模式归属；新消息形态必须进角色登记表。**施行后的违例形态：全部应该是 CI 红，而不是复盘发现。**
