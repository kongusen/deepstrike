# Choosing an API: run_agent vs RuntimeRunner vs run_fanout

## Decision Tree

```
Need streaming events / signals / memory / governance?
├─ No → Single task?
│        ├─ Yes → run_agent()
│        └─ No (parallel + synthesis) → run_fanout()
└─ Yes → RuntimeRunner
```

## Level 1: `run_agent` — Simplest

```python
from deepstrike import run_agent, AnthropicProvider, read_file

text = await run_agent(
    provider=AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"]),
    goal="List files in the current directory",
    tools=[read_file],
    max_turns=10,
)
```

Best for: HTTP handlers, scripts, one-off tasks.

## Level 2: `run_fanout` — Parallel + Synthesis

```python
from deepstrike import run_fanout, AnthropicProvider

result = await run_fanout(
    provider=AnthropicProvider(api_key=...),
    tasks=["Analyze module A", "Analyze module B", "Analyze module C"],
    synthesize="Merge the three analyses and give a conclusion",
    worker_role="explore",
    synthesis_role="plan",
)
print(result["synthesis"])
print(result["outputs"])  # per-node outputs
```

Internally builds a `WorkflowSpec` DAG and runs kernel-gated `run_workflow`.

## Level 3: `RuntimeRunner` — Full Capabilities

```python
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=32_000,
    # optional advanced features below
    skill_dir="./skills",
    dream_store=store,
    governance_policy=policy,
    signal_source=gateway,
    run_group=group,
))

async for event in runner.run(goal, session_id="my-session"):
    ...

# or explicit workflow
outcome = await runner.run_workflow(spec, session_id="wf-1")
```

Only `RuntimeRunner` supports:

- Skill / Memory / Knowledge
- Governance / ResourceQuota
- Signals / ReactiveSession
- Sub-agent / Milestones
- Harness retries

## Comparison Table

| Capability | run_agent | run_fanout | RuntimeRunner |
|------------|:---------:|:----------:|:-------------:|
| Streaming events | ✗ | ✗ | ✓ |
| Tools | ✓ | ✓ | ✓ |
| Workflow DAG | ✗ | ✓ (fixed template) | ✓ |
| Memory | ✗ | ✗ | ✓ |
| Governance | ✗ | ✗ | ✓ |
| Session resume | Limited | Limited | ✓ |

## Further Reading

- [Dynamic Workflows](/en/guides/workflow)
- [RuntimeOptions Reference](/en/reference/runtime-options)
