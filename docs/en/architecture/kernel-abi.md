---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelInput, KernelObservation, KernelRuntime, Syscall, Disposition]
  python: [RenderedContext, MemoryPolicy, ResourceQuota, SchedulerPolicy]
---

# Kernel ABI

The Kernel ABI is the stable boundary between **host and Agent OS microkernel** — analogous to the user/kernel syscall interface. Version: `KERNEL_ABI_VERSION = 1`.

## Design intent

| Principle | Meaning |
|-----------|---------|
| **Versioned** | Every `KernelInput` carries `version` |
| **Event-driven** | Host appends events; never mutates kernel structs directly |
| **Observable** | Decisions emit `KernelObservation` to SessionLog |
| **Language-neutral** | Same ABI for Py / Node / WASM |

## Three message kinds

```text
Host ──KernelInput──► Kernel
Host ◄──KernelAction── Kernel
Host ◄──KernelObservation── Kernel  (audit / replay)
```

### KernelInput (host → kernel)

Grouped by Agent OS subsystem:

| Subsystem | kind | Purpose |
|-----------|------|---------|
| Schedule | `start_run`, `provider_result`, `tool_results`, `sub_agent_result` | Turn loop |
| Syscall feedback | `permission_resolved`, `milestone_result` | Resume from Gate |
| Governance | `load_governance_policy`, `set_resource_quota` | Policy install |
| Workflow | `load_workflow` | Install DAG |
| Memory | `write_memory`, `query_memory`, `set_memory_policy` | Long-term memory |
| Context | `signal` | External inject |
| Recovery | resume / snapshot events | Rebuild from log |

### KernelAction (kernel → host)

| action | Host must |
|--------|-----------|
| `CallLLM` | Call provider with `RenderedContext` + tools |
| `ExecuteTools` | Run ExecutionPlane |
| `SpawnSubAgent` | Run orchestrator with `AgentRunSpec` |
| `AwaitingResume` | Stop stepping until external event |

### KernelObservation → SessionLog

Facts for replay: `tool_denied`, `agent_process_changed`, `workflow_node_completed`, `pressure_compact`, etc.

## Relation to Syscall

Wire events converge internally to `Syscall` + `Disposition` — **one gate** for tools, spawn, memory, DAG growth.

## Python binding

```python
from deepstrike.runtime.kernel_step import kernel_apply

observations: list[dict] = []
kernel_apply(runtime, observations, {"kind": "write_memory", "memory": {...}})
```

## Further reading

- [Execution model](/en/architecture/execution-model)
- [Session & replay](/en/architecture/session-replay)
- Source: `crates/deepstrike-core/src/runtime/kernel.rs`
