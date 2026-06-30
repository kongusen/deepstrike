# Architecture Overview

DeepStrike architecture is framed around **Agent OS**: elevating dynamic workflow harnesses from volatile scripts into **replayable, governed, cross-language** kernel primitives.

This is not an attempt to emulate a real operating system. It is a runtime boundary for agents: **the kernel owns control flow; the host owns side effects**. The model may plan, call tools, spawn sub-agents, write memory, and grow a workflow, but those requests first become syscalls that the kernel adjudicates, accounts for, and records before an SDK executes anything.

## Recommended Reading Order

| # | Doc | You'll learn |
|---|-----|--------------|
| 1 | [What is Agent OS?](/en/architecture/agent-os) | Problem, vs scripts, six harness patterns |
| 2 | [Kernel / Host Split](/en/architecture/overview) | OS analogy, three primitives, module map |
| 3 | [Execution Model](/en/architecture/execution-model) | One turn from syscall to LLM |
| 4 | [Kernel ABI](/en/architecture/kernel-abi) | SDK ↔ kernel message contract |
| 5 | [Session & Replay](/en/architecture/session-replay) | Why runs are recoverable and auditable |

## One-line pitch

> **LLM writes a harness plan → kernel schedules nodes (gated · budgeted · replayable) → host SDK runs I/O.**

```text
┌─────────────────────────────────────────────────────────────┐
│  Your app (HTTP handler · CLI · IDE plugin · automation bot) │
└───────────────────────────┬─────────────────────────────────┘
                            │ RuntimeRunner / run_agent
┌───────────────────────────▼─────────────────────────────────┐
│  Host SDK (Python · Node · Rust · WASM)                        │
│  Provider · ExecutionPlane · SessionLog · DreamStore · Signal  │
└───────────────────────────┬─────────────────────────────────┘
                            │ KernelInput / KernelAction
┌───────────────────────────▼─────────────────────────────────┐
│  deepstrike-core — Agent OS microkernel                        │
│  Syscall trap · TCB/Scheduler · Context VM · Workflow DAG      │
└─────────────────────────────────────────────────────────────┘
```

## Architecture Thesis

Complex agents usually fail because the **control plane** is ad hoc, not because they lack one more wrapper around an LLM API:

| Control-plane question | If left in scripts | Agent OS approach |
|------------------------|-------------------|-------------------|
| Who may trigger side effects? | Each SDK / example decides locally | Every effect enters one syscall trap |
| Who may spawn sub-agents? | Harness creates new clients directly | TCB + TaskTable + quota / trust gates |
| Who owns the context window? | Append messages, then truncate | Context VM partitions, handles, compression, renewal |
| Who proves what happened? | Logs and orchestration state are scattered | SessionLog + KernelSnapshot |
| Who keeps languages consistent? | Python / Node each implement a loop | One `deepstrike-core` drives multiple hosts |

So DeepStrike is not primarily about "calling an LLM." It is a **governed control plane** for long-running agent work.

## What Agent OS fixes

Long-horizon single-context agents hit three failure modes (Anthropic *dynamic workflows*):

| Failure | Meaning | Agent OS structural fix |
|---------|---------|-------------------------|
| **Agentic laziness** | Stops after partial progress | Per-node TCB + token budget; Loop with hard `max_iters` |
| **Self-preferential bias** | Favors own output when judging | Verifier/judge in isolated TCB, no author context |
| **Goal drift** | Constraints lost after compaction | Persistent `task_state` + directives channel |

A JavaScript harness works — but orchestration state is **not serializable, not uniformly governed, not portable across languages**. Agent OS puts **control flow** in the kernel and **I/O** in the host.

## How to read the OS analogy

| OS term | Meaning in DeepStrike |
|---------|-----------------------|
| Kernel | Pure state machine; decides next action, performs no network / file / LLM I/O |
| Syscall | Tool, spawn, memory, workflow-growth, and other side-effect requests |
| Process / TCB | Scheduling entity for a root run, sub-agent, or workflow node |
| Scheduler | Advances tasks under budgets, dependencies, and suspension states |
| Virtual memory | Context partitions, handle table, page-in/out, compression |
| Security | Governance, permissions, quarantine, rate limits |

The analogy exists to make the boundary concrete: **agent syscalls must be governed, agent processes must be resumable, and agent memory must be compressible and reconstructable**.

## Architecture vs guides

| Architecture concept | User-facing feature | Guide |
|---------------------|---------------------|-------|
| Context VM | Four-slot render, compression, prompt cache | [Context engineering](/en/guides/context-engineering) |
| Syscall + governance | Tool policy, quotas, quarantine | [Governance](/en/guides/governance) |
| Workflow DAG | fan-out, classify, loop, tournament | [Dynamic workflows](/en/guides/workflow) |
| Memory syscall | writeMemory / queryMemory / Dream | [Memory](/en/guides/memory) |
| AgentProcess / TCB | Sub-agents, isolation, handoff | [Sub-agents & collaboration](/en/guides/sub-agents-and-collaboration) |

Architecture explains **why and shape**; guides explain **how with examples**.

## Code entry points

| Component | Path |
|-----------|------|
| Kernel | `crates/deepstrike-core/` |
| Python SDK | `python/deepstrike/` |
| Node SDK | `crates/deepstrike-node/` |
| Example | `python/examples/hello_agent/` |

## Design principles

1. **Pure computation** — kernel has zero I/O and zero async
2. **State machine driven** — host feeds events; kernel returns actions
3. **One gate for all effects** — tools, spawn, memory, DAG growth share one syscall trap
4. **Host-owned side effects** — network, disk, LLM APIs live only in the SDK
