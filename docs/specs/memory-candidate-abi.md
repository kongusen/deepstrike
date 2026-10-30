# 记忆候选回执 ABI（草案）

状态：Rust core、Rust SDK 分发与 node、wasm、python 三个桥接已实现；node、python、wasm、Rust SDK 的 M1 策略与 M2 候选 sink 均已接入（第 8、9、11 节）。对应契约 M2（`xiaoman-sdk-extension-contract-v2.md` 第 3 节）。
范围：新增一个 canonical 内核结果，使模型发起的记忆写入可以表达“待确认”，而不被记为已持久化。
非目标：本文不定义六维分类、用户确认流程或来源业务校验，这些归宿主。

## 0. 前提修正（2026-10-26 核实）

本草案原先假定“模型写入”是模型可以直接发起的记忆写入。源码不是这样：

- `crates/deepstrike-core/src/runtime/kernel/wire/driver.rs:1692` 的 F15 裁定明确：内核没有面向模型的记忆写入工具，`memory` 工具只读。
- `RequestMemoryWrite` 的唯一调用通道是子 agent 的 `parent_requests`（子到父），由 `plan_memory_write`（`driver/syscall.rs:383`）转成 `persist_memory` 效果。

本规格仍保持 `memory` 搜索工具只读，同时新增 capability-gated 的 `propose_memory` 模型入口。该入口只接受 `name`、`kind`、`content`、可选 `description` 与 `evidence_refs`，严格拒绝 scope、record id、trust、时间戳、lease、receipt 和 attestation 等宿主字段；它解码为现有 `RequestMemoryWrite` 并复用 `plan_memory_write`。只有 memory enabled、writable binding、candidate sink 存在且 `model_writes = candidate_sink` 时才暴露，保证模型提案只能进入审核而不会直写 store；宿主仅在开启时下发 `propose_memory_enabled`，旧内核在未启用时仍可接受配置。原有 child request 继续使用 `source = child_request` 与既有 v2 candidate IDs，直接模型提案使用 `source = model_proposal` 的 v3 identity。

## 1. 改动前的状态

本节描述 M2 实施之前的协议；当前协议见第 2、7 节。改动前内核只有一种记忆写入回执。

- 宿主回执：`memory_persist_result`，二选一：
  - 成功：`record_ref` + `digest`。
  - 失败：`error`。
- 内核效果结果：`memory_persisted`，带 `receipt { binding_id, record_ref, digest }`。
- 失败路径一律 `failedEffect(..., retryable=true)`（`error` 分支）；缺少 `record_ref`/`digest` 时非可重试。

现状的问题：候选写入没有“已接收但待确认”的状态。如果把待确认强行映射成持久化回执，宿主审计会误记为已写入。如果映射成失败，效果会被可重试地重放，导致重复提交。两者都违反契约 S03。

## 2. 提议

### 2.1 宿主回执事件

新增事件 `memory_candidate_result`，与 `memory_persist_result` 并存，不修改后者的语义。

```text
{
  kind: "memory_candidate_result",
  effect_id: string,
  candidate_id: string,            // 由宿主基于 operation/effect/ordinal 确定性生成
  status: "pending" | "accepted" | "rejected",
  receipt_id: string,              // 宿主回执的稳定标识，用于重放
  reason_code?: string,            // rejected 时必填
  record_ref?: string,             // accepted 时必填
  digest?: string,                 // accepted 时必填
  error?: string                   // 调用失败（超时、sink 不可达）时使用，其余字段省略
}
```

约束：

- `status = "accepted"` 必须同时带 `record_ref` 和 `digest`，缺失则效果失败且非可重试（与 `memory_persist_result` 一致）。
- `status = "pending"` 不带 `record_ref`/`digest`。它不是持久化回执；虽沿用 `memory_persisted` 成功类型的 candidate 分支，内核不得据此生成 `MemoryWritten`。
- `error` 只表示调用没有得到回执。这种情况可以重试，但重试必须使用同一个 `candidate_id`，由宿主做幂等。

### 2.2 内核效果结果

保持 `EffectSuccess` 每个 EffectKind 恰好一个变体的封闭联合。`PersistMemory` 仍使用 `MemoryPersistedSuccess`，其中 `receipt` 与 `candidate` 均为可选，且必须恰好出现一个：

```text
{ kind: "memory_persisted", receipt: { binding_id, record_ref, digest } }
{ kind: "memory_persisted", candidate: { binding_id, candidate_id, receipt_id } }
```

`pending` 使用 candidate 分支并使效果进入终态；产生 `MemoryCandidatePending`，不产生 `MemoryWritten`，不改变召回状态。`accepted` 使用旧 receipt 分支；`rejected` 使用不可重试失败。两分支同时存在或同时缺失均为 `UnexpectedEffectOutcome`。旧 receipt-only JSON 保持兼容。

观察是宿主审计信息，不直接注入模型上下文。候选身份保留在 sink 与宿主事件中，accepted 的 canonical receipt 不额外保存 candidate_id。

### 2.3 幂等与重放

- `candidate_id` 从规范的 operation、effect 与候选序号派生，恢复时不得重新随机生成。
- 相同 `candidate_id` 且 payload 相同：sink 返回原回执，不新增候选；丢失回答后可以再次调用 sink。
- 相同 `candidate_id` 但 payload 不同：拒绝（`reason_code = "candidate_conflict"`），不覆盖原记录。
- 重放 journal 时，效果结果必须来自已持久化的 `receipt_id`，不得重新调用宿主。

## 3. 已实现的改动位置

Rust core 在 `wire/effect.rs` 定义互斥结果，在 `wire/driver/effects.rs` 结算候选，在 `observation.rs` 提供待审观察。四个 SDK 的 canonical runner bridge 解析 `memory_candidate_result`。node、python、wasm 的 session log 与 kernel event adapter 共享 52-kind 词表。运行时策略、sink 与写入路径见第 8、9 节。跨 SDK outcome golden fixture 尚未统一，不把旧草案中的新增枚举和 replay 计数方案视为已实现。

## 4. 兼容性

- 新增是加法：旧内核不认识 `memory_candidate_result`。宿主必须通过能力版本检查决定是否发送它，检测不到能力时拒绝启用候选写入，不得回退到 `memory_persist_result`。
- `memory_persist_result` 的语义不变。

## 5. 验收

与契约第 7 节的对应关系：

- S03：模型写入返回 `pending`，观察信息与事件都表达待确认，不计入召回。
- S04：sink 已落库但回执丢失，重放相同 `candidate_id` 得到原 `receipt_id`，无重复候选。
- S05：伪造来源由宿主拒绝，`reason_code` 明确。

每种状态需要 Rust 与三种桥接层各有一个 golden 测试。

## 6. 待内核负责人确认的问题

1. `pending` 是否应使效果进入终态。本草案假定是终态，且不可重试；如果需要等待确认后再结束效果，则应改用挂起（suspend）语义，这需要另外的 ABI 设计。

   研究结论：应为终态。挂起语义与内核现有的四条规则冲突，而终态与记忆写入现有的形状一致。

   - DEC-3（`transaction.rs`，“每种 kind 至多一个待决效果”）：挂起到人工审核结束的 PersistMemory 会占住唯一的名额，运行中之后的每一次写入都会触发 `ResourceLimitExceeded`。这是内核故障，宿主必须从 journal 重建，不是一次普通的拒绝。由 `a_pending_candidate_frees_the_memory_write_slot_and_an_unsettled_write_holds_it` 钉住。
   - §5k（`driver/effects.rs`，记忆查询与 payload 读回的恢复）：同一轮里只要还有兄弟效果未结算，轮次就停在 `AwaitingResume`。挂起的写入会让同轮的查询一直等到审核结束。
   - 终态清空待决效果（`transaction.rs`，“A terminal ends the operation; nothing is left waiting on the host”）：人工审核通常晚于运行结束，挂起的效果在审核结果到达之前就已被清掉，结果无处落地。
   - 形状一致：审批（RequestApproval）的结算调用 `continue_after`，循环的走向取决于结果，因此挂起合理；记忆写入的结算只返回 `quiet_step`，没有任何后续依赖它，写入请求本身也是 `needs_workflow_round: false`。

   终态的代价：本次运行的审计流只记录“候选待审”，不记录事后的接受或拒绝。候选存储是最终结果的唯一记录来源；接受后的记录在之后的召回中出现。如果需要在同一运行里告知模型审核结果，应由宿主经现有的信号通道投递，而不是把效果挂着。

   附带发现：写入请求的唯一通道是子 agent 的 `parent_requests`，提出写入的子 agent 在效果结算时已经结束，`MemoryCandidatePending` 只是面向宿主的观察，没有任何模型上下文收到它（`MemoryWritten` 也一样）。因此第 2.2 节“给模型的观察信息必须区分三种状态”在当前通道上没有可落地的位置，S03 的“观察信息”只能落在宿主的会话事件与审计流上。另外，§7.12 规定终止运行的那次子任务完成不能发布任何效果：最后一个节点的完成如果带着写入请求，整个完成事件会被内核以 `InvalidLifecycle` 拒绝（写这条测试时实测到）。这与 pending 语义无关，但候选写入的宿主实现需要知道这一点。
2. 新事件是否进入现有 journal 的 `Mm` 类别，还是需要独立类别。
3. `replay.rs` 的计数字段命名，是否需要为 `pending` 单独计数。

## 7. 实现状态（已落地的部分）

按上面的草案，已做的是：

- Rust core：`MemoryPersistedSuccess` 的 `receipt` 与新增的 `candidate` 二选一，两者都缺或都有即为 `UnexpectedEffectOutcome`。`pending` 作为效果终态，不写 `MemoryWritten`，产生观察 `MemoryCandidatePending`。同一候选的重投是重放（DEC-1），不同候选是冲突。线上形状由 `a_memory_candidate_outcome_decodes_from_the_bridge_wire_shape` 钉住。
- node 桥接：`memory_candidate_result`，`pending` 映射为 candidate 结果，`accepted` 同现有持久化路径，`rejected` 为不可重试失败，传输错误为可重试失败。
- Rust SDK `canonical_runner_runtime.rs`：`memory_candidate_result` 分支与 node 同语义，由纯函数 `memory_candidate_resolution` 承载，五个单元测试钉住 pending、accepted、pending 缺字段、rejected、传输错误。
- wasm 桥接（`wasm/src/runtime/canonical-kernel-step.ts`）与 python 桥接（`python/deepstrike/runtime/canonical_kernel_step.py`）：与 node 逐行同语义。测试 `wasm/tests/memory-candidate-bridge.test.ts`、`python/tests/test_memory_candidate_bridge.py` 与 node 的五个用例一一对应。

四层桥接测试现已共同读取 `tests/fixtures/abi/memory_candidate_receipts.json`（61 个案例），每个案例只有一份 expected，不再保留 SDK 特例。直接复用桥接测试，无需新增 sdk-conformance domain。真实内核结算由独立测试覆盖，不能把拦截 commit 的桥接测试当作内核集成验证。

对待决问题的处理：

1. `pending` 按终态实现。第 6 节问题 1 的研究给出了四条依据，仍待内核负责人签字确认。
2. 会话事件词表新增一等事件 `memory_candidate_pending` 与 `memory_candidate_rejected`，node、wasm、python 与共享夹具 `session-event-vocabulary.json` 同步更新（52 个 kind）。内核观察 `memory_candidate_pending` 在三个 SDK 中都映射为同名会话事件（`source = child_request`，无 `receipt_id`），与宿主路径写出的事件同形。Rust 的 core `SessionEvent` 已新增同名两个变体（见第 11 节），但 Rust 的会话词表比 52 kind 小，尚未接入共享词表一致性检查。
3. 计数字段未单独为 pending 计数。

## 8. 候选 sink（node，python/wasm 移植见第 9 节）

实现前核实的事实：PersistMemory 效果在任何 SDK 中都没有产生方。F15 之后模型没有写入入口，四个 SDK 构造 `child_completed` 时都不会填写 `request_memory_write`。真实运行中的记忆写入全部走宿主漏斗 `writeMemory`（`sessionExtraction` 会话抽取、`semanticArchive` 语义归档、宿主显式的 `remember`）：内核只经 `admit_memory_write` 做准入，不参与结算。因此 sink 接在两处，共用一个接口：

- 宿主漏斗：`memoryRuntimePolicy` 的 `sessionExtraction` 或 `semanticArchive` 取 `"candidate_sink"` 时，准入通过的写入交给 sink 而不写 store。会话事件按回执记为 `memory_candidate_pending`、`memory_candidate_rejected`，只有 `accepted` 记为 `memory_written`（`record_id` 为 sink 给出的 `record_ref`）。
- PersistMemory 效果：`modelWrites` 取 `"candidate_sink"` 时，回执以 `memory_candidate_result` 提交给内核。这条路径目前没有产生方，由单元测试覆盖，留给将来的产生方。

约定：

- `RuntimeOptions.memoryCandidateSink`；任一路径取 `"candidate_sink"` 而未提供 sink 时，构造即报错。`resolveMemoryRuntimePolicy` 不再拒绝 `"candidate_sink"`，新增 `*ToCandidateSink` 三个布尔量。未设策略时一律走 store。
- `receipt_id` 由 sink 生成。`candidate_id` 由写入的规范身份派生（`memoryCandidateId`，对各部分的 JSON 数组取 sha256）。PersistMemory 仍使用 `v2, agent_id, child_request, operation_id, effect_id, 0`；语义归档仍使用 `v2, agent_id, semantic_archive, session_id, operation_id, effect_id`。会话抽取使用 `v3, agent_id, session_extraction, session_id, operation_id, payload_digest`，其中 `payload_digest` 是候选内容摘要，不包含 `record_id`、时间戳和召回计数。
- sink 合同（`MemoryCandidateSink` 文档）：同 id 同载荷返回原回执且不新增；同 id 异载荷拒绝为 `candidate_conflict`，不覆盖；抛错表示没有得到回答，可用同一 id 重投。载荷比较不含存储 id 与时间戳。`InMemoryMemoryCandidateSink` 是进程内实现，所有候选都记为 `pending`。

会话抽取的 v3 身份解决了辅助模型重试时的幂等边界：相同 proposal 重试会复用 candidate ID 并得到原回执；内容变化的 proposal 会得到新的 candidate ID，不会被错误地当作同一 ordinal 候选；同一回答内的重复 proposal 由 runner 去重。不同内容是否最终被业务 sink 判为冲突，仍由业务侧为其选择的 candidate ID 和载荷决定。

验收覆盖：S03 由会话抽取与语义归档两条真实路径的端到端测试覆盖（store 零写入、只有 pending 事件、没有 `memory_written`）；S04 由 PersistMemory 路径覆盖（sink 已存下候选但回答丢失，重投同一效果得到原 `receipt_id`，sink 只持有一个候选）。S05（伪造来源）需要真实的 sink 判定，进程内实现不做。

候选归档使用候选结果审计，Node/Python 不再把候选的提议 record_id 写入 `semantic_archive_completed`。

尚未做的：

- PersistMemory 的产生方：需要推翻或修订 F15 裁定，属于产品决策。
- S05 与真实 sink 的接入。

## 9. Python / WASM 的 M1 + M2 移植（2026-10-26）

Python 导出 `MemoryRuntimePolicy`、`MemoryCandidateSink`、`InMemoryMemoryCandidateSink` 与身份函数。策略支持 dataclass 或完整 dict，字段为 `retrieval`、`session_extraction`、`model_writes`、`semantic_archive`；RuntimeOptions 使用 `memory_runtime_policy` 和 `memory_candidate_sink`。WASM 使用与 Node 相同的 camelCase 配置和类型，并从包入口及 memory 入口导出。未指定策略保留旧行为；指定策略要求四个开关完整有效，候选路径缺 sink 时构造失败。

两端在预取 hook、会话抽取模型、归档 summarizer/备用模型之前检查开关。canonical memory binding 的 read/write 分别投影 retrieval/modelWrites。宿主漏斗与 PersistMemory 执行共用 sink；accepted 必须包含 record_ref/digest，rejected 必须有 reason_code，pending 不调用 store.put、不产生 memory_written。显式宿主写入保留原接口与直写行为。

候选身份使用第 8 节的 JSON 数组规则。WASM 使用浏览器可用的同步 SHA256 与 TextEncoder，不依赖 node:crypto/Buffer；固定哈希向量验证与 Node/Python 一致，并确保摘要只有一个 sha256 前缀。两端的归档身份使用捕获的 session/effect ID，不再依赖时间戳。WASM 等待归档提交完成，避免任务跨越运行结束后继续读取可变会话状态；代价是辅助模型/sink 耗时进入运行延迟。

移植同时修复了压缩观察中的 action/tier 在 canonical projection 适配时丢失的问题，否则语义归档可能不被触发。Python 在候选路径只使用候选结果审计，不把提议的 record_id 记为 semantic_archive_completed；Node 已在第 10 节的收敛中对齐。WASM 使用候选审计事件，不新增归档完成事件。

验证：Python 构建了实际 Linux 原生扩展，独立加载运行测试而未覆盖原有 Mach-O 扩展；实现阶段 114 项相关测试通过，整合复核 95 项通过，归档审计修正后 31 项候选/归档测试通过。覆盖原生 child request 权限、pending/accepted/rejected 结算、真实会话抽取与真实语义压缩路径。未运行全量 Python 测试。WASM TypeScript 检查与 build 通过；全量 Jest 49 套、229 项通过，追加 accepted/畸形回执与 disabled modelWrites 测试后 12 项 runner 测试通过。WASM Jest 使用 mock kernel；归档通过 helper 与投影传递测试覆盖，本次未运行真实 WASM binding 集成测试。

边界保持明确：进程内 sink 不跨进程持久化，也不做 S05 来源认证；SDK 尚无常规 PersistMemory 产生方。抽取重试的 candidate identity 已改为 v3 payload-addressed：相同 proposal 可稳定重放，不同内容不会因复用 ordinal 而产生错误冲突。进程重启和并发持久化幂等仍需生产 sink 验证。候选身份和载荷比较已在本节与第 10 节扩展。

## 10. 跨 SDK 契约收敛（2026-10-26）

Node/Python/WASM 的运行时身份使用第 8 节 v2 数组规则。新运行即使复用 session_id，也使用新的 operation_id；恢复同一运行保留 operation_id。归档使用原始 effect_id，不用经过名称清洗后的 record_id 作为幂等身份。进程内 sink 按 `(agent_id, candidate_id)` 隔离，因此不同 agent 使用同一个自定义 ID 不会共享回执；同 agent 同 ID 的 source 变化视为冲突。输入候选、返回回执和查询结果均为深拷贝，调用方不能修改已持有状态。

审核载荷包含 scope、kind、name、description、content、provenance 的 author/trust/session_id/evidence_refs，以及 confidence、links、pinned、ttl_days；source 单独比较。仅存储 record_id、时间戳、召回计数不参与冲突比较。数组顺序有意义。agent_id 是宿主提供的命名空间，不构成认证或防伪来源保证。

迁移注意：v2 改变候选 ID 推导和载荷摘要。已有自定义持久化 sink 不得在尚有旧运行重投时直接切换身份版本；应让旧运行使用原 SDK/身份规则排空，或由宿主保存旧 ID 到新身份的明确映射。本仓库提供的 sink 仅为进程内实现，不执行持久化迁移。

共享回执夹具共 61 例，四个 SDK 对 pending/accepted 必需字段都要求非空字符串；数字、布尔、数组等不得隐式转为回执 ID。accepted 桥接保留旧兼容性，不新要求 candidate_id/receipt_id。非 null 的 error 优先，包含空字符串、0、false；字符串直接作为错误消息，结构值使用紧凑 JSON（对象键排序），结果可重试。拒绝原因不是非空字符串时统一使用 unspecified。每个案例只有一个 canonical outcome，仍保持 candidate/receipt 互斥。

真实 WASM 测试 `wasm/tests/memory-candidate-binding.node.mjs` 已接入 `test:canonical-binding`。从当前源码重建 binding 后，测试实际发布 PersistMemory、经 runner/sink/bridge 结算 pending/accepted/rejected、禁用写权限、回执丢失重投、DEC-1 相同结果重放、不同结果拒绝、journal 恢复以及同会话两次抽取。此前本地 pkg-node 二进制仍为旧 receipt-only 协议，真实提交曾报 unknown field candidate；重建后通过，说明仅加载 binding 但拦截 commit 的测试不足以证明 ABI 兼容。

本轮验证：Node build 与全量 Jest 207 套/1399 项通过（6 套/14 项跳过）；WASM build 与全量 Jest 49 套/291 项通过；重建 binding 的 5 个候选集成场景、canonical binding 检查与 74 项 golden 检查通过；Python 实际 Linux 扩展相关回归 139 项通过，未跑全量；Rust 完整 SDK test target 中共享回执测试通过，遍历 61 例，本轮未跑完整 Rust 测试集。进程重启与并发持久化幂等仍待生产 sink 实现后验证。

## 11. Rust SDK 的 M1 + M2 移植（2026-10-27）

`rust/src/memory.rs` 导出 `MemoryRuntimePolicy`（`retrieval`、`session_extraction`、`model_writes`、`semantic_archive`，serde 为 snake_case，四个开关必填、拒绝未知字段）、`MemoryCandidateSink` trait、`MemoryCandidate`/`MemoryCandidateReceipt`（以 `status` 为标签，JSON 即 `memory_candidate_result` 的主体）、`InMemoryMemoryCandidateSink`，以及 `memory_candidate_id`/`memory_candidate_payload_digest`。身份与载荷摘要沿用第 8、10 节规则；固定向量 `candidate:ead40e56…7043` 与其他 SDK 一致，整数值 confidence 按 JSON.stringify 写法序列化。

`RuntimeOptions` 新增 `memory_runtime_policy` 与 `memory_candidate_sink: Option<Arc<dyn MemoryCandidateSink>>`。因 `RuntimeRunner::new` 不返回 `Result`，缺 sink 的校验放在 `run_streaming*` 入口，运行开始前报错，不调用任何模型。未设策略时保持旧行为。

接入点：`set_memory_enabled` 携带 read/write 位，并修正 Rust 桥接此前忽略这两位、恒授 read/write 的问题；retrieval 关闭时跳过预取 hook、模型 QueryMemory 返回空结果、宿主 `query_memory` 返回空；会话抽取与语义归档在调用辅助模型前检查开关，候选路径经私有漏斗 `write_memory_routed` 交给 sink；PersistMemory 在 `model_writes = candidate_sink` 时以 `memory_candidate_result` 结算，sink 出错为可重试的 `error`。公开 `write_memory` 签名与直写行为不变。

四个 SDK 的显式 `remember()` 也经过同一 memory write funnel：先由 kernel 做准入，再按 runtime policy 选择 store 或 candidate sink；拒绝、没有绑定 store 或 sink 返回拒绝时，facade 报错，不返回一个实际上没有写入的 record。显式 `recall()` 经过统一 retrieval gate、`retrievalTopK` 限制、kernel 派生的 recall lifecycle 和 `memory_retrieval_result` 审计；retrieval disabled 时返回空结果。

顺带修正：语义归档名由时间戳改为按 page-out effect 派生（与 Node `stableSemanticArchiveName` 同规则），`provenance.session_id` 改用本次运行的 session_id，审计写入本次运行的会话。core `SessionEvent` 新增 `MemoryCandidatePending`（`receipt_id` 可选）与 `MemoryCandidateRejected`，归入 Mm 类别；内核观察 `MemoryCandidatePending` 投影为 `source = child_request`、无 `receipt_id` 的同名会话事件。Rust 未发出 `semantic_archive_pending/completed/failed`，与改动前一致。

验证（Linux 沙箱，rustc 1.99.1）：`deepstrike-sdk` lib 162 项全过，其中新增 15 项（memory.rs 4 项；`runtime/runner_memory_candidate_tests.rs` 11 项，覆盖缺 sink 拒绝启动、S03 会话抽取端到端、无策略直写、全关闭零模型调用/零 store 访问、S04 重投得原回执与冲突拒绝、sink 出错可重试、语义归档同 effect 单候选、归档关闭零调用、内核观察投影、拒绝审计）；`deepstrike-core` lib 1169 项全过；`deepstrike-tests` 187 项通过、23 项忽略；wasm/node/py 绑定 crate `cargo check` 通过。未跑 clippy/rustfmt（最小工具链未装）。

## 12. Context 投影与门面收敛（2026-10-28）

M4/M5 将显式 memory API 和运行时预取收敛到同一条 kernel-owned 规则链：

- `remember()` 先走 kernel admission，再按 `memoryRuntimePolicy` 选择直接 store 或 candidate sink；`false` 不再被门面吞掉，而是报告写入拒绝。
- `recall()` 和 `pre_query_memory` 都受 retrieval gate 与 `retrievalTopK` 约束，并经过同一 recall lifecycle；命中会更新 recall 计数、触发 promotion edge，并写 `memory_retrieval_result`。
- 初始预取把每个命中作为一条普通 history user turn；canonical 文本为 `[MEMORY record_id=<id> kind=<kind> trust=<trust>] <content>`。
- renewal 期间运行中的 canonical operation 不接受宿主直接追加 history，因此重查命中通过 keyed knowledge `memory:<record_id>` 进入新的 sprint。该条目仍使用相同 canonical 文本，并按 record ID 去重。
- Node、Python、WASM、Rust 均导出共享的 recall projection/helper，并通过固定向量和跨 SDK conformance 验证。

本轮验证包括四端 shared-evidence snapshot/conformance、Node/Python/WASM/Rust 的 M4/M5 回归、真实 WASM kernel 的 P0/P1/P2 boundary checks、candidate binding 和 renewal re-query。WASM 的 `test:canonical-binding` 入口需要先安装 `wasm-pack` 以重建 `pkg-node`；当前环境缺少该命令，因此本轮使用已有 binding 运行全部真实边界脚本，并先执行 `npm run build` 后通过 renewal binding。Node payload-store 的 age-cleanup 用例仍受沙箱文件时间/删除权限影响；S05 真实来源认证和跨进程生产 sink 仍未实现。

## 13. Durable candidate ledger（2026-10-28）

Node、Python、Rust 新增 `FileMemoryCandidateSink` 作为生产前的文件型参考实现；WASM 继续只接受宿主注入的 `MemoryCandidateSink`。三端共享以下语义：

- 快照以 `(agent_id, candidate_id)` 为唯一键，保存候选 payload digest、source、状态回执、record reference、record digest 和时间戳。
- 相同键和相同 payload/source 返回已保存的原回执；相同键但 payload 或 source 变化返回新的 `candidate_conflict` 回执，不覆盖原候选。
- `resolve()` 只允许 pending 到 accepted/rejected 的一次终态转移；重复 resolve 返回原终态回执。accepted 的 `record_ref`/`digest` 和 rejected 的 `reason_code` 会随回执持久化。
- 候选提交、接受、拒绝和 outbox ack 与 ledger 快照一起原子发布。文件写入使用临时文件、fsync、rename；锁目录串行化跨进程 writer。恢复遇到 JSON 或 schema 损坏时 fail closed，不重建空 ledger。
- `outbox()` 返回尚未 ack 的宿主事件；`ackOutbox()` 幂等删除指定 event。数据库或消息队列实现可以替换该参考 sink，而不改变 `MemoryCandidateSink.submit()` ABI。

## 14. Source attestation/verifier（2026-10-28）

来源证明与候选业务身份分离。candidate identity 仍只由 `(agent_id, candidate_id, payload_digest, source)` 决定；attestation 不进入业务 payload digest。证明尝试单独记录 `attestation_digest`、issuer、key_id、reason code 和时间，失败不会创建正式 candidate ledger entry，也不会占用 candidate identity。兼容 `submit()` ABI 的 rejected receipt 只表示本次提交未获准入，携带新证明可以用同一 candidate ID 重试。

attestation canonical JSON 数组包含：`issuer`、`key_id`、`agent_id`、`tenant_id`、`namespace`、`candidate_id`、`source`、`payload_digest`、`issued_at`、`expires_at`、`policy_version`。verifier 通过 `verify(agent_id, candidate)` 接收 agent 边界；默认允许 60 秒时钟偏差，最大有效期 10 分钟。HMAC-SHA256 作为便携 reference verifier 保留；Node、Python、Rust 同时提供只持有公钥的 Ed25519 verifier，签名格式为 `ed25519:<base64url>`，两种 verifier 使用同一 canonical payload。

共享 admission helper 在所有内存/文件 sink 前执行：`host_verified` 必须同时由 `host` author、有效 attestation、匹配 agent/scope/source/digest、受信 key 和有效时间窗口证明；model/extraction/child candidate 可以继续以 untrusted 或 user_asserted 进入 pending，但不能通过修改 provenance 自行提升为 host_verified。

本轮新增跨 SDK memory lifecycle 基础规则：lease 绑定 `agent_id + tenant_id + namespace`，按 `expires_at`、撤销时间和 `max_writes` 拒绝写入；`ttl_days` 过期记录不会从 reference in-memory store 的 search 返回；host 可按 tenant/namespace scope predicate 执行删除同步。durable file sink 的 lease 状态和跨文件系统事务仍属于后续接入工作。WASM 不包含文件 API，以避免把 Node/Python/Rust 的宿主文件假设带入浏览器包。
