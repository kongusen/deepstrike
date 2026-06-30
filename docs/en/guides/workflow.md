# Dynamic Workflows

Dynamic workflows are the Agent OS **Process Scheduler**. They decompose a goal into schedulable sub-agent process graphs, and every spawn, append, branch, and reduce step passes through kernel state machines and governance quotas.

**Source code:**
- Kernel: `crates/deepstrike-core/src/orchestration/`, `scheduler/state_machine/workflow.rs`
- SDK: `python/deepstrike/types/agent.py`, `runtime/workflow_control_flow.py`

---

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| Process scheduling | Each `WorkflowNodeSpec` maps to an isolatable sub-agent run |
| Dependency management | DAG edges define the ready queue; blocked nodes do not run |
| Dynamic growth | `submit_workflow_nodes` / `start_workflow` append nodes through syscalls |
| Control flow | Loop / Classify / Tournament mutate the active subgraph instead of relying on prompt convention |
| Governance | `max_workflow_nodes`, spawn depth, role, and isolation can be trapped by the kernel |

A workflow is not just an SDK helper. It is the Agent OS orchestration layer: the host supplies providers and tools, while the kernel keeps graph growth controlled and recoverable.

![Dynamic Workflow Mechanisms](/workflow_mechanisms.svg)

## Concept

```
WorkflowSpec
  └── WorkflowNodeSpec[]  # each node = one sub-agent run
        ├── task / role / isolation
        ├── depends_on[]  # DAG edges
        ├── loop / classify / tournament / reducer  # control flow
        └── submit_workflow_nodes / start_workflow  # runtime extension
```

---

## Level 1: `run_fanout` out of the box

```python
from deepstrike import run_fanout, AnthropicProvider

result = await run_fanout(
    provider=AnthropicProvider(api_key=...),
    tasks=["Research A", "Research B", "Research C"],
    synthesize="Merge the three reports and recommend next steps",
    worker_role="explore",
    synthesis_role="plan",
)
print(result["synthesis"])
```

Equivalent to a DAG of 3 explore nodes plus 1 plan synthesis node. Under the hood:

```python
# python/deepstrike/runtime/facade.py
spec = WorkflowSpec(
    nodes=[WorkflowNodeSpec(task=t, role=worker_role) for t in tasks]
    + [WorkflowNodeSpec(task=synthesize, role=synthesis_role, depends_on=list(range(len(tasks))))],
)
```

---

## Level 2: Explicit `WorkflowSpec`

```python
from deepstrike import WorkflowSpec, WorkflowNodeSpec, RuntimeRunner

spec = WorkflowSpec(nodes=[
    WorkflowNodeSpec(task="Research competitors", role="explore", isolation="read_only"),
    WorkflowNodeSpec(task="Write implementation plan", role="plan", depends_on=[0]),
    WorkflowNodeSpec(task="Implement", role="implement", depends_on=[1]),
])

outcome = await runner.run_workflow(spec, session_id="wf-1")
print(outcome["completed"])   # ['wf-node0', 'wf-node1', 'wf-node2']
print(outcome["outputs"])     # final text per node
```

---

## Level 3: Built-in templates

```python
from deepstrike import fanout_synthesize, generate_and_filter, verify_rules

# Parallel explore → plan synthesis
fan = fanout_synthesize(["a", "b", "c"], "merge results")

# Parallel implement → verify filter
gen = generate_and_filter(["x", "y"], "dedupe by rules")

# Parallel verify per rule → optional skeptic summary
ver = verify_rules(["rule1", "rule2"], "skeptic summary")
```

Templates carry kernel role defaults (e.g. verifiers use `read_only` + `context_inheritance="none"` for bias resistance).

---

## Level 4: Control-flow nodes

### Loop node

```python
WorkflowNodeSpec(
    task="Process checklist items one by one",
    role="implement",
    loop={"max_iters": 5},
    depends_on=[0],
)
```

The agent can end early by outputting `{"loop_continue": false}`. SDK helpers: `loop_instruction()`, `extract_loop_continue()`.

### Classify node

```python
WorkflowNodeSpec(
    task="Classify user intent",
    role="plan",
    classify={
        "branches": [
            {"label": "bug", "nodes": [1, 2]},
            {"label": "feature", "nodes": [3]},
        ]
    },
)
```

Agent returns `{"branch": "bug"}` → kernel runs that branch and prunes the rest.

### Tournament node

```python
WorkflowNodeSpec(
    task="Pick the best approach",
    role="verify",
    tournament={"entrants": ["Approach A description", "Approach B description"]},
)
```

Parallel entrant generation → pairwise judge → winner selected.

### Runtime dynamic extension

Agents can call meta-tools during a run:

| Tool | Behavior |
|------|----------|
| `submit_workflow_nodes` | Append nodes to an in-flight DAG |
| `start_workflow` | Top-level: bootstrap a new DAG; inside workflow: flatten to parent DAG |

Governed by `Syscall::SubmitNodes` / `LoadWorkflow`; `max_workflow_nodes` quota prevents runaway growth.

Top-level agents **auto-pivot** via `start_workflow`: bootstrap a new kernel-driven workflow, then resume the original reason loop when done.

### Reduce node (no LLM)

```python
WorkflowNodeSpec(
    task="Merge outputs",
    reducer="union",  # or a custom reducer
    depends_on=[0, 1, 2],
)
```

Register custom reducers: `RuntimeOptions(reducers={**builtin_reducers(), "my_merge": fn})`

---

## Kernel behavior

- Workflow driver spawns isolated sub-agents per node with role/isolation defaults
- DAG edges gate readiness; control-flow nodes mutate the active subgraph
- Syscall traps enforce node count and spawn depth quotas

---

## Further reading

- [WorkflowNodeSpec reference](/en/reference/workflow-node-spec)
- [Sub-Agents & Collaboration](./sub-agents-and-collaboration)
- [Structured Output & Reducers](./structured-output-and-reducers)
- [Provider Routing](./provider-routing)
- [RunGroup budget](/en/concepts/run-group-budget)
- Test: `python/tests/test_workflow_drive.py`
