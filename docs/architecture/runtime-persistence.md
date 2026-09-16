---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelInput, KernelTransaction, PlannedStep, SessionEvent]
  python: [KernelJournal]
---

# Runtime 持久契约（Journal / Checkpoint / SessionLog）

L4 层宪法：三库三真相的权威划分、每库条文（J1–J7 / K1–K6 / S1–S4）、跨库不变量（P-1–P-5）与错误分类学。三库的角色**不可互替**。

> 出处：P6 持久契约（2026-09-15，存档于 `.local-docs/specs/runtime-data-model-p6-persistence-contract-2026-09-15.md`）。条文编号与链验证器 C 规则一一对应（见 [Causality](./runtime-causality)）。

## 三库三真相

| 库 | 真相角色 | 回答的问题 | 权威 | 编号空间 |
|---|---|---|---|---|
| **Journal** | **State Truth** | "operation 按什么顺序做了什么裁决" | core（record 字节+digest）；host 持 CAS 存储 | `step_seq`（链位置） |
| **Checkpoint** | **State Snapshot** | "到 through_step_seq 为止的逻辑状态是什么" | kernel（三重 digest + 显式投影）；host 持安装/ack | 挂 `step_seq` 边界 |
| **SessionLog** | **Evidence Truth** | "世界看到了什么" | host | `seq`（**独立**，S2） |

同一事实在三库中至多一个权威 home。跨库引用只允许走已登记身份字段（operation_id / effect_id / digest），**禁止时序对齐充当关联**（P-4）。

## Journal 契约（J1–J7）

**J1 · 字节与哈希的唯一实现是 core。** record 由 core 构建，字段私有只读；host 逐字节存储、按 core 给的 digest 索引。"host 重算哈希发现不一致"是不可达状态——重序列化 record 以重算哈希的 journal 实现即违规。decode 时重校验：被篡改的 entry 在边界失败，而不是在 replay 深处。

**J2 · CAS 是存储层原语。** 必须是真原子操作（file lock / `O_EXCL` 链式命名 / 条件数据库更新），不是 read-compare-write 序列。

**J3 · step_seq 与 SessionLog seq 是两个独立编号空间。** 剪枝 journal 前缀永不在业务事件编号上打洞。

**J4 · durable record 永不携带 planned step。** record 存 normalized input + `step_digest`；重建 = 对 canonical input 重跑确定性 transition 并比对 digest。**合法性前提：re-derive 确定性**（混批条文 M3/M4，见 [Causality](./runtime-causality)）——任何破坏 re-plan 确定性的内核变更同时破坏 J4 与验证规则 C3。record 大小是 input 的函数，永不是它产生的 step 的函数。

**J5 · 链即 operation 身份。** genesis record 绑定 `ResolvedOperationConfig`（不是稀疏 config，不是二进制默认值）；genesis 无 previous digest，其 record_digest 即 operation 的 genesis_digest。

**J6 · 七字段记录**：`operation_id / input_id / step_seq / previous_record_digest / canonical_input / input_digest / step_digest / record_digest`。多一个字段即 ABI 变更，少一个即链断。

**J7 · 错误分类学是契约的一部分，永不坍缩为 opaque Error：** `JournalCasConflictError`（**可重试**：abort→重读 head→重建→重放）/ `JournalIntegrityError`（**永不可重试**——重试即重放同一矛盾）/ 存储损坏。**durable-step wrapper 在这三类错误的任何一个上不得发布 effect。**

## Checkpoint 契约（K1–K6）

**K1 · checkpoint 是独立契约形状的 canonical DTO，不是内核内部快照。** LogicalKernelState 由显式投影构建——给状态机加一个字段不能静默改变 checkpoint 格式，DTO 需要的字段不能静默消失。

**K2 · 每块正确性状态恰有一个 home**（transition / syscall / scheduler / context_vm 四分区不重叠）。pending effects、replay ledger、terminal 在 transition；task attempts 在 scheduler；handles 在 context_vm；header 不重复任何一项。`single_ownership_is_structural` 测试扫描序列化文档证明此条——结构即证明。

**K3 · bounded tail 精确**：`tail_inputs` 覆盖 `(base_step_seq, through_step_seq]`，无洞无重无越界，**构造期校验**——会重放出不同历史的 checkpoint 不可构造。

**K4 · 三重 digest 答三个问题**：`state_digest`（状态是它吗）/ `tail_digest`（tail 是这段吗）/ `checkpoint_digest`（整体含 header 是它吗）。全部复用 record 层 canonical bytes——host 验证器不需要第二个序列化器。

**K5 · 安装/回收规则**：covered_head 在**安装点**校验（不是当前头）；**ack 不是 KernelInput**（runtime 维护句柄，不写 record）；ack 可回收前缀 = `boundary(){through_step_seq, covered_head}`；replay/dedupe ledger（accepted_inputs）**永不被 ack 清空**——base 以下 redelivery 得到的是**幂等确认而非 step 重现**。

**K6 · 配置真值链**：genesis record 绑定 → acked checkpoint 的 resolved_config 接续。pruning 后配置真值的权威随之迁移，迁移点即 ack。

> **实施状态**：checkpoint 模块当前只实现 generation/verify；install/restore/rebase/ack 路径属迭代总谱 0.2.65（Task 16）。K5 是已生成未安装的契约。

## SessionLog 契约（S1–S4）

**S1 · Evidence Truth，append-only。** 事件是"发生过什么"的原始记录，只增不改。

**S2 · 编号空间独立**（J3 的另一面）。`seq` 属 SessionLog 自己；journal 剪枝不打洞。

**S3 · 词表 host 所有，mirror 须登记。** 新增事件 kind 必须在四 SDK 词表同步登记，否则即违规（词表 manifest 门禁）。

**S4 · 跨库 join 键 = `effect_id`，永不是 `step_seq`。** 旧日志缺字段时验证器降级标注（C7），不失败。

## 跨库不变量（P-1–P-5）

| # | 条文 |
|---|---|
| P-1 | 一 operation = 一 journal 链；genesis_digest 即其身份 |
| P-2 | 恢复阶梯只有三级，全部幂等：journal 前缀重放 → checkpoint+tail 重建 → base 以下幂等确认 |
| P-3 | 任何库的错误分类学不得坍缩；durable-step wrapper 在 journal 错误上永不发布 effect |
| P-4 | 三库间只允许身份字段互链，禁止时序对齐充当关联 |
| P-5 | 剪枝只发生在 ack 之后、只回收 boundary() 声明的前缀、永不动 replay ledger |

## 存储实现参考

node 侧参考实现：node/src/runtime/kernel-journal.ts（File/InMemory 两实现，三条硬规则在模块头注释）。四 SDK 的 ABI 投影保持 opaque（F10 宪法，见 [Data Model](./runtime-data-model)）。
