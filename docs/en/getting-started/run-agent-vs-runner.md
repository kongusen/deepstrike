# Choosing an API: Agent vs RuntimeRunner

Choose an entry point based on how much responsibility your Agent needs.

## Decision Tree

```
Need the standard executable Agent contract?
├─ Yes → create_agent() + agent.run()/agent.stream()
└─ Need custom host control → RuntimeRunner
```

## Level 1: `Agent` — Standard public contract

```python
from deepstrike import create_agent, AnthropicProvider, read_file

agent = create_agent(
    "reader",
    model="anthropic/claude",
    runtime_binding={"provider": AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"])},
    tools=[read_file],
)
result = await agent.run("List files in the current directory", max_turns=10)
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

It creates one focused Agent per task and returns both the synthesis and each worker's output.

## Level 3: `RuntimeRunner` — Full Capabilities

```python
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=32_000,
    # optional advanced features below
    skill_dir="./skills",
    memory_store=store,
    governance_policy=policy,
    signal_source=gateway,
    run_group=group,
))

async for event in runner.run(goal, session_id="my-session"):
    ...

# or explicit workflow
outcome = await runner.run_workflow(spec, session_id="wf-1")
```

Use `RuntimeRunner` when your Agent needs:

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
