# 记忆候选回执 ABI（草案）

状态：Rust core、Rust SDK 分发与 node、wasm、python 三个桥接已实现；node、python、wasm 的 M1 策略与 M2 候选 sink 已接入（第 8、9 节），Rust SDK sink 未接。对应契约 M2（`xiaoman-sdk-extension-contract-v2.md` 第 3 节）。
范围：新增一个 canonical 内核结果，使模型发起的记忆写入可以表达“待确认”，而不被记为已持久化。
非目标：本文不定义六维分类、用户确认流程或来源业务校验，这些归宿主。

## 0. 前提修正（2026-10-26 核实）

本草案原先假定“模型写入”是模型可以直接发起的记忆写入。源码不是这样：

- `crates/deepstrike-core/src/runtime/kernel/wire/driver.rs:1692` 的 F15 裁定明确：内核没有面向模型的记忆写入工具，`memory` 工具只读。
- `RequestMemoryWrite` 的唯一调用通道是子 agent 的 `parent_requests`（子到父），由 `plan_memory_write`（`driver/syscall.rs:383`）转成 `persist_memory` 效果。

因此，本规格覆盖的“待确认回执”实际作用于子 agent 提出的写入请求，而不是模型直接写入。`source = model_write` 应改为 `source = child_request`，或在宿主侧把子请求也归类为候选。若要真正开放模型写入入口，需要先推翻 F15 裁定，这是单独的产品决策，不属于本规格。

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

四层的映射目前靠四份手写测试保持一致，而不是一份共享夹具。`tests/fixtures/sdk-conformance` 按 domain 分发，要收敛到一份夹具，需要新增 domain 并让 node、python、wasm、rust 四个一致性适配器（`scripts/sdk-conformance/`）都实现它，这是单独的工作。

对待决问题的处理：

1. `pending` 按终态实现。第 6 节问题 1 的研究给出了四条依据，仍待内核负责人签字确认。
2. 会话事件词表新增一等事件 `memory_candidate_pending` 与 `memory_candidate_rejected`，node、wasm、python 与共享夹具 `session-event-vocabulary.json` 同步更新（52 个 kind）。内核观察 `memory_candidate_pending` 在三个 SDK 中都映射为同名会话事件（`source = child_request`，无 `receipt_id`），与宿主路径写出的事件同形。Rust SDK 的会话日志没有类型化词表，不参与这项一致性检查。
3. 计数字段未单独为 pending 计数。

## 8. 候选 sink（node，python/wasm 移植见第 9 节）

实现前核实的事实：PersistMemory 效果在任何 SDK 中都没有产生方。F15 之后模型没有写入入口，四个 SDK 构造 `child_completed` 时都不会填写 `request_memory_write`。真实运行中的记忆写入全部走宿主漏斗 `writeMemory`（`sessionExtraction` 会话抽取、`semanticArchive` 语义归档、宿主显式的 `remember`）：内核只经 `admit_memory_write` 做准入，不参与结算。因此 sink 接在两处，共用一个接口：

- 宿主漏斗：`memoryRuntimePolicy` 的 `sessionExtraction` 或 `semanticArchive` 取 `"candidate_sink"` 时，准入通过的写入交给 sink 而不写 store。会话事件按回执记为 `memory_candidate_pending`、`memory_candidate_rejected`，只有 `accepted` 记为 `memory_written`（`record_id` 为 sink 给出的 `record_ref`）。
- PersistMemory 效果：`modelWrites` 取 `"candidate_sink"` 时，回执以 `memory_candidate_result` 提交给内核。这条路径目前没有产生方，由单元测试覆盖，留给将来的产生方。

约定：

- `RuntimeOptions.memoryCandidateSink`；任一路径取 `"candidate_sink"` 而未提供 sink 时，构造即报错。`resolveMemoryRuntimePolicy` 不再拒绝 `"candidate_sink"`，新增 `*ToCandidateSink` 三个布尔量。未设策略时一律走 store。
- `receipt_id` 由 sink 生成。`candidate_id` 由写入的规范身份派生（`memoryCandidateId`，对各部分的 JSON 数组取 sha256）：PersistMemory 为 `child_request, operation_id, effect_id, 0`；会话抽取为 `session_extraction, session_id, 序号`；语义归档为 `semantic_archive, session_id, record_id`（记录 id 由效果 id 派生）。
- sink 合同（`MemoryCandidateSink` 文档）：同 id 同载荷返回原回执且不新增；同 id 异载荷拒绝为 `candidate_conflict`，不覆盖；抛错表示没有得到回答，可用同一 id 重投。载荷比较不含存储 id 与时间戳。`InMemoryMemoryCandidateSink` 是进程内实现，所有候选都记为 `pending`。

验收覆盖：S03 由会话抽取与语义归档两条真实路径的端到端测试覆盖（store 零写入、只有 pending 事件、没有 `memory_written`）；S04 由 PersistMemory 路径覆盖（sink 已存下候选但回答丢失，重投同一效果得到原 `receipt_id`，sink 只持有一个候选）。S05（伪造来源）需要真实的 sink 判定，进程内实现不做。

已知限制：会话抽取在重试时会重新调用辅助模型，内容可能变化，同一 `candidate_id` 的新内容会被 sink 判为 `candidate_conflict`。语义归档在候选 pending 时仍记 `semantic_archive_completed`，其中的 `record_id` 在 store 中并不存在。

尚未做的：

- Rust SDK 的 sink 接入；python、wasm 已完成本节接口与策略的移植。
- PersistMemory 的产生方：需要推翻或修订 F15 裁定，属于产品决策。
- S05 与真实 sink 的接入。

## 9. Python / WASM 的 M1 + M2 移植（2026-10-26）

Python 导出 `MemoryRuntimePolicy`、`MemoryCandidateSink`、`InMemoryMemoryCandidateSink` 与身份函数。策略支持 dataclass 或完整 dict，字段为 `retrieval`、`session_extraction`、`model_writes`、`semantic_archive`；RuntimeOptions 使用 `memory_runtime_policy` 和 `memory_candidate_sink`。WASM 使用与 Node 相同的 camelCase 配置和类型，并从包入口及 memory 入口导出。未指定策略保留旧行为；指定策略要求四个开关完整有效，候选路径缺 sink 时构造失败。

两端在预取 hook、会话抽取模型、归档 summarizer/备用模型之前检查开关。canonical memory binding 的 read/write 分别投影 retrieval/modelWrites。宿主漏斗与 PersistMemory 执行共用 sink；accepted 必须包含 record_ref/digest，rejected 必须有 reason_code，pending 不调用 store.put、不产生 memory_written。显式宿主写入保留原接口与直写行为。

候选身份使用第 8 节的 JSON 数组规则。WASM 使用浏览器可用的同步 SHA256 与 TextEncoder，不依赖 node:crypto/Buffer；固定哈希向量验证与 Node/Python 一致，并确保摘要只有一个 sha256 前缀。两端的归档身份使用捕获的 session/effect ID，不再依赖时间戳。WASM 等待归档提交完成，避免任务跨越运行结束后继续读取可变会话状态；代价是辅助模型/sink 耗时进入运行延迟。

移植同时修复了压缩观察中的 action/tier 在 canonical projection 适配时丢失的问题，否则语义归档可能不被触发。Python 在候选路径只使用候选结果审计，不把提议的 record_id 记为 semantic_archive_completed；Node 第 8 节描述的该限制仍存在。WASM 使用候选审计事件，不新增归档完成事件。

验证：Python 构建了实际 Linux 原生扩展，独立加载运行测试而未覆盖原有 Mach-O 扩展；实现阶段 114 项相关测试通过，整合复核 95 项通过，归档审计修正后 31 项候选/归档测试通过。覆盖原生 child request 权限、pending/accepted/rejected 结算、真实会话抽取与真实语义压缩路径。未运行全量 Python 测试。WASM TypeScript 检查与 build 通过；全量 Jest 49 套、229 项通过，追加 accepted/畸形回执与 disabled modelWrites 测试后 12 项 runner 测试通过。WASM Jest 使用 mock kernel；归档通过 helper 与投影传递测试覆盖，本次未运行真实 WASM binding 集成测试。

边界保持明确：进程内 sink 不跨进程持久化，也不做 S05 来源认证；SDK 尚无常规 PersistMemory 产生方；抽取候选的 session/ordinal 身份在同会话多次运行或模型重新生成不同内容时可能冲突。幂等载荷只比较 scope、kind、name、description、content，未覆盖 provenance 等全部元数据，也没有新增跨 agent 身份隔离保证。上述事项不属于本次 SDK 移植已完成的保证。
