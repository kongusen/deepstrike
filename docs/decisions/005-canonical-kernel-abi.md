# ADR-005：Canonical Kernel ABI

## 状态

Accepted

## 日期

2026-07-29

## 背景

内核 ABI 有两段历史：v1 建立了 input/action/observation 与跨语言驱动，是“简单行为 ABI”的依据；v2（ADR-002）补齐 operation/event/effect identity、strict lifecycle、structured fault、delivery-aware signal、reservation budget、typed cancellation 与 prepare/commit，是“可靠事务 ABI”的依据。但 v2 是增量叠加的结果，并未收敛成唯一最优边界：配置面同时存在 `ConfigureRun` 与十余个 `Set*`/`Load*`；root workflow 必须先伪装成 agent run 再靠特权 `complete_run` 收尾；effect result 分散为多个顶层 variant；`Done` 被建模成不需要 host 执行的 effect；direct `step` 与 durable prepare/commit 并列公开；snapshot 仍是 full journal 加一份完整 `KernelStep`；host session、文件路径、vendor raw error 与 `now_ms` 仍在穿越 core。2026-07-29 已针对基线 v0.2.50（`23db0ed`）完成四簇内核源码溯源复核（transition/durable、effect/causation/payload、checkpoint/recovery、input authority/config），结论为 CONFIRMED 31 / DISCREPANCY 37 / RISK 17，并形成唯一裁决记录（DEC-1..9 与迁移前止血清单）；本 ADR 冻结的是按该裁决修订后的 spec，而不是复核前的提案版本。

## 决策

### 1. 运行时只存在一套 Canonical Kernel ABI

不在 v1 与 v2 之间二选一，也不并存两套协议。0.2.51 的 runtime 不提供旧 ABI adapter、协议协商、按 payload 形状猜版本或 SDK fallback；wire revision 提升为 `KERNEL_ABI_VERSION = 3`，只作 fail-closed 安全门禁，不作为产品能力名称。unknown revision/field/variant/effect ID/causation、lifecycle 不符或 digest 不符一律拒绝，且拒绝不推进 step sequence、不改变 lifecycle、不发布任何 effect。旧 ABI 的未完成 operation 不可恢复：升级前必须完成或取消，需要继续恢复的部署停留在 0.2.50。产品文档统一称 Kernel ABI，历史 ADR 保留 v1/v2 名称。

### 2. 顶层 input 收敛为五类，并强制归约到 P1/P2/P3

`KernelInput` 只保留 `ConfigureOperation`、`StartOperation`、`ResolveEffect`、`DeliverExternalEvent`、`HostControl` 五类；分类的意义是权限与生命周期，不是减少 enum 数量，五类必须走不同 validation path。现行 54 个扁平 variant 按此归约（约 11:1），并且必须同时删除 `lifecycle_transition` 的 `_ =>` 兜底臂，为每个 variant 显式声明 authority 与 lifecycle 二元组——只折叠 enum 而保留兜底放行，等价于把整张配置面永久留在 live-mutable 集合内。wire input 不是新的内核原语：任何 variant 在进入协议前必须写明它归约到 P1 syscall、P2 task table 还是 P3 context VM，无法归约的能力不得加入 ABI。不引入自由形式的 `AgentRequest { actor_id }`，并清除生产中语义等价的 caller 自声明旁路（`submitter_agent_id` 省略即提权、`SpawnSubAgent`、host 代发 `SkillActivated`、SDK 伪造 tool result 等）。

### 3. Root 启动是原子事件

根入口统一为 `StartOperation { entry: RootEntry::Agent | RootEntry::Workflow, initial_context }`。agent root 直接产生 `CallProvider`，workflow root 直接建立 DAG 并产生 `SpawnTasks`；root workflow 的 `LoadWorkflow` 与特权 `CompleteRun` 删除，workflow 完成由 kernel 直接提交 terminal。`RootKind` 在 operation 生命周期内不变，`ExecutionFocus` 可在 agent turn 与 nested workflow controller 之间切换：agent 通过 syscall 启动 workflow 时 root kind 仍是 `Agent`，nested workflow 完成后恢复 parent agent 而不提交 root terminal。`InitialContext` 与 `LogicalAgentSpec` 是独立 wire DTO，不复用携带 host session/path 的 SDK 类型。

### 4. 唯一 durable transition protocol

production transition 只允许 `prepare → journal CAS append → commit`：core 在 decode 前执行 bootstrap 字节/深度限制，随后 `normalize → validate → plan`，返回 `Prepared(record_bytes, planned_step, token)`；host 对 journal 做 `compare_and_append`，成功后 kernel `commit(token)`，只有已 commit 的 planned step 才可发布 effect、observation 与 terminal。`abort` 的边界严格划在 append 之前，append 成功后的任何失败一律走“丢弃 runtime + 从 journal rebuild”；CAS conflict 必须闭环为 abort → 重读 head → rebuild → 重放当前 input，而不是只 abort 后上抛。幂等锚定“caller 可提供的 `input_id` + durable journal”，不锚定内存重放窗口，256 条窗口降级为实现优化，结果事件不得携带 host 墙钟（DEC-2）。effect-level 去重命中时 preparation 返回 `Replayed` 并指向既有 record 的 `step_seq`，不产生新 record、不报 `Prepared`（DEC-1）。bindings 不再公开 production direct `step`；benchmark 与单元测试改走 `InMemoryKernelJournal` 上的同一协议。

### 5. Effect 生命周期由 kernel 独占，host 只回灌结果

所有 host effect 的完成统一经 `ResolveEffect { effect_id, outcome }`，`EffectOutcome::Succeeded | Failed` 是唯一入口；approval、milestone、memory、page-in/page-out 与 task control 都必须具备同构失败路径。`SpawnTasks` 在 effect 提交前由 kernel 分配 `task_id`、`attempt_id` 与 `launch_token`，host 只能确认执行结果，task attempt 只有在 spawn acknowledgement 之后才进入 Running。每个 effect kind 至多一个 pending effect，发出新的同类 effect 前必须先 resolve 旧的，静默驱逐同类登记的行为删除（DEC-3）。kernel 不驱动重试：收到 `Failed` 只做一次策略决策，不得对同一意图自动重发 effect；重试由 host 以新 causation 发起并按 effect/launch token 幂等（DEC-5）。host 对无法执行或未知的 effect 必须回 `Failed { ProtocolError }`，不得静默丢弃（DEC-7）；operation configuration 增加 host effect 支持面声明，kernel 对未声明支持的 effect kind fail-closed 拒绝发射（DEC-8），消除同一 effect 在四种语言产生不同 outcome 的现状。provider vendor raw error 只进 host diagnostics，不作为 kernel recovery decision 的输入。

### 6. Terminal 不是 effect，terminal 是硬闸

`Done` 从 `KernelEffect` 中移出，terminal 成为 `KernelStep.terminal`：不要求 resolution，不分配 effect ID，与其 terminal observation 在同一个 committed step 提交，usage report 只在 terminal 内唯一提交。terminal 之后拒绝**一切** state-changing input，包括 `DeliverSignal`——被拒 signal 走 typed control rejection，不写 journal、不进 signal queue（DEC-4）。terminal 之后相同 input 的 replay 返回同一 step，新 input 返回 `InvalidLifecycle`。

### 7. Agent 权限由 causation 推导，memory input 只接受 proposal

P1 syscall 不是允许 host 自由声明 actor 的第六类 wire input：当前 operation 的 provider tool call 由 kernel 在处理 provider resolution 时识别并进入 P1，child 对 parent 的请求只能附着在 `ChildCompleted(task_id, attempt_id)` 上由 parent kernel 从 attempt 推导 caller。agent 的 memory input 只接受 `MemoryWriteProposal`/`MemoryQueryProposal`，不得携带 tenant/namespace、record ID、author/trust、timestamp 或 session provenance；kernel 使用 operation 绑定的 opaque `MemoryAccessBinding`、envelope accepted time 与 causation 生成最终 memory effect 与 provenance。quarantined task 不得通过 workflow append、memory scope 或 capability mutation 提权；agent syscall 不能调用 host-only root workflow。

### 8. External payload 与 page-in/out 唯一承载于 P3 handle/residency

大结果正文不再双向穿越 core：host 在提交 `External` 之前先持久化正文，kernel 只校验 digest、size、preview 与 configured threshold，并为 inline/external tool result 分配或更新 P3 `Handle`，residency 区分 `External`（生成即超限）与 `PagedOut`（压力归档）。`read_result` 等 meta-tool 归约为 `SyscallRequest::PageIn { handle_id }`，只能产生相应的 `LoadPayload` effect。`SpoolLargeResult` effect/result/observation 及通过 SessionLog 扫描原结果的恢复路径删除；`PayloadRef` 是 opaque locator，不得解释为文件路径，正文的真实性、权限、加密与 retention 由 host 负责。命名冲突按 DEC-9 消除：P3 syscall 保留 `PageIn { handle_id }`（读回），host 向 knowledge partition 推送条目的现行 `PageIn { entries }` 更名为 `SeedKnowledge` 并归入 host command 类。

历史数据不做无损自动升级：旧 `spool_ref` 只记录宿主路径，缺少 canonical `digest` 与
`original_size`，因此不能直接改标签为 `PayloadRef`。仍可访问正文的宿主必须重新读取正文、
计算 SHA-256 与 UTF-8 字节数、写入新的 `PayloadStore` locator，再生成完整 `External`
descriptor；正文缺失或不可验证时必须作废该 handle，并让后续 `PageIn` 返回 typed
`StorageUnavailable`，不得猜测 digest/size 或继续解释旧路径。

### 9. 恢复只依赖 logical checkpoint + bounded tail

恢复统一为 logical checkpoint + bounded tail：`LogicalKernelState` 按 transition / P1 syscall / P2 scheduler / P3 context VM 四个 owner 分区，每项 correctness state 只出现一次；checkpoint 只以 `state_digest`/`tail_digest` 表达最后一次 transition 的等价性，不内联 `RenderedContext`，durable record 与 checkpoint 均不保存完整 `KernelStep`。安装使用 candidate → host 持久化 → covered-head CAS install → `ack_checkpoint` 协议：candidate 之后允许继续 append 并保留为 tail，install 不要求 covered head 仍是当前 head，ack 之前不得回收 covered prefix。tail 同时按条数与字节设 watermark，超过 hard limit 时 prepare 返回**可重试**的 `CheckpointRequired`（该 input 尚未 accepted），取代现行“一旦 overflow 即永久禁 snapshot 且永久拒 prepare”的 latch。full accepted-input snapshot、generic `Resume`、workflow `resumed_*` 以及 SDK 用业务 SessionLog 重建 workflow graph 的主路径全部删除。版本字段更名为 `checkpoint_version`（常量 `KERNEL_CHECKPOINT_VERSION`），不得沿用现值为 2 的 `KERNEL_SNAPSHOT_VERSION`——否则 2 → 1 是降号且与 v1 历史值碰撞，会绕过 restore 的 fail-closed 边界校验；`KERNEL_ABI_VERSION` 由 core 定义并经 binding 导出，三个 host 的手抄常量删除（DEC-6）。

### 10. 跨语言 wire 纪律与单一 record/digest 实现

wire 契约固定 `WireU64`（十进制字符串，避免 JS 精度漂移）、strict tagged union 与 unknown-field rejection、fixed-point policy 与 finite observation float、canonical bytes projection。canonical input bytes、record digest 与 chain 只由 core 生成，四语言一律复用，不在各语言重新实现 hash contract——现状是三个 host 各写一份而 core 与 Rust host 各零份。host session/tenant/user ID、文件路径、provider vendor error、异步 handle 与 executor retry/backoff 参数不得进入 core wire；host 只提交 opaque ID、`observed_at_ms`、normalized outcome、child completion、payload 引用与 cancellation 事实。

## 与既有决策的关系

- **ADR-001** 的 operation identity、required mutation 与 observer 分离、budget reservation 原则继续有效。
- **ADR-002** 记录 已废弃 ABI 的历史可靠性切换。本 ADR **supersede 其具体 wire shape**（`KERNEL_ABI_VERSION`、input/effect/observation 的具体形状、direct step 与 full-journal snapshot），但保留其历史记录与仍然成立的可靠性语义（identity、strict lifecycle、structured fault、delivery-aware signal、reservation budget、typed cancellation、命令与事实分离）。
- **ADR-003** “只公开宿主有资源语义的可靠性参数”继续有效；host retry、path、provider config 等字段按本 ADR 进一步移出 core。
- **ADR-004** 的 single transition path、external payload、logical checkpoint + bounded tail 是本 ADR 的必达项。**勘误**：ADR-004 决策 1 的文字顺序 `normalize → validate → plan → commit → journal` 勘正为 `normalize → validate → plan/prepare → durable journal CAS append → commit/publish`——publish gate 在 durable append **之后**，而不是先 commit 再补写 journal。ADR-004 其余表述不变。
- `.local-docs/specs/agent-os-three-primitives.md` 的 P1/P2/P3 是本 ADR 的上游所有权模型；五类 wire input 必须归约到这些原语，而不是形成第四套并行业务状态机。SDK Runtime API 的收敛依赖本 ADR 的跨语言 parity 关口，不得反向决定 kernel 权限或 lifecycle。

## 实施顺序

按 spec §17 的 Phase 0–7 与 Checkpoint A–F 纵向推进，每个任务先加失败的 contract test，每个 checkpoint 保持 workspace 可构建：Phase 0 能力基线与决策冻结（本 ADR 即 Checkpoint A 的门禁）→ Phase 1 canonical protocol types（Checkpoint B：wire golden 全绿、v1/v2 payload structured rejection）→ Phase 2 单一 transaction 与 record chain（Checkpoint C：production direct step callsite 归零）→ Phase 3 root execution 与 workflow authority（Checkpoint D）→ Phase 4 effect、payload 与 provider/executor outcome → Phase 5 logical checkpoint（Checkpoint E：core complete）→ Phase 6 bindings 与四语言 host cutover（Checkpoint F：跨语言 parity）→ Phase 7 SDK Runtime API、文档与 0.2.51 破坏性发布门禁。Phase 0 的 Task 0 是迁移前止血清单，七项纯实现缺陷：milestone effect 四语言泄漏、主循环无兜底导致未知 effect 忙等、Rust 忽略 `step.faults` 并 panic、WASM 悬空 `spool_ref`、Rust 流内异常不发 `provider_error`、`cancel_operation.pending_call_ids` 混用三种 id 命名空间、结果事件 `now_ms` 的 host 侧半修；它们不是契约差距，但会在迁移期把协议不匹配伪装成忙等、panic、悬空引用与不可解析 pending 项。其中五项可独立于 Canonical ABI 立即提交，两项有前置依赖需顺延——`pending_call_ids` 要等 kernel-issued task/attempt/launch token 落地后才有唯一命名空间可指向，`now_ms` 的删除要先让 envelope accepted time 无条件生效（host 侧半修可在 0.2.50 线先落为兼容修复）。迁移期风险已登记：若非 Node host 先切 durable path 而 checkpoint 尚未落地，现有 snapshot-overflow 的“静默降级”会升级为“硬失败”，因此可重试的 `CheckpointRequired` 必须先于该 cutover 落地。

## 非目标

- 不为 旧版 ABI 提供 adapter、shim、协商或 deprecation window；旧 operation 不可在新 runtime 恢复。
- core 不执行网络、文件、数据库、provider、tool 或 sub-agent I/O，也不持有 lease token、API key、路径或可执行 handle。
- 不由 kernel 承担 host 的重试、backoff、真实取消与 blob 加密/retention。
- 不在本 ADR 内决定 `CancellationReason` 由 reason 驱动 terminal 语义的扩展（现状四值折叠为 `UserAbort`，若需要须另行设计）。
- 不把 SDK `run`/`runWorkflow` 的表面 API 形状作为 kernel 权限或 lifecycle 的输入。

## 依据

- 权威 spec：`.local-docs/.local_spc/canonical-kernel-abi.md`（2026-07-29 修订版，含四簇复核落位）。
- 四簇溯源审计与唯一裁决记录：`.local-docs/.local_spc/audit/cluster-a…d-*.md` 与 `.local-docs/.local_spc/audit/adjudication-2026-07-29.md`（DEC-1..9、ACCEPT 清单、迁移前止血清单）。

以上均为本地工作文档（git-excluded），不随仓库发布。repo-facing 的英文计划为 `docs/en/plans/canonical-kernel-abi.md`，其内容尚待与修订后的 spec 同步；在同步完成前，以本 ADR 与本地 spec 为准。
