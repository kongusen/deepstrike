# Structured Output

When Agents hand work to each other, `output_schema` requires an agent node to produce a value
matching a JSON Schema subset. To combine several nodes' outputs with deterministic code, write an
ordinary function in a host program (`runDynamicWorkflow`); no special node type is needed.

**Code entry points**:

- `python/deepstrike/runtime/output_schema.py`
- `crates/deepstrike-core/src/orchestration/workflow/mod.rs`
- `python/deepstrike/runtime/runner.py`

## What structured results give your Agents

| Need | Behavior |
| --- | --- |
| Reliable handoffs | `output_schema` validates a specialist's result before another Agent consumes it. |
| Recovery from mistakes | A schema mismatch can be returned as feedback for a bounded retry. |
| Stable dependencies | Downstream Agents receive predictable structured input. |

Together these features turn “ask the Agent for a structure” into a reliable handoff: validate, retry, and merge with ordinary application code.

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

## Level 2: Generate → Evaluate → Retry

`gen_eval` combines a worker and a verifier:

```python
from deepstrike import gen_eval

spec = gen_eval(
    implement="Write a CSV parser",
    evaluate="Verify quote, escape, and empty-cell handling",
)
```

Internally this is one worker node plus one verify node. `AttemptLoop` drives retry with feedback; the verdict structure pairs with `verdict_output_schema()`.

## Runtime and SDK Responsibilities

| Behavior | Owner |
|----------|-------|
| carrying schema on node descriptor | runtime |
| injecting schema instruction | SDK |
| JSON extraction and validation | SDK |
| retry prompt | SDK |

## Common Issues

| Issue | Handling |
|-------|----------|
| model returns markdown fence | `extract_json_value` attempts fenced JSON extraction |
| model adds prose around JSON | attempts to slice first object / array |
| schema uses `oneOf` / `pattern` | lightweight validator ignores unknown keywords |

## Verification Entry Points

- `python/tests/test_output_schema.py`
- `python/tests/test_workflow_control_flow.py`
- `node/tests/output-schema.test.ts`
