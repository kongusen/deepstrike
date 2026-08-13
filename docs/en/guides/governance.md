# Governance

Governance gives an Agent clear boundaries. You can allow, deny, or pause tool calls; constrain arguments; cap delegation and memory writes; and make approval a normal part of the run.

**Source code:** `crates/deepstrike-core/src/governance/`, `python/deepstrike/governance.py`

---

## Decisions your application can make

| Decision | Example |
| --- | --- |
| Allow or deny | Hide `publish_public` from an Agent that may only draft content. |
| Ask for approval | Pause `email_editor` until a person approves it. |
| Constrain arguments | Require a path, enum, or numeric range. |
| Limit resources | Cap turns, tokens, child Agents, workflow nodes, or memory writes. |
| Cancel safely | Stop a run or child Agent when the application no longer needs it. |

Governance decisions are visible to the Agent and recorded with the run, so the model can adapt and the application can explain what happened.

![Agent governance decision flow](/governance_pipeline.svg)

## Concept

| Mechanism | Description |
|-----------|-------------|
| Permission | allow / deny / ask_user |
| Veto | Hard-ban tool list |
| Rate limit | Sliding-window call cap |
| Constraint | Parameter required / enum / range |
| ResourceQuota | Sub-agent concurrency, depth, memory write frequency |
| Sandbox | Sub-agent isolation profile |

```python
# python/deepstrike/governance.py
@dataclass
class GovernancePolicy:
    default_action: GovernancePolicyAction | None = None
    rules: list[GovernancePolicyRule] = field(default_factory=list)
    vetoes: list[str] = field(default_factory=list)
    rate_limits: list[GovernanceRateLimit] = field(default_factory=list)
    constraints: list[dict[str, Any]] = field(default_factory=list)
    surface_denied_in_system: bool = True
```

---

## Level 1: Declarative policy

```python
from deepstrike import GovernancePolicy, GovernancePolicyRule, GovernanceRateLimit

policy = GovernancePolicy(
    default_action="ask_user",
    rules=[
        GovernancePolicyRule(pattern="write_*", action="deny"),
        GovernancePolicyRule(pattern="read_*", action="allow"),
    ],
    vetoes=["dangerous_tool"],
    rate_limits=[
        GovernanceRateLimit(tool="search", max_calls=10, window_ms=60_000),
    ],
)

RuntimeOptions(..., governance_policy=policy)
```

When action is `ask_user`, the kernel emits `PermissionRequestEvent`; resolve it via `on_permission_request`.

---

## Level 2: Parameter constraints

```python
policy = GovernancePolicy(
    constraints=[
        {"kind": "required", "tool": "write_file", "path": "path"},
        {"kind": "enum", "tool": "set_mode", "path": "mode", "values": ["read", "write"]},
        {"kind": "range", "tool": "resize", "path": "size", "min": 1, "max": 1000},
    ],
)
```

Constraints are lowered to kernel events via `governance_policy_to_kernel_event()`.

---

## Level 3: ResourceQuota

```python
from deepstrike import ResourceQuota, MemoryWriteRateLimit

RuntimeOptions(
    ...,
    resource_quota=ResourceQuota(
        max_concurrent_subagents=3,
        max_total_subagents=20,
        max_spawn_depth=2,
        memory_writes_per_window=MemoryWriteRateLimit(max_writes=5, window_ms=60_000),
    ),
)
```

With `RunGroup`, spawn counts accumulate across stateless runs — see [RunGroup budget](/en/concepts/run-group-budget).

---

## Level 4: Syscall trap

Workflow growth goes through kernel syscalls:

- `SubmitNodes { count }` — append nodes
- `AppendWorkflowNodes { nodes }` — extend the DAG after the kernel derives the caller

When `max_workflow_nodes` is exceeded, the kernel commits a `control_request_rejected` observation.
Nothing executed, so nothing is rolled back. Top-level `runWorkflow` returns the reason through
`WorkflowOutcome.rejection`; a rejected in-flight node submission fails the submitting node so the
root agent receives the real outcome.

### I5: Schema pre-filter

When `GovernancePolicy.surface_denied_in_system=True` (default), the runner pre-filters denied tools and surfaces the deny list in the system prompt:

```python
# python/deepstrike/governance.py
def governance_filter_schema(tools: list, policy: GovernancePolicy | None) -> tuple[list, list[str]]:
    """Bucket tools into (allowed, denied) per the policy."""
    ...
```

## Stateful decision hooks

`on_tool_call` is a pre-execution decision boundary. A thrown hook fails closed by default and the
tool call returns `governance_denied`; only purely advisory hooks should explicitly set
An `on_tool_call` exception always denies execution. `on_tool_result` runs after the tool side effect and therefore keeps
observer/enrichment failure isolation—it cannot retroactively claim that the tool did not execute.

---

## Runtime behavior

- Every tool call and workflow syscall is evaluated before execution
- Tool denials commit error tool results to context so the model sees its attempt and why it failed
- Allowed sibling calls in the same batch continue executing
- Rate limits use sliding windows per tool id

---

## Further reading

- [Sub-Agents & Collaboration](./sub-agents-and-collaboration) — sandbox / isolation
- [Execution Plane & Tools](./execution-plane-and-tools) — where tools execute and how audit callbacks are emitted
- [OS Profile & Runtime Snapshots](./os-profile-and-snapshots) — profile, policy, and dashboard state
- Test: `python/tests/test_resource_quota.py`
