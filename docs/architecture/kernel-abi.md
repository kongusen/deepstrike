---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelInput, KernelObservation, KernelRuntime, Syscall, Disposition]
  python: [RenderedContext, MemoryPolicy, ResourceQuota, SchedulerPolicy]
---

# Kernel ABI

Kernel ABI 是 **宿主与 Agent OS 微内核** 之间的稳定边界——类似用户态与内核态的 syscall 接口。版本：`KERNEL_ABI_VERSION = 1`（`crates/deepstrike-core/src/runtime/kernel.rs`）。

## 设计意图

| 原则 | 说明 |
|------|------|
| **Versioned** | 每个 `KernelInput` 带 `version` 字段 |
| **Event-driven** | SDK 只 append event，不直接 mutate 内核 struct |
| **Observable** | 每个决策产出 `KernelObservation` 进 SessionLog |
| **Language-neutral** | 同一 ABI 绑定 Py / Node / WASM |

内核 **never** 在 ABI 里携带 provider API key 或文件路径——那些是宿主配置。

## 三类消息

```text
SDK ──KernelInput(event)──► Kernel
SDK ◄──KernelAction────────── Kernel   （下一步做什么）
SDK ◄──KernelObservation────── Kernel   （审计 / replay）
```

### KernelInput（宿主 → 内核）

```json
{
  "version": 1,
  "event": { "kind": "start_run", "goal": "..." }
}
```

按 Agent OS 子系统分类的 event kind：

| 子系统 | kind | 用途 |
|--------|------|------|
| **调度** | `start_run` | 创建根 TCB，进入 Reason |
| **调度** | `provider_result` | LLM 响应（text + tool_calls） |
| **调度** | `tool_results` | 工具执行结果 |
| **调度** | `sub_agent_result` | 子 agent / workflow 节点完成 |
| **Syscall 回灌** | `permission_resolved` | AskUser 人工裁决 |
| **Syscall 回灌** | `milestone_result` | 里程碑 verifier 结果 |
| **治理** | `load_governance_policy` | 安装 GovernanceConfig |
| **治理** | `set_resource_quota` | spawn / memory write 配额 |
| **Workflow** | `load_workflow` | 安装 DAG |
| **Memory** | `write_memory` / `query_memory` | 长期记忆 syscall |
| **Memory** | `set_memory_policy` | 校验与 retrieval 策略 |
| **Context** | `signal` | 外部信号注入 |
| **恢复** | `resume` / snapshot events | 从 SessionLog 重建 |

### KernelAction（内核 → 宿主）

宿主 **必须** 执行或显式挂起；不可静默丢弃。

| action | 宿主行为 |
|--------|----------|
| `CallLLM` | `provider.stream(RenderedContext, tools)` |
| `ExecuteTools` | `ExecutionPlane.execute(calls)` |
| `SpawnSubAgent` | `SubAgentOrchestrator.run(AgentRunSpec)` |
| `Synthesize` | idle pipeline 等需 LLM 的内核请求 |
| `AwaitingResume` | 停止 stepping，等待外部 event |

`CallLLM` 携带的 `RenderedContext` 是 Context VM 的 **序列化视图**；`tools` 已过 governance 预过滤（若启用 I5）。

### KernelObservation（内核 → SessionLog）

可 JSON 序列化、可 replay 的 **事实记录**：

- `tool_invoked` / `tool_denied`
- `agent_process_changed`
- `workflow_batch_spawned` / `workflow_node_completed`
- `memory_written` / `memory_queried`
- `pressure_compact` / `prefix_invalidated`
- `governance_denied`

Replay 时 SDK 用 observation 流 **重建** `KernelRuntime`，而非重跑 LLM。

## RunConfig _bundle

一次 run 的「OS 策略」通过 event 注入：

| 结构 | Agent OS 含义 |
|------|---------------|
| `GovernanceConfig` | syscall 默认策略 |
| `MemoryPolicy` | WriteMemory 校验规则 |
| `ResourceQuota` | spawn 深度、并发、DAG 节点上限 |
| `SchedulerPolicy` | 版本化 DAG 调度权重；不混入墙钟预算 |

Python 侧多数通过 `RuntimeOptions` 在 boot 时转为 kernel events（见 [RuntimeOptions 参考](../reference/runtime-options)）。

## 与 Syscall 的关系

ABI event 是 **wire format**；内核内部统一收敛为 `Syscall` + `Disposition`：

```text
tool_results 里的每个 call  ← 已 Allow 的 Invoke
spawn 请求                  ← Spawn
write_memory event          ← WriteMemory
```

未来 milestone 也会走 trap，保持 **one gate** 叙事。

## Python 绑定

```python
from deepstrike.runtime.kernel_step import kernel_apply, kernel_action

observations: list[dict] = []
runtime = KernelRuntime(...)
kernel_apply(runtime, observations, {"kind": "write_memory", "memory": {...}})

for obs in observations:
    session_log.append(session_id, obs)
```

## 延伸阅读

- [执行模型](./execution-model) — action 在 turn 中的顺序
- [Session 与重放](./session-replay)
- 源码：`crates/deepstrike-core/src/runtime/kernel.rs`
