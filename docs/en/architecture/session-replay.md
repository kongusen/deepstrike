# Session & Replay

Agent OS **replayability** comes from logical kernel checkpoints, canonical transaction records in the host-owned KernelJournal, and append-only SessionLog evidence, not from saving chat history alone.

## Recoverable boundary

```text
SessionLog (append-only evidence)
    +
opaque logical checkpoint + bounded KernelJournal tail
    +
Host stores (DreamStore, ArchiveStore, FileSessionLog)
```

The kernel never writes disk; the SDK owns I/O. The kernel emits checkpoint candidates, canonical records, and observations. SessionLog is audit and offline diagnostic evidence, not the production source of truth for reconstructing workflow graphs.

## SessionLog implementations

| Type | Use |
|------|-----|
| `InMemorySessionLog` | Dev / tests |
| `FileSessionLog` | Production |

Typical kinds: `run_started`, `tool_invoked`, `agent_process_changed`, `workflow_node_completed`, `memory_written`, `pressure_compact`.

## Wake / resume

Suspended when: AskUser, sub-agent join, workflow barrier.

```python
# The SDK restores the canonical kernel from its checkpoint + KernelJournal.
async for event in runner.run(goal, session_id=existing_id):
    ...
```

The host loads the latest installed checkpoint and records after it, then invokes canonical restore. The checkpoint owns the complete workflow DAG, node state, and pending effect identities, so restore cost is bounded by the tail rather than total run length. Runtime `SubmitNodes` extensions are restored from workflow graph state; production resume does not accept or synthesize workflow `resumed_*` inputs.

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
