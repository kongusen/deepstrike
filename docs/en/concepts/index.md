# Concept Index

Concepts explain the **design concepts** that directly affect DeepStrike behavior in code. They sit below the architecture pages and above API reference.

If [Architecture](/en/architecture/) explains the overall Agent OS shape, Concepts answer:

- Which fields define a sub-agent's privilege boundary?
- Why is Context not a chat log?
- Why does prompt cache need a frozen prefix?
- Why does RunGroup live in SDK storage instead of kernel persistence?

## Recommended Reading

| Page | Main code entry | What it covers |
|------|-----------------|----------------|
| [Roles & Isolation](/en/concepts/roles-and-isolation) | `types/agent.rs`, `orchestration/workflow/`, `scheduler/tcb.rs` | How role, isolation, capability, and trust become executable kernel constraints |
| [Prompt Cache Design](/en/concepts/prompt-cache-design) | `context/renderer.rs`, `context/manager.rs`, `mm/handle.rs` | How four-slot rendering, state_turn, handle projection, and frozen prefix protect cache reuse |
| [RunGroup Budget](/en/concepts/run-group-budget) | `python/deepstrike/runtime/run_group.py`, `node/src/runtime/run-group.ts`, `scheduler/state_machine/gate.rs` | How multiple stateless runs share one cumulative token / spawn governance domain |

## How This Differs From Architecture

| Layer | Focus |
|-------|-------|
| Architecture | Why DeepStrike is an Agent OS microkernel and how kernel / host split responsibilities |
| Concepts | How one mechanism is represented in code, which fields are sources of truth, and what the host executes |
| Guides | How to use the mechanism in real workflows |
| Reference | Full type, option, and event-field details |

## Code Facts First

Concept pages follow three rules:

1. **Core types are the source of truth**: Rust `deepstrike-core` defines kernel semantics.
2. **Host responsibilities are explicit**: LLM calls, tools, filesystems, SessionLog, and RunGroup stores are SDK work.
3. **Defaults are documented**: default roles, default inheritance, default budgets, and default cache behavior affect observable results.

