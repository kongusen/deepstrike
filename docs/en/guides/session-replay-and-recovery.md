# Agent Sessions, Replay & Recovery

An Agent needs to remember what it is doing and continue after interruption. SessionLog retains model output, tool requests and results, compression, permissions, collaboration, memory, and workflow events for each run as an explainable session record. It supports:

- **Diagnostics**: fold OS Snapshots or inspect historical workflow projections offline
- **Audit**: filter important events by kernel primitive
- **Reproduction**: replay model output via provider replay / ReplayProvider

Production recovery is a separate path: the application persists an opaque logical checkpoint and canonical KernelJournal, then restores by installing the checkpoint and replaying only the bounded tail. SessionLog explains, audits, and tests a run; it never reconstructs production control state.

**Code entry points**:

- `python/deepstrike/runtime/session_log.py`
- `python/deepstrike/runtime/session_repair.py`
- `python/deepstrike/runtime/provider_replay.py`
- `python/deepstrike/runtime/replay_provider.py`
- `python/deepstrike/runtime/replay_fixture.py`
- `python/deepstrike/runtime/os_snapshot.py`

## Agent continuity

| Responsibility | Description |
|----------------|-------------|
| Event log | SessionLog is the append-only evidence stream for a run |
| Recovery | Canonical checkpoints and KernelJournal records restore complete logical state; SessionLog has no state authority |
| Audit | Events can be filtered by runtime primitive to locate important decisions |
| Reproduction | provider replay / ReplayProvider makes tests independent of live model calls |
| Operations | OS Snapshot summarizes session events into dashboard-ready state |

SessionLog is the Agent's evidence chain. KernelJournal and logical checkpoints own production recovery. The former explains, reproduces, and supports operations; the latter holds the logical state required to continue.

![Session Replay & Recovery Mechanisms](/session_replay_mechanisms.svg)

## Level 1: Choose a SessionLog

In-memory development log:

```python
from deepstrike import InMemorySessionLog

session_log = InMemorySessionLog()
```

Local durable JSONL:

```python
from deepstrike import FileSessionLog

session_log = FileSessionLog("./sessions")
```

`FileSessionLog` is sequentially safe within one instance. Multi-process writes to the same session need an external lock or database-backed `SessionLog`.

## Level 2: Pin session_id

```python
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
))

async for event in runner.run("fix payment bug", session_id="pay-bug-42"):
    ...

events = await session_log.read("pay-bug-42")
```

If `session_id` is omitted, the SDK creates a new id. Pass one explicitly to correlate checkpoint/journal state, audit evidence, or RunGroup membership.

## Level 3: Read and Filter Events

```python
events = await session_log.read("pay-bug-42")
latest = await session_log.latest_seq("pay-bug-42")

memory_events = await session_log.read(
    "pay-bug-42",
    primitive_filter="memory",
)
```

Common events:

| kind | Purpose |
|------|---------|
| `run_started` / `run_terminal` | run lifecycle and multimodal-attachment audit projections; authoritative restore state lives in the logical checkpoint |
| `llm_completed` | assistant text, tool_calls, provider_replay |
| `tool_requested` / `tool_completed` | tool evidence |
| `compressed` / `context_renewed` | Context VM compression and renewal |
| `tool_gated` / `permission_requested` / `permission_resolved` | permission path |
| `agent_process_changed` | sub-agent lineage |
| `workflow_node_completed` / `workflow_nodes_submitted` | workflow audit and offline diagnostic projections |
| `memory_written` / `memory_queried` / `memory_validation_failed` | memory syscalls |

## Level 4: Provider Replay

Provider replay handles assistant messages that are not just text: native blocks, reasoning details, or stateful response ids. SDKs store replayable envelopes in `llm_completed.provider_replay`.

```python
from deepstrike.runtime.provider_replay import seed_provider_replay_from_events

events = await session_log.read("pay-bug-42")
seed_provider_replay_from_events(provider, events)
```

Compatibility is checked by provider descriptor:

- same protocol → seed replay
- different protocol → skip the envelope and use neutral transcript
- provider has no replay hooks → no-op

## Level 5: Offline Tests with ReplayProvider

Extract recorded assistant messages:

```python
from deepstrike import ReplayProvider, ReplayProviderOpts, extract_recorded_messages

events = await session_log.read("pay-bug-42")
messages = extract_recorded_messages(events)
provider = ReplayProvider(ReplayProviderOpts(messages=messages))
```

Pass `provider` to `RuntimeRunner` to test runtime behavior, tool execution, governance, and workflow driving with fixed assistant outputs.

## Level 6: Repair Bad Events Offline

Old logs may miss `token_count` or contain oversized replay text:

```python
from deepstrike.runtime.session_repair import repair_events_for_recovery

events = await session_log.read("pay-bug-42")
repaired = repair_events_for_recovery(events, max_bytes=100_000)
```

`repair_events_for_recovery` retains its historical name as an offline log-repair helper; it neither emits canonical inputs nor restores a kernel. It:

- sanitizes `llm_completed.content`
- backfills `token_count`
- preserves original `provider_replay`
- never synthesizes provider-specific replay shapes

## Level 7: Workflow Recovery Boundary

`workflow_node_completed` and `workflow_nodes_submitted` are audit projections, not production
recovery inputs. Callers must not scan SessionLog to assemble completed nodes or reapply dynamic
nodes. The logical checkpoint scheduler partition owns the complete DAG and node runtime state and
restores it together with the bounded journal tail.

## Level 8: OS Snapshot

`rebuild_os_snapshot_from_session_events` folds session events into a status summary:

```python
from deepstrike.runtime.os_snapshot import rebuild_os_snapshot_from_session_events

events = [e.event for e in await session_log.read("pay-bug-42")]
snapshot = rebuild_os_snapshot_from_session_events(events)
print(snapshot.process_by_agent)
print(snapshot.tool_gated_count)
```

Use it for dashboards / debug views. It is not a replacement for the canonical Kernel Checkpoint.

## Boundaries

| Capability | Guaranteed by SessionLog? |
|------------|---------------------------|
| append-only evidence chain | yes |
| strong multi-process write consistency | depends on your `SessionLog` implementation |
| provider-native replay | depends on provider descriptor / replay hooks |
| completed workflow-node recovery | no; logical checkpoint + KernelJournal own it |
| runtime append recovery | no; checkpoint workflow graph state owns it |
| rollback of filesystem / tool side effects | no; tools need idempotency or compensation |

## Verification Entry Points

- `python/tests/test_session_recovery.py`
- `python/tests/test_provider_replay.py`
- `python/tests/test_replay_fixture.py`
- `python/tests/test_workflow_resume.py`
- `node/tests/provider-replay.test.ts`
