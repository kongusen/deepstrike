# Concepts

Conceptual documentation for the runtime model.

| Document | Covers |
| --- | --- |
| [Agent OS (0.2.6+)](./agent-os.md) | Kernel mediation, native profile defaults, spool, memory syscalls, OS snapshots |
| [Dynamic Workflows (0.2.11+)](./dynamic-workflows.md) | The six harness patterns as kernel nodes, runtime DAG-append (`SubmitNodes`), deterministic `Reduce` nodes, trust/quarantine, `output_schema`, budget-as-signal, and resume |
| [Core Concepts](./core-concepts.md) | Skills, memory, knowledge, harnesses, signals, collaboration, and safety |
| [Context Slots and Compression](./context-slots-compression.md) | Four-slot context layout, Layer-1 spool, compression tiers, renewal, and renderer behavior |

Start with [Agent OS](./agent-os.md) for the capability overview, then [Dynamic Workflows](./dynamic-workflows.md) for the orchestration model, and drill into Core Concepts for feature-specific behavior.
