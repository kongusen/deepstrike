# RunGroup Budget

`RunGroup` is a host-SDK governance domain: it lets multiple stateless runs / persona sessions share **cumulative tokens, cumulative sub-agent spawns, and lineage**. The kernel remains a pure state machine; cross-run durable ledger state lives in `GroupBudgetStore`.

Main implementation entry points:

- `python/deepstrike/runtime/run_group.py`
- `node/src/runtime/run-group.ts`
- `python/deepstrike/runtime/runner.py`
- `node/src/runtime/runner.ts`
- `crates/deepstrike-core/src/scheduler/state_machine/gate.rs`
- `crates/deepstrike-core/src/governance/quota.rs`

## Why RunGroup Lives in the SDK

DeepStrike kernels can be created and destroyed by stateless request handlers. If one logical task is carried by multiple sessions, personas, or workflow bootstraps, cumulative budget cannot live inside a single kernel instance.

The implementation is split:

| Layer | Responsibility |
|-------|----------------|
| SDK `RunGroup` | Store group id, read/write cumulative ledger, register members |
| Kernel seed | Receive `group_tokens_base` / `group_spawns_base` at boot |
| Kernel gate | Check token cap, spawn quota, workflow growth inside this vehicle |
| SDK charge | Write this run's consumption back to the group ledger at terminal |

## Data Model

Python:

```python
@dataclass
class GroupLedger:
    tokens_spent: int = 0
    subagents_spawned: int = 0

@dataclass
class GroupMember:
    session_id: str
    role: str | None = None

@dataclass
class RunGroup:
    id: str
    budget_store: GroupBudgetStore
```

Node uses camelCase:

```ts
interface GroupLedger {
  tokensSpent: number
  subagentsSpawned: number
}

interface RunGroup {
  id: string
  budgetStore: GroupBudgetStore
}
```

## Lifecycle: seed → run → charge

```text
runner.run(session_id)
        │
        ├─ join(group_id, GroupMember(session_id, agent_id))
        ├─ ledger = budget_store.read(group_id)
        ├─ kernel configure_run:
        │    group_tokens_base = ledger.tokens_spent
        │    group_spawns_base = ledger.subagents_spawned
        │
        ▼
kernel run
        │
        ├─ SchedulerBudget.max_total_tokens checks:
        │    group_tokens_base + local total_tokens
        ├─ ResourceQuota.max_total_subagents checks:
        │    group_spawns_base + local_subagents_spawned()
        │
        ▼
run terminal
        │
        └─ budget_store.charge(group_id, tokens=total_tokens, subagents=spawned)
```

The next member starts with all previous members' cumulative spend.

## Cumulative vs Instantaneous Budget

RunGroup shares only **cumulative** budget, not instantaneous concurrency state.

| Budget | Accumulates across RunGroup? | Reason |
|--------|------------------------------|--------|
| `SchedulerBudget.max_total_tokens` | yes | SDK seeds `group_tokens_base`; kernel evaluates cumulative tokens |
| `ResourceQuota.max_total_subagents` | yes | SDK seeds `group_spawns_base`; kernel evaluates cumulative spawns |
| `ResourceQuota.max_concurrent_subagents` | no | running children exist only in the current vehicle's `TaskTable` |
| `ResourceQuota.max_spawn_depth` | no | structural constraint of the current spawn lineage |
| `memory_writes_per_window` | no | uses current kernel observed clock / write timestamps |
| `max_workflow_nodes` | current workflow | evaluated against active `WorkflowRun` node count |

This boundary matters: cross-process "how much has been spent" can be folded from a ledger; cross-process "how many are running right now" requires an external scheduler and is not a kernel fact.

## Standalone Workflow

`RuntimeRunner.run_workflow(...)` bootstraps a workflow kernel when there is no active parent run. With a RunGroup it:

1. Registers the standalone workflow session as a group member.
2. Seeds the kernel from the group ledger.
3. On completion, charges the envelope kernel's `local_subagents_spawned()` back as subagents.

Each workflow node counts on the spawn axis because each scheduled node is a child proc in the kernel `TaskTable`.

## ReactiveSession

`ReactiveSession` places multiple persona runners in one logical session. All peer runners should receive the same `RunGroup`:

```python
session = ReactiveSession(
    run_group=group,
    make_runner=lambda pid, shared: RuntimeRunner(RuntimeOptions(
        ...,
        run_group=shared["run_group"],
    )),
    ...
)
```

Membership is recorded as `GroupMember(session_id, role)`, so lineage can be queried across personas and invocations.

## Budget Stores

| Implementation | Location | Use case |
|----------------|----------|----------|
| `InMemoryGroupBudgetStore` | Python / Node | single-process development and tests |
| `SessionLogGroupBudgetStore` | Python / Node | persist ledger and membership to `SessionLog`, rebuildable across store instances |

`SessionLogGroupBudgetStore` persists by folding events:

| event kind | Purpose |
|------------|---------|
| `group_budget_charged` | add tokens / subagents |
| `group_member_joined` | register session membership, idempotent by session_id |

## Configuration Example

```python
from deepstrike import (
    InMemoryGroupBudgetStore,
    ResourceQuota,
    RunGroup,
    RuntimeOptions,
    RuntimeRunner,
)

store = InMemoryGroupBudgetStore()
group = RunGroup(id="incident-42", budget_store=store)

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    run_group=group,
    max_total_tokens=100_000,
    resource_quota=ResourceQuota(max_total_subagents=12),
))
```

Multiple runners share a cumulative governance domain when they use the same `RunGroup.id` and shared store.

## Common Misreadings

| Misreading | Actual implementation |
|------------|-----------------------|
| the kernel persists the RunGroup ledger | ledger lives in SDK `GroupBudgetStore`; the kernel only receives seed values |
| `max_concurrent_subagents` applies across all members | it only sees running child tasks in the current kernel |
| completed sub-agents free `max_total_subagents` | no; total is cumulative |
| standalone workflow does not count against group spawns | it does; completion charges workflow node count |
| budgets break without RunGroup | no; behavior falls back to N=1 per-run budget |

## Verification Entry Points

- `python/tests/test_run_group_budget.py`
- `node/tests/run-group-budget.test.ts`
- `python/tests/test_workflow_drive.py`
- `node/tests/e2e/composition.test.ts`

## Further Reading

- [Signals & Reactive](/en/guides/signals-and-reactive)
- [Governance](/en/guides/governance)
- [Workflow](/en/guides/workflow)
