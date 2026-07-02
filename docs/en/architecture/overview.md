---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelInput, KernelAction, KernelObservation, TaskTable, Tcb, AgentProcess, TaskState]
---

# Kernel / Host Split

Under the [Agent OS](/en/architecture/agent-os) narrative, DeepStrike splits into **microkernel (`deepstrike-core`)** and **host SDK**. This page maps modules and OS analogies.

## Layer diagram

![Agent OS Architecture](/agent_os_architecture.svg)


| Layer | Does | Does not |
|-------|------|----------|
| **Kernel** | State machines, syscall adjudication, context render, workflow schedule, budgets | Network, disk, async LLM |
| **SDK** | LLM calls, tools, persistence, signals, memory store I/O | Reimplement spawn gates ad hoc |
| **Provider** | Vendor APIs, streaming, `RenderedContext` mapping | Governance decisions |
| **ExecutionPlane** | Tool registration/execution, worktree, remote sandbox | Context compression |

## Kernel modules ↔ Agent OS subsystems

| Subsystem | Module | Role |
|-----------|--------|------|
| **Syscall & security** | `syscall/`, `governance/` | Unified trap; permission, rate limit, veto |
| **Schedule & process** | `scheduler/`, `proc/` | L* loop, `TaskTable`, `Tcb`, `AgentProcess` view |
| **Context VM** | `context/` | Partitions, render, compression, skills, task_state |
| **Memory mgmt** | `mm/`, `memory/` | Handles, residency, semantic memory, idle pipeline |
| **Job scheduler** | `orchestration/` | Workflow DAG, control-flow nodes, Reduce |
| **Signals** | `signals/` | Route into context state partition |
| **ABI** | `runtime/kernel.rs` | KernelInput / Action / Observation |

## L* loop (intra-turn)

```text
Reason   →  render context, return CallLLM
Act      →  tool_calls → ExecuteTools / Spawn / LoadWorkflow
Observe  →  ingest provider_result, tool_results
Delta    →  pressure, compression, renewal
```

`TaskState` (Ready / Running / Blocked / Suspended / Done) is **orthogonal** — it describes schedulability, not turn phase.

## Workflow as second scheduling dimension

Each node spawn = `Syscall::Spawn` + child TCB. Unmet `depends_on` → kernel **waits** without burning LLM tokens.

Runtime `SubmitNodes` / `LoadWorkflow` extends the DAG under `max_workflow_nodes` quota.

## Host loop (RuntimeRunner)

```python
while not done:
    action = kernel_step(runtime, observations)
    # dispatch CallLLM / ExecuteTools / SpawnSubAgent / AwaitingResume
    # append KernelObservation → SessionLog
```

## Context VM (not a chat log)

| Partition | Contents |
|-----------|----------|
| system | Identity, stable system prompt |
| knowledge | Skill body, `initial_memory`, host-pinned durable refs (keyed + boundary eviction + budget) |
| history | Turns + runtime memory/knowledge retrieval hits + prefetch (compressible, frozen prefix) |
| state | task_state, signals, plan footer |

`mm::HandleTable` pages large tool results by residency.

## Further reading

- [Execution model](/en/architecture/execution-model)
- [Kernel ABI](/en/architecture/kernel-abi)
- [Context engineering](/en/guides/context-engineering)
