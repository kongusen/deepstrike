# 什么是 Agent OS？

DeepStrike 不是「又一个 LLM wrapper」。它是一个 **Agent 操作系统微内核**：内核决定 **何时、是否、以何种预算** 运行 agent 节点；宿主 SDK 负责 **真正执行** LLM 调用与工具 I/O。

这个说法的重点不是品牌化概念，而是工程边界：一旦 agent 能自己写计划、调用工具、派生子 agent、写入记忆、追加 workflow 节点，它就需要一个可治理的控制平面。DeepStrike 把这层控制平面做成微内核。

## 从动态工作流说起

面对复杂任务，模型 increasingly 会 **写一个小 harness**——spawn 多个子 agent，各自独立上下文、聚焦目标，而不是在一个窗口里又规划又执行。

在 IDE 脚本里，这个 harness 往往是 **易失的**（一次性的 JS/Python）。DeepStrike 把它 **内核化**：

```text
LLM 产出结构化计划（WorkflowSpec / submit_workflow_nodes）
        │
        ▼
deepstrike-core  ──  调度：gated spawn · 预算 · 信任边界 · 可快照
        │
        ▼
宿主 SDK  ──  跑 provider、工具、worktree、DreamStore、Webhook
```

**控制流在内核，I/O 在宿主**——这是 Agent OS 的核心分工。

![Agent OS Architecture](/agent_os_architecture.svg)

## 叙事模型：把 harness 变成内核对象

一个临时 harness 通常同时做四件事：保存上下文、决定下一步、执行工具、记录结果。小 demo 可以这样写；长任务会很快失控，因为这些职责被混在一个脚本进程里。

Agent OS 把它拆开：

| 职责 | 归属 | 原因 |
|------|------|------|
| 计划与控制流 | Kernel | 需要可重放、可治理、可跨语言保持一致 |
| LLM / tool / file / network I/O | Host SDK | 需要接入真实环境、凭据、异步 runtime |
| 证据链 | SessionLog | 需要恢复、审计、debug、评估 |
| 长上下文 | Context VM | 需要压缩、page-in/out、prompt cache 断点 |

所以 DeepStrike 的最小心智模型是：**agent 在用户态申请 effect，kernel 决定是否允许，host 负责执行并回灌 observation**。

## 为什么是「OS」，而不是「框架库」？

传统 agent 框架把 loop、memory、governance 散落在 SDK 里，每个宿主各写一套。DeepStrike 借用操作系统的设计语言：

| OS 概念 | Agent OS 对应 | 代码 |
|---------|---------------|------|
| **系统调用 (syscall)** | 一切副作用请求先进 trap：`Invoke`、`Spawn`、`WriteMemory`、`SubmitNodes`… | `crates/deepstrike-core/src/syscall/` |
| **进程 / 线程** | 每个 agent run 是一个 **TCB**（Task Control Block）；sub-agent = 子任务 | `scheduler/tcb.rs`、`proc/` |
| **调度器** | `TaskState`（Ready/Running/Blocked/Suspended/Done）+ `BudgetLedger` | `scheduler/` |
| **虚拟内存** | Context 分区 + Handle 表 + page-in/out（大工具结果）+ knowledge 生命周期（键控条目、边界驱逐、预算、skill 租约） | `context/`、`mm/` |
| **IPC / 信号** | `RuntimeSignal` 注入 state 分区；ReactiveSession 黑板 | `signals/` |
| **安全模块** | Governance pipeline：Allow / Deny / Gate(AskUser) / RateLimited | `governance/` |
| **作业调度 (DAG)** | Workflow 节点 spawn 经同一 gate；Reduce 节点零 token 确定性归约 | `orchestration/workflow/` |

内核 **不** 发起 HTTP、 **不** 读文件、 **不** 调 LLM。Agent OS 不是要替代真实操作系统，而是把 agent 的副作用边界、调度实体、上下文内存和审计日志显式建模。

一句话：**OS 类比用于约束设计，不用于扩大职责。**

## 三大原语（Primitives）

这三件事构成 Agent OS 的最小内核面：所有高级能力都应该能还原到 syscall、TCB / scheduler、Context VM 的组合，而不是散落成宿主 SDK 的特殊分支。

![L* Loop & Syscall Trap](/agent_os_loop_flow.svg)

代码注释中的 P0/P1/P2 收敛为三个可组合原语：

### P1 — Syscall Trap（唯一副作用边界）

```rust
// crates/deepstrike-core/src/syscall/mod.rs
pub enum Syscall {
    Invoke(ToolCall),
    Spawn(IsolationManifest),
    PageIn(PageInRequest),
    WriteMemory(MemoryWriteRequest),
    QueryMemory(MemoryQuery),
    SubmitNodes { count: usize },
    LoadWorkflow { node_count: usize },
}
```

每次 `Invoke`（工具）、`Spawn`（子 agent）、`SubmitNodes`（运行时追加 DAG 节点）都产出统一的 `Disposition`：`Allow` · `Deny` · `Gate` · `RateLimited` …

**意义**：quota、trust、audit 只写一次，对所有 effect 生效。

### P2 — TCB + Scheduler（统一调度实体）

根 agent 与每个 sub-agent / workflow 节点共享同一套 `TaskTable`：

- `TaskState` 统一了 loop lifecycle 与 `ProcessState`
- `AgentProcess` 是 TCB 的 **派生视图**（session log / SDK ABI），不是第二份真相源
- `BudgetLedger` 在 spawn 时扣减 token / 节点配额

### P3 — Context VM（地址空间）

上下文不是 append-only chat log，而是 **分区 + handle 表**：

- Identity / Knowledge / History / State 四槽渲染
- 压缩 pipeline 在压力下回收 token，并标记 prompt-cache 失效点
- Skill / Memory 通过 meta-tool 与 syscall 改变 **能力 manifest**，而非直接 mutate history

## 六种 Harness 模式 = 一等内核节点

![Dynamic Workflow Orchestration DAG](/agent_os_workflow_dag.svg)

Anthropic 文章中的六种可组合模式，在 DeepStrike 里都是 **Workflow 节点 kind**，由同一 executor 驱动：

| Harness 模式 | 内核原语 | SDK 模板 |
|--------------|----------|----------|
| **Classify-and-act** | `classify` 节点 → 选一支、prune 其余 | `classify_instruction()` |
| **Fan-out-and-synthesize** | DAG：N worker → 1 barrier synthesize | `fanout_synthesize()` |
| **Adversarial verification** | 每规则独立 verify TCB，`context_inheritance: none` | `verify_rules()` |
| **Generate-and-filter** | N implement → 1 verify barrier | `generate_and_filter()` |
| **Tournament** | 并行 entrant → 两两 judge bracket | `tournament` 节点 |
| **Loop until done** | `loop` + `loop_continue` + `max_iters`；运行时 `SubmitNodes` | `loop_instruction()` |

详见 [动态工作流](../guides/workflow)。

## 文章之外：内核强制执行的机制

| 机制 | 行为 |
|------|------|
| **Quarantine + 污点传递** | `NodeTrust::Quarantined` 节点不可申请写权限；其 `SubmitNodes` 提交的节点强制 quarantined |
| **Reduce 节点** | 不跑 LLM，纯函数归约依赖输出（dedupe / merge / concat） |
| **output_schema** | SDK 校验节点 JSON 输出，失败重试，仍失败则饿死下游 |
| **WorkflowBudget** | 每次 spawn 告知剩余预算，协调者可按余量决定 fan-out 规模 |
| **model_hint** | 宿主 `provider_for` 路由到不同模型 |

## 脚本 vs 内核：你多得到了什么

| 性质 | 脚本 harness | Agent OS |
|------|--------------|----------|
| 可重放 | 编排状态在内存/文件里，难对齐 LLM 消息 | SessionLog + KernelSnapshot 可重建 |
| 受治理 | 自行 if/else 拦工具 | 统一 syscall gate + audit event |
| 可恢复 | 中断常需重头 | wake/resume workflow + 运行时 append 的节点 |
| 跨语言 | 绑定一种运行时 | 同一 `deepstrike-core` → Py / Node / WASM |
| I/O 归属 | 混在一起 | 内核纯计算，SDK 拥有 provider/工具/存储 |

## 完整用例（概念性）

「逐条验证技术声明，每条规则一个 verifier + 一个 skeptic 汇总」= 一张 `WorkflowSpec` DAG：

```python
from deepstrike import WorkflowSpec, WorkflowNodeSpec, verify_rules, RuntimeRunner

spec = verify_rules(
    rules=["声明 A 有出处", "声明 B 可复现", "声明 C 无过度推断"],
    synthesize="汇总 skeptic 结论，列出未通过项",
)
outcome = await runner.run_workflow(spec)
```

内核 **门控每次 spawn**、在 join 处挂起、完成时推进 DAG；宿主 **跑 LLM 与工具**。

## 下一步

- [内核与宿主分层](./overview) — 模块地图与分层图
- [执行模型](./execution-model) — 一次 turn 的逐步拆解
- [Hello Agent](../getting-started/hello-agent) — 最小可运行示例
