---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelInput, KernelAction, KernelObservation, TaskTable, Tcb, AgentProcess, TaskLifecycle]
---

# 内核与宿主分层

在 [Agent OS](./agent-os) 叙事下，DeepStrike 分为 **微内核（deepstrike-core）** 与 **宿主 SDK**。本节给出模块地图与 OS 类比对照。

## 分层图

![Agent OS Architecture](/agent_os_architecture.svg)


| 层 | 职责 | 不做的事 |
|----|------|----------|
| **Kernel** | 状态机、syscall 裁决、Context 渲染、Workflow 调度、预算 | 网络、磁盘、async LLM |
| **SDK** | LLM 调用、工具执行、持久化、信号、memory store I/O | 自行实现 spawn gate（应交给 kernel） |
| **Provider** | 厂商 API、流式事件、`RenderedContext` → API 格式 | 治理决策 |
| **ExecutionPlane** | 注册/执行工具、worktree、remote sandbox | 上下文压缩 |

## 内核模块 ↔ Agent OS 子系统

| 子系统 | 模块 | 职责 |
|--------|------|------|
| **Syscall & 安全** | `syscall/`、`governance/` | 统一 trap；permission、rate limit、constraint、veto、sandbox |
| **调度 & 进程** | `scheduler/`、`proc/` | L* loop、`TaskTable`、`Tcb`、`AgentProcess` 视图 |
| **Context VM** | `context/` | 分区、渲染、压缩、renewal、Skill catalog、task_state |
| **内存管理** | `mm/`、`memory/` | Handle 表、residency、semantic memory、idle pipeline |
| **作业调度** | `orchestration/` | Workflow DAG、Loop/Classify/Tournament、Reduce |
| **信号** | `signals/` | 路由到 context signals 分区 |
| **ABI** | `runtime/kernel.rs` | `KernelInput` / `KernelAction` / `KernelObservation` |

## L* 执行循环（单任务 turn 内）

`LoopPhase` 描述 **一个 Running 任务** 在 turn 内的步骤（与 `TaskLifecycle` 正交）：

```text
Reason   →  kernel 渲染 Context，返回 CallLLM
Act      →  模型 tool_calls → ExecuteTools / Spawn / LoadWorkflow
Observe  →  SDK 回灌 provider_result、tool_results
Delta    →  压力评估、压缩、renewal、capability 更新
```

`TaskLifecycle` 描述 **任务是否可被调度**：

```text
Ready → Running → Suspended → Done
```

- **Suspended**：携 `WaitReason` —— `Approval`（治理 AskUser 人工审批）或 `SubAgentJoin`（阻塞等待子任务 join）
- **Done**：携 `TerminationReason`（对应进程视图 `ProcessState::{Joined, Failed}`）

## Workflow 是第二维调度

单 agent run 是 **一条 TCB 链**；`WorkflowRun` 在根任务上叠加 **DAG**：

```text
WorkflowSpec
  node0 (explore) ──┐
  node1 (explore) ──┼──► node3 (plan, reducer=synthesize)
  node2 (verify)  ──┘
```

每个节点 spawn = 一次 `Syscall::Spawn` + 子 TCB；`depends_on` 未满足时内核 **挂起** 该节点，不消耗 LLM。

运行时 `SubmitNodes` / `LoadWorkflow` = 动态扩展 DAG，仍过 `max_workflow_nodes` quota。

## 宿主循环（RuntimeRunner）

SDK 侧是经典的 **解释器循环**：

```python
# 概念性伪代码 — 见 python/deepstrike/runtime/runner.py
while not done:
    action = kernel_step(runtime, observations)
    match action.kind:
        case "call_llm":
            async for ev in provider.stream(rendered_context, tools):
                yield ev
            feed provider_result to kernel
        case "execute_tools":
            results = await plane.execute(tool_calls)
            feed tool_results to kernel
        case "spawn_sub_agent":
            result = await orchestrator.run(spec)
            feed sub_agent_result to kernel
        case "awaiting_resume":
            break
```

每次 feed 产生 `KernelObservation` → 写入 `SessionLog`（可重放证据链）。

## Context：不是 chat log，是 VM

`ContextManager` 维护：

| 分区 | 内容 |
|------|------|
| system | Identity、稳定 system prompt |
| knowledge | Skill 正文、`initial_memory`、宿主钉住的耐久参考（键控 + 边界驱逐 + 预算） |
| history | 对话 turns + 运行时 memory/knowledge 检索命中 + 预取（可压缩、可 frozen prefix） |
| state | task_state、signals、plan、recent_actions footer |

`mm::HandleTable`：大 tool result 以 handle 引用，按 **Residency** page-in/out，避免单条 message 撑爆窗口。

## 最小示例

```python
from deepstrike import RuntimeRunner, RuntimeOptions, InMemorySessionLog, LocalExecutionPlane

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=32_000,
))

async for event in runner.run("分析仓库结构"):
    ...  # SDK 事件；内核在后台 stepping
```

## 延伸阅读

- [执行模型](./execution-model) — 逐步 trace
- [Kernel ABI](./kernel-abi)
- [Context 工程](../guides/context-engineering)
