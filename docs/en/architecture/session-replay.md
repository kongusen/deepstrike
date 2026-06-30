# Session & Replay

Agent OS **replayability** comes from serializable control-flow state in the kernel plus append-only SessionLog events from the host — not from saving chat history alone.

## Recoverable boundary

```text
SessionLog (append-only evidence)
    +
KernelSnapshot / event replay
    +
Host stores (DreamStore, ArchiveStore, FileSessionLog)
```

The kernel never writes disk; the SDK owns I/O. The kernel **emits** persistable observations.

## SessionLog implementations

| Type | Use |
|------|-----|
| `InMemorySessionLog` | Dev / tests |
| `FileSessionLog` | Production |

Typical kinds: `run_started`, `tool_invoked`, `agent_process_changed`, `workflow_node_completed`, `memory_written`, `pressure_compact`.

## Wake / resume

Suspended when: AskUser, sub-agent join, workflow barrier.

```python
async for event in runner.run(goal, session_id=existing_id):
    ...
```

Runtime `SubmitNodes` append is logged — resumed DAG includes dynamic extensions.

## Replay & deterministic tests

- `ReplayProvider` — fixed LLM output
- `rebuild_os_snapshot_from_events` — rebuild counters from log
- Audit events stripped when reconstructing provider messages

## Cross-links

- Compression → `ArchiveStore`, `frozen_prefix_len` — see [Prompt cache design](/en/concepts/prompt-cache-design)
- Multi-peer → [RunGroup budget](/en/concepts/run-group-budget)

## Further reading

- [Execution model](/en/architecture/execution-model)
- [Kernel ABI](/en/architecture/kernel-abi)
