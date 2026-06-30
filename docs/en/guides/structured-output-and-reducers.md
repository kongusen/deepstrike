# Structured Output & Reducers

DeepStrike workflow supports two mechanisms that reduce LLM usage and improve control:

- `output_schema`: require an agent node to produce a value matching a JSON Schema subset
- `Reduce` node: run deterministic host code instead of an LLM to combine dependency outputs

**Code entry points**:

- `python/deepstrike/runtime/output_schema.py`
- `python/deepstrike/runtime/reducers.py`
- `crates/deepstrike-core/src/orchestration/workflow/mod.rs`
- `python/deepstrike/runtime/runner.py`

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| To workflows | Turns node output into validated data, not just natural language |
| To providers | SDK injects schema instruction and retry; the kernel carries the contract |
| To the host | Reduce nodes run deterministic reducers without model calls |
| To dependents | Schema failure blocks dependent nodes; reducer output can continue as stable input |

This plane turns "ask the LLM for a structure" into an executable OS contract: validate, retry, fail, and merge with deterministic code.

![Reducers & Output Validation Mechanisms](/reducers_mechanisms.svg)

## Level 1: Add output_schema

```python
from deepstrike import WorkflowNodeSpec

schema = {
    "type": "object",
    "required": ["title", "risks"],
    "properties": {
        "title": {"type": "string"},
        "risks": {
            "type": "array",
            "items": {"type": "string"},
        },
    },
}

node = WorkflowNodeSpec(
    task="Analyze proposal risks and return structured JSON",
    role="verify",
    output_schema=schema,
)
```

The kernel only carries the schema. The SDK:

1. appends `schema_instruction(schema)` to the node goal
2. extracts a JSON value from agent output
3. validates with `validate_against_schema`
4. retries with `schema_retry_instruction` on failure
5. fails the node if it still does not conform, starving dependents

## Supported JSON Schema Subset

`output_schema.py` supports the common structured-output subset:

| keyword | Support |
|---------|---------|
| `type` | object / array / string / number / integer / boolean / null |
| `required` | required object properties |
| `properties` | recursive object field validation |
| `items` | recursive array item validation |
| `enum` | enum values |

Unknown keywords are ignored. This is a lightweight SDK validator, not a full JSON Schema engine.

## Level 2: Reduce Node

A Reduce node is host compute and does not call the model:

```python
from deepstrike import WorkflowNodeSpec

reduce_node = WorkflowNodeSpec(
    task="merge risk lists",
    reducer="merge_json_arrays",
    depends_on=[0, 1, 2],
)
```

Built-in reducers:

| reducer | Behavior |
|---------|----------|
| `concat` | concatenate dependency outputs |
| `dedupe_lines` | dedupe by line |
| `merge_json_arrays` | extract JSON values, merge arrays, dedupe |
| `count` | count non-empty outputs |

## Level 3: Register a Custom Reducer

```python
from deepstrike import RuntimeOptions, builtin_reducers

def top_risks(inputs: list[dict]) -> str:
    return "\n".join(i["output"] for i in inputs[:3])

runner = RuntimeRunner(RuntimeOptions(
    ...,
    reducers={**builtin_reducers, "top_risks": top_risks},
))
```

Custom reducers should be pure functions: same input, same output, no network / file / LLM I/O. If you need I/O, use a normal workflow node.

## Level 4: Schema + Reducer

```python
spec = WorkflowSpec(nodes=[
    WorkflowNodeSpec(
        task="List module A risks as a JSON array",
        role="verify",
        output_schema={"type": "array", "items": {"type": "string"}},
    ),
    WorkflowNodeSpec(
        task="List module B risks as a JSON array",
        role="verify",
        output_schema={"type": "array", "items": {"type": "string"}},
    ),
    WorkflowNodeSpec(
        task="merge risks",
        reducer="merge_json_arrays",
        depends_on=[0, 1],
    ),
])
```

Verifier nodes use LLMs for structured findings; merge consumes no model tokens.

## Level 5: Generate → Evaluate → Retry

`gen_eval` combines an implementation loop and verifier:

```python
from deepstrike import gen_eval

spec = gen_eval(
    implement="Write a CSV parser",
    evaluate="Verify quote, escape, and empty-cell handling",
    max_iters=3,
)
```

Internally this uses a loop worker + verify node. Verdict structure can pair with `verdict_output_schema()`.

## Kernel / Host Boundary

| Behavior | Owner |
|----------|-------|
| carrying schema on node descriptor | kernel |
| injecting schema instruction | SDK |
| JSON extraction and validation | SDK |
| retry prompt | SDK |
| reducer node scheduling and dependencies | kernel |
| reducer function execution | SDK |

## Common Issues

| Issue | Handling |
|-------|----------|
| model returns markdown fence | `extract_json_value` attempts fenced JSON extraction |
| model adds prose around JSON | attempts to slice first object / array |
| schema uses `oneOf` / `pattern` | lightweight validator ignores unknown keywords |
| reducer not found | node fails; check `RuntimeOptions.reducers` |
| reducer needs file access | use a normal tool / workflow node instead |

## Verification Entry Points

- `python/tests/test_output_schema.py`
- `python/tests/test_workflow_reduce.py`
- `python/tests/test_workflow_control_flow.py`
- `node/tests/output-schema.test.ts`
