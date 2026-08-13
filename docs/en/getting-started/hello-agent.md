# Hello Agent

Run your first tool-using Agent in five minutes. This example gives an Agent a model, one file-reading tool, streaming output, and a session.

## Code

```python
import asyncio
import os
from deepstrike import (
    AnthropicProvider,
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    read_file,
    TextDelta,
    ToolCallEvent,
    ToolResultEvent,
    DoneEvent,
)

async def main(goal: str):
    provider = AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"])
    plane = LocalExecutionPlane().register(read_file)
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=200_000,
        max_turns=10,
    ))

    async for event in runner.run(goal):
        if isinstance(event, TextDelta):
            print(event.delta, end="", flush=True)
        elif isinstance(event, ToolCallEvent):
            print(f"\n[→ {event.name}]")
        elif isinstance(event, ToolResultEvent):
            print(f"[← {event.content[:80]}...]")
        elif isinstance(event, DoneEvent):
            print(f"\n[done in {event.iterations} turns]")

asyncio.run(main("Read README.md and summarize"))
```

## Run

```bash
cd python
pip install -e .
ANTHROPIC_API_KEY=sk-... python examples/hello_agent/main.py "Read README.md and summarize"
```

## What the Agent does

1. The Agent receives the goal and the `read_file` capability.
2. The model decides whether it needs the file and requests the tool.
3. The application runs the tool and returns its result to the Agent.
4. The Agent uses the result to write the answer while events stream to the application.
5. The session ends with a `DoneEvent` that includes the run summary.

## Simpler Alternative

If you do not need streaming events:

```python
from deepstrike import run_agent, AnthropicProvider

text = await run_agent(
    provider=AnthropicProvider(api_key=...),
    goal="Summarize README.md",
)
print(text)
```

## Next Steps

- [Choosing an API](./run-agent-vs-runner)
- [Context Engineering](/en/guides/context-engineering)
