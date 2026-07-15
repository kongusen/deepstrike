# OS Profile & Runtime Snapshots

OS Profile is a host-selectable set of default governance policies: attention queue, governance policy, and native profile validation. OS Snapshot is a status summary folded from SessionLog events for dashboards, debugging, and operations.

**Code entry points**:

- `python/deepstrike/runtime/os_profile.py`
- `python/deepstrike/runtime/os_snapshot.py`
- `node/src/runtime/os-profile.ts`
- `node/src/runtime/os-snapshot.ts`
- `node/src/runtime/kernel-primitives-dashboard.ts`

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| Profile | Packages attention, governance, and other defaults into host-selectable runtime configuration |
| Validation | Checks declarative policy before startup so invalid config does not reach the kernel |
| Snapshot | Folds runtime state from SessionLog instead of relying on in-memory transient objects |
| Dashboard | Converts kernel primitives into health, queue, permission, and process state for the UI |

OS Profile answers "which policy starts the Agent OS?" OS Snapshot answers "what state did the Agent OS reach?" The first sets boundaries; the second supports observability.

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
| AttentionPolicy | `max_queue_size=64` |
| GovernancePolicy | `pattern="*" action="allow"` |

This is a basic "kernel primitives enabled" default, not a production safety boundary.

## Level 2: Validate a Profile

```python
from deepstrike import assert_native_profile

profile = assert_native_profile("native")
```

`validate_declarative_policy` checks:

- governance rules must be a list
- rule pattern must be string
- action must be `allow` / `deny` / `ask_user`
- attention `max_queue_size` must be a positive integer

## Level 3: Custom OsProfile

```python
from deepstrike import GovernancePolicy, GovernancePolicyRule, OsProfile
from deepstrike.runtime.os_profile import AttentionPolicy

profile = OsProfile(
    id="review-safe",
    attention_policy=AttentionPolicy(max_queue_size=32),
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
| `spool_count` | `large_result_spooled` |
| `tool_gated_count` | `tool_gated` |
| memory counters | `memory_*` events |

## Level 5: Check Event Category Completeness

```python
from deepstrike.runtime.os_snapshot import session_log_has_required_categories

events = [entry.event for entry in await session_log.read("session-1")]
assert session_log_has_required_categories(events)
```

This verifies kernel events carry correct `category` and `primitive`, useful before CI or dashboard ingest.

## OS Snapshot vs Kernel Snapshot

| Name | Purpose | Can restore execution? |
|------|---------|------------------------|
| OS Snapshot | observed summary folded from SessionLog | no |
| KernelSnapshotV2 | accepted ABI transactions plus validation metadata | yes, for exact wake / replay |
| ContextSnapshot | context partition snapshot | partially, for context restore |

OS Snapshot is for humans and monitoring. `KernelSnapshotV2` is for runtime recovery. It does not serialize private state-machine structs: restore deterministically replays the public ABI and verifies lifecycle, operation, step/effect identity, and the terminal latch. Node exposes `snapshotKernelRuntime` / `restoreKernelRuntime`; Python exposes `snapshot_kernel_runtime` / `restore_kernel_runtime`. Configure the bound with `kernelReliability.snapshotInputLimit` or `KernelReliability.snapshot_input_limit`.

## Production Practices

1. Do not use `native` profile as your production safety boundary.
2. Default write / deploy / shell tools to `ask_user` or `deny`.
3. Check `session_log_has_required_categories` before dashboard ingest.
4. Build OS Snapshot periodically for long runs and watch `tool_gated_count`, `spool_count`, and memory validation failures.
5. Combine Profile with `ResourceQuota`: profile governs policy; quota governs resources.
6. Size the snapshot input limit to the recovery window. Once exceeded, snapshot creation fails explicitly with `snapshot_incompatible` instead of emitting a partial checkpoint.

## Verification Entry Points

- `python/tests/test_os_profile.py`
- `python/tests/test_os_snapshot.py`
- `node/tests/os-profile.test.ts`
- `node/tests/kernel-primitives-dashboard.test.ts`
