# RunGroup Budget

`RunGroup` is the host SDK's cross-run governance domain. Stateless runs, persona sessions, and standalone workflows can share cumulative tokens, sub-agent spawns, loop rounds, and lineage. `GroupBudgetStore` owns cross-run state; the kernel receives only the reservation grant admitted for the current vehicle.

## The single budget protocol

```text
join member
    │
    ▼
reserve(limits, requested) ──► reservation_id + granted capacity
    │
    ▼
configure_run.budget_grant
    │
    ▼
kernel local enforcement
    │
    ▼
budget_usage_reported(reservation_id, actual usage)
    │
    ▼
settle(reservation_id, actual) / release(reservation_id) before terminal
```

The former `group_tokens_base`, `group_spawns_base`, `group_rounds_base`, host `read/charge` fallback, and `SessionLogGroupBudgetStore` paths are removed. Runners do not infer settlement from `LoopResult` or `local_subagents_spawned()`.

## Responsibility boundary

| Component | Responsibility |
|-----------|----------------|
| `GroupBudgetStore` | Atomically account for settled + held capacity, reserve, idempotently settle/release, and keep member lineage |
| SDK runner | Reserve before `start_run`, configure the kernel grant, and settle only a correlated usage report |
| Kernel | Enforce the local grant and correlate exceeded/usage events with operation and reservation identities |

A store must implement `join`, `members`, `reserve`, `settle`, and `release`. Multi-replica implementations should use a Redis script, database transaction, or equivalent atomic primitive. A generic append-only session log has no CAS and is not a reservation store.

## Budget axes

| Axis | RunGroup behavior |
|------|-------------------|
| `max_total_tokens` | Reserve token capacity; the grant becomes this run's effective cumulative limit |
| `max_total_subagents` | Reserve spawn capacity; ordinary spawns and workflow nodes use the same kernel gate |
| loop `max_rounds` | Each loop vehicle requests one round, preventing concurrent oversubscription |
| `max_concurrent_subagents` | Instantaneous current-kernel limit; not cross-vehicle |
| `max_spawn_depth` | Structural current-lineage limit |
| `memory_writes_per_window` | Current-kernel observed-clock rate limit |

An unrequested axis is omitted from `budget_grant`, meaning that admission does not constrain that axis. It does not mean zero capacity. The kernel may still report actual usage for accounting. An explicit `tokens = 0` grant is the opposite case — it is an admission error: the kernel rejects `configure_run` with an `InvalidConfig` fault whose message carries the `reservation_id`, rather than silently running a tool-less wrap-up round.

## Standalone workflows

With no active parent, `runWorkflow()` creates a real kernel run. The SDK reserves first and starts the workflow. When the DAG finishes, it sends `complete_run`; the kernel then emits the ordinary `done` effect and one correlated `budget_usage_reported`. Workflow-node usage therefore comes from the kernel TaskTable rather than host-side counting.

## Nested vehicles

Child runs derived by the `SubAgentOrchestrator` (spawned sub-agents and workflow nodes) still join the parent run's RunGroup, but the join only keeps member lineage and settles actual usage into the group ledger at terminal state; it reserves no budget axis (tokens, subagents, or rounds). RunGroup admission governs peer vehicles only — top-level runs, persona sessions, and LoopDriver rounds. Parent-child derivation is an internal affair of a single vehicle.

A nested vehicle's limits are enforced locally by the kernel policy's `maxTotalTokens` and `resourceQuota` (including the spawn gate's concurrency and depth checks), consistent with the sub-agent model of independent execution over the parent's tool and skill surface.

## Example

```python
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

`InMemoryGroupBudgetStore` is intended for one process and tests. Production deployments should provide a durable implementation of the same reservation contract.

Verification: `python/tests/test_run_group_budget.py`, `node/tests/run-group-budget.test.ts`, and `python/tests/test_workflow_drive.py`.
