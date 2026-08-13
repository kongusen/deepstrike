# Concept Index

Concepts explain the design choices that affect how Agents behave. They sit between capability guides and the API reference.

If [Agent Process Runtime](/en/architecture/agent-process-runtime) explains the overall runtime shape, Concepts answer:

- Why do root Agents, sub-agents, and workflow nodes share one process tree?
- Which fields define a sub-agent's privilege boundary?
- Why is Context not a chat log?
- Why does prompt cache need a frozen prefix?
- How can several Agent runs share a cumulative budget?

## Recommended Reading

| Page | Main code entry | What it covers |
|------|-----------------|----------------|
| [Agent Process Runtime](/en/architecture/agent-process-runtime) | `scheduler/tcb.rs`, `scheduler/wait_index.rs`, `runtime/kernel/wire/` | How process trees, durable waits, authority, budgets, IPC, supervision, and recovery form one runtime |
| [Roles & Isolation](/en/concepts/roles-and-isolation) | `types/agent.rs`, `orchestration/workflow/`, `scheduler/tcb.rs` | How role, isolation, capability, and trust become executable kernel constraints |
| [Prompt Cache Design](/en/concepts/prompt-cache-design) | `context/renderer.rs`, `context/manager.rs`, `mm/handle.rs` | How four-slot rendering, state_turn, handle projection, and frozen prefix protect cache reuse |
| [RunGroup Budget](/en/concepts/run-group-budget) | `python/deepstrike/runtime/run_group.py`, `node/src/runtime/run-group.ts`, `scheduler/state_machine/gate.rs` | How multiple stateless runs share one cumulative token / spawn governance domain |

## How This Differs From Architecture

| Layer | Focus |
|-------|-------|
| Architecture | How Agent Process Runtime processes, scheduling, effects, and recovery fit together |
| Concepts | Why a capability behaves a certain way and which fields control it |
| Guides | How to use the mechanism in real workflows |
| Reference | Full type, option, and event-field details |

## Code Facts First

Concept pages follow three rules:

1. **Agent behavior is the source of truth**: examples and public types describe what developers can rely on.
2. **Application responsibilities are explicit**: model calls, tools, filesystems, SessionLog, and RunGroup stores stay with the integrating application.
3. **Defaults are documented**: default roles, default inheritance, default budgets, and default cache behavior affect observable results.
