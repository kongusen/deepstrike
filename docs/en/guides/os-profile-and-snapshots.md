# Agent Runtime Policy & State Snapshots

Runtime policy determines how an Agent handles signals, permissions, and resources by default. State snapshots turn SessionLog events into an observable run summary for dashboards, debugging, and operations. `OsProfile` and `OS Snapshot` are the existing API names for these two capabilities.

**Code entry points**:

- `python/deepstrike/runtime/os_profile.py`
- `python/deepstrike/runtime/os_snapshot.py`
- `node/src/runtime/os-profile.ts`
- `node/src/runtime/os-snapshot.ts`
- `node/src/runtime/kernel-primitives-dashboard.ts`

## What your application can configure and observe

| Responsibility | Description |
|----------------|-------------|
| Profile | Packages attention, governance, and other defaults into application-selectable runtime configuration |
| Validation | Checks declarative policy before startup so invalid configuration does not reach the runtime |
| Snapshot | Folds runtime state from SessionLog instead of relying on in-memory transient objects |
| Dashboard | Converts runtime events into health, queue, permission, and process state for the UI |

`OsProfile` answers "which default policy starts the Agent?" `OS Snapshot` answers "what state did this run reach?" The first sets boundaries; the second supports observability.

![OS Profile & Snapshots Mechanisms](/snapshots_mechanisms.svg)

## Level 1: Use the Native Profile

```python
from deepstrike import RuntimeOptions, RuntimeRunner, os_profile

profile = os_profile("native")

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    os_profile=profile,
))
```

`native` defaults:

| Policy | Default |
|--------|---------|
| SignalPolicy | `queue_max=64` |
| GovernancePolicy | `pattern="*" action="allow"` |

This is a basic runnable default, not a production safety boundary.

## Level 2: Validate a Profile

```python
from deepstrike import assert_native_profile

profile = assert_native_profile("native")
```

`validate_declarative_policy` checks:

- governance rules must be a list
- rule pattern must be string
- action must be `allow` / `deny` / `ask_user`
- signal `queue_max` must be a positive integer; optional `ttl_ms` must also be positive

## Level 3: Custom OsProfile

```python
from deepstrike import GovernancePolicy, GovernancePolicyRule, OsProfile
from deepstrike.runtime.os_profile import SignalPolicy

profile = OsProfile(
    id="review-safe",
    signal_policy=SignalPolicy(queue_max=32, ttl_ms=60_000),
    governance_policy=GovernancePolicy(
        default_action="ask_user",
        rules=[
            GovernancePolicyRule(pattern="read_*", action="allow"),
            GovernancePolicyRule(pattern="write_*", action="ask_user"),
            GovernancePolicyRule(pattern="run_*", action="deny"),
        ],
    ),
)
```

Pass it via `RuntimeOptions(os_profile=profile)` and the SDK lowers it into kernel config.

## Level 4: OS Snapshot

Build a runtime summary from SessionLog events:

```python
from deepstrike.runtime.os_snapshot import rebuild_os_snapshot_from_session_events

events = [entry.event for entry in await session_log.read("session-1")]
snapshot = rebuild_os_snapshot_from_session_events(events)

print(snapshot.last_suspend)
print(snapshot.process_by_agent)
print(snapshot.budget_exceeded)
```

Snapshot fields:

| Field | Source event |
|-------|--------------|
| `last_suspend` | `suspended` |
| `last_resumed_turn` | `resumed` |
| `process_by_agent` | `agent_process_changed` |
| `budget_exceeded` | `budget_exceeded` |
| `signals` | `signal_delivery_disposed` |
| `page_out_count` / `page_in_count` | memory paging |
| `tool_gated_count` | `tool_gated` |
| memory counters | `memory_*` events |

## Level 5: Check Event Category Completeness

```python
from deepstrike.runtime.os_snapshot import session_log_has_required_categories

events = [entry.event for entry in await session_log.read("session-1")]
assert session_log_has_required_categories(events)
```

This verifies runtime events carry correct `category` and `primitive`, useful before CI or dashboard ingest.

## OS Snapshot vs Recoverable Checkpoint

| Name | Purpose | Can restore execution? |
|------|---------|------------------------|
| OS Snapshot | observed summary folded from SessionLog | no |
| Kernel Checkpoint | opaque logical state, digests, and a bounded journal tail | yes, for exact wake / replay |
| ContextSnapshot | context partition snapshot | partially, for context restore |

`OS Snapshot` is for humans and monitoring. `Kernel Checkpoint` is for runtime recovery. It neither serializes private state-machine structs nor stores the complete accepted-input history or a derived planned step; it stores logical state by transition/P1/P2/P3 owner and verifies state/tail digests. Applications manage it through candidate -> persist -> covered-head CAS install -> ack, then restore only the bounded tail and journal records after the checkpoint.

## Production Practices

1. Do not use `native` profile as your production safety boundary.
2. Default write / deploy / shell tools to `ask_user` or `deny`.
3. Check `session_log_has_required_categories` before dashboard ingest.
4. Build OS Snapshot periodically for long runs and watch `tool_gated_count`, paging, and memory validation failures.
5. Combine Profile with `ResourceQuota`: profile governs policy; quota governs resources.
6. Size the checkpoint tail count/byte soft watermark and hard limit for the recovery objective; the hard limit returns retryable `CheckpointRequired` without accepting the input or setting a permanent overflow latch.

## Verification Entry Points

- `python/tests/test_os_profile.py`
- `python/tests/test_os_snapshot.py`
- `node/tests/os-profile.test.ts`
- `node/tests/kernel-primitives-dashboard.test.ts`
