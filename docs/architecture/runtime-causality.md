---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelEffect, KernelInput, PlannedStep, SyscallRequest]
  python: [RuntimeRunner, KernelJournal]
---

# Runtime 因果链与链验证器

本文颁布：①全局因果链（10 跳）与每跳的互链字段；②混批发布排序宪法 M1–M5；③admission 五门；④链验证器规则 C1–C8。原则 A4：跨层关系必须有 ID 互链，禁止"前后顺序大概对应"。

> 出处：P2 Identity & Causality、P5 Intent/Effect/Outcome（2026-09-15，存档于 `.local-docs/specs/`）。

## 因果链（10 跳）

```text
[H1]  operation_id ──bind── genesis record (step_seq=0)
[H2]  operation ──spawn──▶ task        (TaskLaunch parent→child，LaunchToken 幂等)
[H3]  task ──retry──▶ attempt          (ChildCompleted{task_id, attempt_id})
[H4]  input_id ──append──▶ record{step_seq, previous_record_digest}   (哈希链)
[H5]  input ──plan──▶ effect           (causation_input_id)
[H6]  effect ──resolve──▶ ResolveEffect{effect_id, outcome}           (accept_outcome 形状校验)
[H7]  effect ──host──▶ ProviderAttempt (主键 effect_id + attempt_seq)
[H8]  attempt ──bind──▶ request_fingerprint
[H9]  request ──respond──▶ response_id
[H10] journal ⟷ SessionLog             (join 键 = effect_id，编号空间各自独立)
```

H1–H6 在内核 journal 内闭合（State Truth）。H7–H9 是 host 执行证据链（Evidence Truth），从 effect_id 单点续接——**kernel 链不延长，host 链从 effect_id 续接**（D3）。H10 的 join 键是 effect_id；**step_seq 永不进入 SessionLog**（保护 S2 编号空间独立）。旧日志缺 join 字段时验证器降级标注（C7），不失败。

## 混批发布排序宪法（M1–M5）

一个 turn 里 syscall 与 host tool 混杂时的发布纪律（0.2.62 三连环事故的根因层，机制在 crates/deepstrike-core/src/runtime/kernel/wire/driver/ 各模块）：

| # | 条文 | 含义 |
|---|---|---|
| M1 | effects XOR terminal | 一个 step 发布 effects 或 terminal，永不同时 |
| M2 | 每 kind 至多一个 pending effect | effect 结算的 admissibility 前提 |
| M3 | tool batch 不与 syscall effect 同车发布 | 同车发布 = host 收到双 effect step；batch 无法事后重推导。syscall effect 结算后 resume **重新推导** batch |
| M4 | 兄弟 effect 未清，turn 不抢先续跑 | pending 非空 → AwaitingResume；最后一个 settle 的 effect 带着自由手重跑 resume |
| M5 | causation ledger 最后结算 | 任何中途 fault 的转换不得在 journal 里留下半吞状态 |

M3/M4 的 re-derive 确定性是 J4（step 不进 journal、只存 step_digest）的合法性前提——验证规则 C3 同时门禁两者。

## Admission 五门（ToolCall → KernelEffect）

只有过了全部五个门的意图才会变成 KernelEffect（effect_id 只能由内核铸造）：

```text
ProviderCompleted.message.tool_calls  (Fact 载荷)
  ▼ 门1 归属还原    三拒绝：未知 effect / 未曝光 tool / 已消费 call_id（caller 内核推导，B9）
  ▼ 门2 解码        malformed arguments = Rejection（审计事实），永不为 fault
  ▼ 门3 权限族      quarantine：读过不可信内容的 task 不得经特权族扩权，整族 fail-closed
  ▼ 门4 治理        capability lease 先于 governance 管线；评估逻辑 caller 身份
  ▼ 门5 裁决分支    syscall → 内核自答（AnsweredCall）；host tool → ExecuteTools effect
```

syscall（11 个 meta-tool）在 feed 之前裁决——它们改变下一次渲染的内容；assistant message 整体 feed；host tool 随引擎 phase 派发。

## Host 应答义务（DEC-7 / DEC-8）

- **DEC-7**：每个发布的 effect 必须经 `ResolveEffect{effect_id, outcome}` 应答。形状校验：Failed 恒可受理；Succeeded 必须形状匹配发布时的 kind。无答 = operation 悬挂（内核宁可悬挂也不猜测）。
- **DEC-8**：host_effect_support 声明先行（ConfigureOperation 时冻结进 resolved_config）；未声明的能力走降级路径（审计事实与"host 说不能"和"host 做不到"完全一致）。
- 失败事实的词表受控：传输梯子耗尽 → `TransportExhausted`（"The kernel never saw the backoff"）；vendor 原文永不过边界（B1）。**失败也是 Fact，与成功同权入 journal。**

## 链验证器规则（C1–C8）

验证器输入 = journal 前缀 +（可选）checkpoint +（可选）SessionLog。按链分段检查，每段独立出报告；退出码 0=全绿 / 1=违例 / 2=输入不可解析。实现归属：core 库模块 + `deepstrike inspect|verify|replay|fork`（C3 本质是"用内核重放自己"，非 core 不可；验证器是 host ops 工具，不进 SDK runtime 路径）。

| # | 规则 | 强制执行的条文 |
|---|---|---|
| C1 | 链完整性：previous_record_digest 咬合、step_seq 严格 +1、genesis prev=None | J1/J5/J6 |
| C2 | input 幂等：同 input_id 不出现两条不同 record | K5 rule 7/10 |
| C3 | 因果闭合：每个 ResolveEffect 引用的 effect_id 可由更早 record 的 re-plan 复现（step_digest 重算比对） | J4 + M3/M4 |
| C4 | 任务谱系：每个 task 的 parent 链终止于 genesis root；LaunchToken 不重复用于不同 TaskLaunch | H2/H3 |
| C5 | checkpoint 自洽：三重 digest、tail 可从 base 重放到 through_step_seq、covered_head 在安装点校验 | K3/K4/K5-rule2 |
| C6 | 证据 join：每个 ResolveEffect(CallProvider) 存在 ≥1 条带同 effect_id 的 llm_completed；其 fingerprint 有对应 prompt_measured；provider_attempt 与 effect 链逐一对应 | S4 + H7/H8 |
| C7 | 降级标注：旧格式日志缺字段时检查降级而非失败，报告标注 degraded hops | S4 兼容性 |
| C8 | invocation 链伪造检测：同 invocation 的 effectChain 中相邻 effect 间必有一条 Failed resolution input | H7 防伪 |

C3 同时是 **re-plan 确定性的回归门禁**：任何破坏 re-plan 确定性的内核变更会让 C3 红。C2+C3 恰好覆盖 0.2.62 multi-effect 事故的两个失效面——链验证器落地后该类事故从"生产 brick"降级为"CI 红"。

本验证器是 fork-replay 重放台的正确性门禁子集，可独立先行。
