---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelInput, KernelEffect, BudgetLedger, TaskId, ResourceQuota, Message]
  python: [RuntimeRunner]
---

# Runtime Authority 矩阵与身份模型

本文颁布两件事：①全系统核心数据的**权威归属矩阵**（同一事实只能有一个 authority，A1）；②**身份铸造四模式**与身份注册表（A3）。层模型与表示词表见 [Runtime Data Model](./runtime-data-model)。

> 出处：P1 Authority Matrix 与 P2 Identity & Causality（2026-09-15，存档于 `.local-docs/specs/`）。裁决编号 D1–D5 在此为终态。

## 身份铸造四模式

全系统只有四种合法的身份产生方式，每个 id 必须归入其一；归不入即设计错误。

| 模式 | 含义 | 实例 |
|---|---|---|
| **kernel-minted** | 内核铸造，host 只许传递；host 伪造 = 协议错误 | EffectId / TaskId / AttemptId / HandleId / LaunchToken / SignalId |
| **host-minted, kernel-bound** | host 提议，内核在首个 accepted input 处绑定并冻结 | OperationId（唯一一例；绑定后不可变是可重放性的前提） |
| **provider-minted, kernel-adopted** | provider 产生，内核采用而非重铸（严格配对要求回放一致） | CallId（唯一性作用域 = 包含它的 turn，跨 turn 关联必须走 effect/task 链） |
| **content-addressed** | 值即内容哈希，无铸造者问题；算法前缀 `sha256:` 是升级通道 | Digest / requestFingerprint |

内核侧 id 全部是 branded string（crates/deepstrike-core/src/runtime/kernel/wire/scalar.rs）：非空、限长、无控制字符、反序列化时拒绝数字（防 JSON 数值静默升格为 id）。

## 权威矩阵（主表）

### L0 · Protocol

| 数据 | Authority | 非权威表示 |
|---|---|---|
| wire request/response 原始字节 | Provider（线上既逝）；adapter 归档副本 | evidence → SessionLog |
| ProviderReplay / ProviderWireEvidence | Host Provider Adapter | evidence → SessionLog |
| vendor 方言映射 | wire-family normalizer | — |
| 协议能力声明 | host 静态词表（node/src/providers/protocol-capabilities.ts） | reference |
| API key / credential | Host CredentialVault | **永不在任何 fingerprint/journal 内** |

### L1 · Canonical Semantic

| 数据 | Authority | 非权威表示 |
|---|---|---|
| 消息/内容语义本体 | StoredMessageState（B6） | projection → 渲染；mirror → SDK |
| 大对象字节 | Host Object/Payload Store | reference → kernel Handle / DurableSource::Object |
| reasoning 轨迹 | Host（L0 evidence，B3） | evidence → ProviderWireEvidence |
| token 计量值 | Host TokenMeasurement（指纹键侧表） | measurement；checkpoint 内 = 冻结记账锚 |

### L2 · Execution

| 数据 | Authority | 非权威表示 |
|---|---|---|
| ProviderRequestPlan + 指纹 | Host（sanitized plan 的 sha256，node/src/providers/request-plan.ts） | evidence → `prompt_measured` |
| RecordedPromptMeasurement | Host，绑 request 指纹（不匹配即弃用） | evidence → SessionLog |
| ProviderUsage / NormalizedProviderUsage | Host per-wire-family normalizer | measurement |
| ResolvedProviderRoute | Host route resolver（内容寻址 routeId） | evidence → `run_started.route` / provider_attempt |
| ProviderAttempt | Host execution runtime（主键 = effect_id + attempt_seq） | evidence → SessionLog |
| UsageAccountingPolicy → ModelUsageSettlement | Host accounting policy | 两数字过边界 → kernel |
| CostObservation / PricingSnapshot | Application/Billing | — |

### L3 · Kernel Control

| 数据 | Authority | 非权威表示 |
|---|---|---|
| 五类 KernelInput 的权威来源 | 类型即矩阵：`KernelInput::authority()`（crates/deepstrike-core/src/runtime/kernel/wire/envelope.rs） | — |
| effect_id / causation_input_id | kernel-minted | reference → host 执行侧 |
| Task 生命周期 / 进程谱系 / WaitSet | kernel | projection → LogicalStateProjection |
| Capability grant/lease | kernel（checkpoint 内） | reference → host 执行点 enforcement |
| **每 operation 预算** | kernel BudgetLedger | report → UsageReport 事件 |
| **跨 operation 组预算** | Host GroupLedger（node/src/runtime/run-group.ts） | **单向委托边**（D4） |
| Context VM 状态 | kernel | projection → render |
| Memory record 内容 | Host MemoryRecordStore | receipt → kernel MemoryPersistReceipt |
| StopReason 词表 | kernel 受控（`Other` 不透传 vendor 原文） | mirror → host 映射输入 |

### L4 · Evidence / Persistence

| 数据 | Authority | 非权威表示 |
|---|---|---|
| Journal record 字节 + digest 链 | **core**（host 重算哈希 = 不可达状态） | CAS 存储原语 → host journal 实现 |
| Journal head | host 存储层 CAS | — |
| Checkpoint 内容 | kernel（三重 digest 链） | 存储 → host |
| 配置真值（pruning 后） | genesis record → acked checkpoint 的 resolved_config | — |
| 业务事件历史 | Host SessionLog（独立 seq 空间） | mirror → 四 SDK（词表门禁） |

## 裁决（D1–D5，终态）

**D1 · ProviderWireEvidence 成体。** Authority = Host Provider Adapter；与 `ProviderRequestPlan.fingerprint` **字段级互链**（request_fingerprint 强制非空），response_id 归档，raw_usage 用 BoundedJson 上限截断保留 vendor 原样。禁止靠"同一 turn 先后出现"隐式关联。

**D2 · 内核 usage 最小知情权是宪法。** 内核只消费 settlement 的两个可比较数字（observed_input_tokens / observed_output_tokens）；cache/reasoning/计费语义全部在 host accounting policy 折算。usage 数字进 journal，vendor 语义进得越少 replay 确定性越强。

**D3 · Execution 层对象全归 host。** ResolvedProviderRoute / ProviderAttempt / InvocationOutcome 的 authority = Host Execution Runtime，kernel 侧零新增字段。**kernel 链不延长，host 链从 effect_id 续接。** Route pin 到 attempt，不 pin 到 effect。`Attempt` 双轴命名：harness AttemptLoop = quality attempt；ProviderAttempt = transport attempt。

**D4 · 双层预算 = 两级 + 一条单向边。** 跨 operation 容量池 authority = Host GroupLedger；每 operation 执行权威 = kernel BudgetLedger。唯一合法流向：**host→kernel 仅发生在 configure（委托授予）；kernel→host 仅发生在 settlement（事实回流）**。运行中的预算变更只能走 `HostControl.TightenResourceQuota` 控制面。

**D5 · reasoning = L0 evidence，不进 L1 Content。** 三种 wire 形态、不同跨轮保留规则、不同签名要求都是 vendor 语义；canonical 化会把 vendor 语义拉进 L1。replay 需要的是原始块而非 canonical 形式。

## 投影对纪律（双生类型族）

TerminationReason / PaceAction / ToolCall / ResourceQuota 四族：wire 版 = ABI 权威；内部版 = 语义词表更富的合法 projection（如内部 TerminationReason 9 变体 vs wire 7 变体——UserAbort/Error 映射到**不同终态 kind** 而非 termination 词表）。纪律：①唯一合法出入 = driver 的穷尽转换函数（crates/deepstrike-core/src/runtime/kernel/wire/driver.rs）；②同时 import 两边必须别名注明权威方向（`TerminationReason as WireTermination`）；③内部加变体 → 编译器穷尽 match 强制更新转换缝；ABI 加变体 → ABI rev 流程。
