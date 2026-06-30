# What is Agent OS?

DeepStrike is not "another LLM wrapper." It is an **Agent operating-system microkernel**: the kernel decides **when, whether, and within what budget** to run agent nodes; the host SDK **actually executes** LLM calls and tool I/O.

The point is not branding. It is an engineering boundary: once an agent can author plans, call tools, spawn sub-agents, write memory, and append workflow nodes, it needs a governed control plane. DeepStrike implements that control plane as a microkernel.

## Starting from dynamic workflows

On hard tasks, models increasingly **author a small harness** — spawn sub-agents with isolated contexts and focused goals instead of planning and executing in one window.

In IDE scripts that harness is often **volatile** (one-off JS/Python). DeepStrike **kernelizes** it:

```text
LLM produces a structured plan (WorkflowSpec / submit_workflow_nodes)
        │
        ▼
deepstrike-core  ──  schedule: gated spawn · budgets · trust · snapshots
        │
        ▼
Host SDK  ──  run providers, tools, worktrees, DreamStore, webhooks
```

**Control flow in the kernel, I/O in the host** — the core split of Agent OS.

![Agent OS Architecture](/agent_os_architecture.svg)

## Narrative model: turn harnesses into kernel objects

A temporary harness usually does four jobs at once: hold context, choose the next step, execute tools, and record results. That works for demos. Long-running tasks break down because those responsibilities live inside one script process.

Agent OS separates them:

| Responsibility | Owner | Why |
|----------------|-------|-----|
| Plan and control flow | Kernel | Must be replayable, governed, and consistent across languages |
| LLM / tool / file / network I/O | Host SDK | Needs real environment access, credentials, and async runtimes |
| Evidence chain | SessionLog | Needed for recovery, audit, debugging, and evals |
| Long context | Context VM | Needs compression, page-in/out, and prompt-cache boundaries |

The minimal mental model is: **the agent requests effects from user space, the kernel decides whether they are allowed, and the host executes them and feeds observations back**.

## Why "OS" and not "framework library"?

Traditional agent frameworks scatter loop, memory, and governance across SDKs. DeepStrike borrows OS vocabulary:

| OS concept | Agent OS | Code |
|------------|----------|------|
| **Syscall** | Every side effect enters the trap: `Invoke`, `Spawn`, `WriteMemory`, `SubmitNodes`… | `syscall/` |
| **Process** | Each agent run is a **TCB**; sub-agents are child tasks | `scheduler/tcb.rs`, `proc/` |
| **Scheduler** | `TaskState` + `BudgetLedger` | `scheduler/` |
| **Virtual memory** | Context partitions + handle table + page-in/out | `context/`, `mm/` |
| **Signals / IPC** | `RuntimeSignal` into state partition; ReactiveSession blackboard | `signals/` |
| **Security module** | Governance: Allow / Deny / Gate / RateLimited | `governance/` |
| **Job scheduler (DAG)** | Workflow nodes through same gate; Reduce nodes at zero tokens | `orchestration/workflow/` |

The kernel **does not** open sockets, read files, or call LLMs. Agent OS is not a replacement for a real operating system; it explicitly models the side-effect boundary, scheduling entities, context memory, and audit trail of agent work.

In one sentence: **the OS analogy constrains responsibility; it does not expand it**.

## Three primitives

These three pieces are the minimal kernel surface for Agent OS. Higher-level features should reduce to syscall, TCB / scheduler, and Context VM composition instead of becoming special host-side branches.

![L* Loop & Syscall Trap](/agent_os_loop_flow.svg)


### P1 — Syscall trap (single side-effect boundary)

```rust
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

Each tool call, spawn, and DAG append yields a unified `Disposition`.

### P2 — TCB + scheduler

Root agent and every sub-agent / workflow node share one `TaskTable`. `AgentProcess` is a **derived view** over child TCBs — not a second source of truth.

### P3 — Context VM

Context is partitioned address space — not an append-only chat log. Compression, handles, and capability manifests live here.

## Six harness patterns as first-class nodes

![Dynamic Workflow Orchestration DAG](/agent_os_workflow_dag.svg)


| Pattern | Kernel primitive | SDK template |
|---------|------------------|--------------|
| **Classify-and-act** | `classify` node | `classify_instruction()` |
| **Fan-out-and-synthesize** | N workers → synthesize barrier | `fanout_synthesize()` |
| **Adversarial verification** | isolated verify TCBs | `verify_rules()` |
| **Generate-and-filter** | N implement → verify barrier | `generate_and_filter()` |
| **Tournament** | parallel entrants → judge bracket | `tournament` node |
| **Loop until done** | `loop` + `SubmitNodes` | `loop_instruction()` |

See [Dynamic workflows](/en/guides/workflow).

## Beyond the blog post: kernel-enforced mechanisms

| Mechanism | Behavior |
|-----------|----------|
| **Quarantine + taint** | Quarantined nodes cannot escalate; their submitted nodes stay quarantined |
| **Reduce nodes** | Deterministic merge/dedupe — no LLM |
| **output_schema** | SDK validates JSON; failures retry then starve dependents |
| **WorkflowBudget** | Spawns carry remaining budget for adaptive fan-out |
| **model_hint** | Host `provider_for` routes models per node |

## Script vs kernel: what you gain

| Property | Script harness | Agent OS |
|----------|----------------|----------|
| Replayable | Ad-hoc state | SessionLog + KernelSnapshot |
| Governed | Manual if/else | Unified syscall gate + audit |
| Resumable | Often restart from scratch | wake/resume + dynamic DAG nodes |
| Cross-language | One runtime | Same `deepstrike-core` → Py/Node/WASM |
| I/O ownership | Mixed | Kernel pure; SDK owns I/O |

## Next steps

- [Kernel / host split](/en/architecture/overview)
- [Execution model](/en/architecture/execution-model)
- [Hello Agent](/en/getting-started/hello-agent)
