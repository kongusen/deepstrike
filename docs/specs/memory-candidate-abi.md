# 记忆候选回执 ABI（草案）

状态：Rust core、Rust SDK 分发与 node、wasm、python 三个桥接已实现并通过测试；宿主侧 candidate sink 未实现。对应契约 M2（`xiaoman-sdk-extension-contract-v2.md` 第 3 节）。
范围：新增一个 canonical 内核结果，使模型发起的记忆写入可以表达“待确认”，而不被记为已持久化。
非目标：本文不定义六维分类、用户确认流程或来源业务校验，这些归宿主。

## 0. 前提修正（2026-10-26 核实）

本草案原先假定“模型写入”是模型可以直接发起的记忆写入。源码不是这样：

- `crates/deepstrike-core/src/runtime/kernel/wire/driver.rs:1692` 的 F15 裁定明确：内核没有面向模型的记忆写入工具，`memory` 工具只读。
- `RequestMemoryWrite` 的唯一调用通道是子 agent 的 `parent_requests`（子到父），由 `plan_memory_write`（`driver/syscall.rs:383`）转成 `persist_memory` 效果。

因此，本规格覆盖的“待确认回执”实际作用于子 agent 提出的写入请求，而不是模型直接写入。`source = model_write` 应改为 `source = child_request`，或在宿主侧把子请求也归类为候选。若要真正开放模型写入入口，需要先推翻 F15 裁定，这是单独的产品决策，不属于本规格。

## 1. 现状

内核只有一种记忆写入回执。

- 宿主回执：`memory_persist_result`，二选一：
  - 成功：`record_ref` + `digest`。
  - 失败：`error`。
- 内核效果结果：`memory_persisted`，带 `receipt { binding_id, record_ref, digest }`。
- 失败路径一律 `failedEffect(..., retryable=true)`（`error` 分支）；缺少 `record_ref`/`digest` 时非可重试。

现状的问题：候选写入没有“已接收但待确认”的状态。如果把待确认强行映射成成功，内核会记为 `memory_persisted`，模型会以为记忆已生效。如果映射成失败，效果会被可重试地重放，导致重复提交。两者都违反契约 S03。

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
- `status = "pending"` 不带 `record_ref`/`digest`。它不是持久化回执，内核不得据此生成 `memory_persisted`。
- `error` 只表示调用没有得到回执。这种情况可以重试，但重试必须使用同一个 `candidate_id`，由宿主做幂等。

### 2.2 内核效果结果

新增效果结果 `MemoryCandidateRecorded`，带：

```text
{ status, candidate_id, receipt_id, record_ref?, digest? }
```

- `pending`：效果以“已记录候选”完成。内核不写入 `memory_persisted`，不改变召回状态。
- `accepted`：语义与 `memory_persisted` 一致，但保留 `candidate_id` 以便追溯。
- `rejected`：效果以“拒绝”完成，附 `reason_code`，不可重试。

给模型的观察信息必须区分三种状态，禁止统一表达为“已记住”。`pending` 应表达为“候选已提交，等待确认”。

### 2.3 幂等与重放

- `candidate_id` 从规范的 operation、effect 与候选序号派生，恢复时不得重新随机生成。
- 相同 `candidate_id` 且 payload 相同：返回原回执，不重新调用 sink。
- 相同 `candidate_id` 但 payload 不同：拒绝（`reason_code = "candidate_conflict"`），不覆盖原记录。
- 重放 journal 时，效果结果必须来自已持久化的 `receipt_id`，不得重新调用宿主。

## 3. 各层改动清单

以下位置均已在 `main` @ `3ac3b07c` 上核实存在。

| 层 | 位置 | 改动 |
| --- | --- | --- |
| Rust core 效果枚举 | `crates/deepstrike-core/src/runtime/kernel/wire/effect.rs:811` 附近 | 新增 `MemoryCandidateRecorded` 变体及 kind 字符串 |
| Rust core 事件分类 | `crates/deepstrike-core/src/runtime/event_log.rs:92` | 把新事件归入 `Mm` 类别 |
| Rust core 重放 | `crates/deepstrike-core/src/runtime/replay.rs:79`、`:212` | 增加候选计数，`pending` 不计入 `memory_persisted_count` |
| Rust 宿主解析 | `rust/src/runtime/canonical_runner_runtime.rs:599` 附近 | 新增 `memory_candidate_result` 分支 |
| Node 桥接 | `node/src/runtime/canonical-kernel-step.ts:1362` 附近 | 新增 case，映射三种状态 |
| wasm 桥接 | `wasm/src/runtime/canonical-kernel-step.ts:1288` 附近 | 同上 |
| Python 桥接 | `python/deepstrike/runtime/canonical_kernel_step.py:748` 附近 | 同上 |
| 夹具 | `tests/fixtures/kernel-wire/` | 每种状态一份 golden fixture |
| 契约测试 | `tests/rust/src/t16_kernel_contract_fixtures.rs:73` 附近 | 把新 kind 加入已知列表 |

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

四层的映射目前靠四份手写测试保持一致，而不是一份共享夹具。`tests/fixtures/sdk-conformance` 按 domain 分发，要收敛到一份夹具，需要新增 domain 并让各 SDK 的一致性运行器实现它（wasm 目前没有接这套运行器），这是单独的工作。

对待决问题的处理：

1. `pending` 按终态实现。第 6 节问题 1 的研究给出了四条依据，仍待内核负责人签字确认。
2. 新观察 `memory_candidate_pending` 没有加入共享的会话事件词表，它经由通用的 `kernel_observation` 事件（带 `observation_kind`）进入审计流。要做成一等事件，需要同步 `tests/fixtures/sdk-conformance/canonical/session-event-vocabulary.json`、python 与 wasm 的词表。
3. 计数字段未单独为 pending 计数。

尚未做的：

- 运行器侧的 candidate sink 与产生 `memory_candidate_result` 的宿主实现。没有它，任何真实运行都不会走到 pending。
