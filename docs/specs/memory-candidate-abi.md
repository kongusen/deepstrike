# 记忆候选回执 ABI（草案）

状态：草案，未实现。对应契约 M2（`xiaoman-sdk-extension-contract-v2.md` 第 3 节）。
范围：新增一个 canonical 内核结果，使模型发起的记忆写入可以表达“待确认”，而不被记为已持久化。
非目标：本文不定义六维分类、用户确认流程或来源业务校验，这些归宿主。

## 0. 前提修正（2026-10-26 核实）

本草案原先假定“模型写入”是模型可以直接发起的记忆写入。源码不是这样：

- `crates/deepstrike-core/src/runtime/kernel/wire/driver.rs:1692` 的 F15 裁定明确：内核没有面向模型的记忆写入工具，`memory` 工具只读。
- `RequestMemoryWrite` 的唯一调用通道是子 agent 的 `parent_requests`（子到父），由 `plan_memory_write`（`driver/syscall.rs:387`）转成 `persist_memory` 效果。

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
2. 新事件是否进入现有 journal 的 `Mm` 类别，还是需要独立类别。
3. `replay.rs` 的计数字段命名，是否需要为 `pending` 单独计数。
