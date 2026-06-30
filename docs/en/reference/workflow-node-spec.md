---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  python: [WorkflowNodeSpec, WorkflowSpec, KernelAgentRole, AgentIsolation, ContextInheritance]
---

# WorkflowNodeSpec Reference

A single node in a declarative workflow DAG. Definition: `python/deepstrike/types/agent.py`.

## Base fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `task` | `str \| dict` | — | Goal string, or `{goal, criteria?, lane?}` |
| `role` | `KernelAgentRole` | — | explore / plan / implement / verify / custom |
| `isolation` | `AgentIsolation` | `"shared"` | shared / read_only / worktree / remote |
| `context_inheritance` | `ContextInheritance` | `"none"` | none / system_only / full |
| `depends_on` | `list[int]` | `[]` | Dependent node indices |
| `model_hint` | `str \| None` | None | Host provider routing hint |
| `trust` | `NodeTrust` | `"trusted"` | trusted / quarantined |
| `token_budget` | `int \| None` | None | Sub-run cumulative token cap |
| `output_schema` | `dict \| None` | None | JSON Schema output validation |

## Control-flow fields (mutually exclusive)

| Field | Description |
|-------|-------------|
| `reducer` | Deterministic reduce node name; no LLM run |
| `loop` | `{"max_iters": N}` loop node |
| `classify` | `{"branches": [{"label", "nodes": [idx]}]}` |
| `tournament` | `{"entrants": [task, ...]}` tournament |

## Examples

### Plain node

```python
WorkflowNodeSpec(task="Research", role="explore", isolation="read_only")
```

### Task with criteria

```python
WorkflowNodeSpec(
    task={"goal": "Write tests", "criteria": ["Coverage > 80%"]},
    role="implement",
    depends_on=[0],
)
```

### Loop node

```python
WorkflowNodeSpec(
    task="Process each checklist item",
    role="implement",
    loop={"max_iters": 5},
    depends_on=[0],
)
```

### Reduce node

```python
WorkflowNodeSpec(
    task="Merge",
    role="plan",  # role has no substantive effect on reduce
    reducer="union",
    depends_on=[0, 1, 2],
)
```

## WorkflowSpec

```python
@dataclass
class WorkflowSpec:
    nodes: list[WorkflowNodeSpec]
```

Convert to kernel JSON: `workflow_spec_to_kernel(spec)`

## Runtime meta-tools

| Tool | Description |
|------|-------------|
| `submit_workflow_nodes` | Append nodes to in-progress DAG |
| `start_workflow` | Bootstrap or flatten entire spec |

## Built-in templates

| Function | Pattern |
|----------|---------|
| `fanout_synthesize(tasks, synthesize)` | N × explore → plan |
| `generate_and_filter(tasks, filter_goal)` | N × implement → verify |
| `verify_rules(rules, synthesize)` | N × verify → plan |

## Further reading

- [Workflow](/en/guides/workflow)
- Kernel: `crates/deepstrike-core/src/orchestration/workflow/`
