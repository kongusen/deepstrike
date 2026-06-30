# Hello Agent

Run your first agent in five minutes. Full example: `python/examples/hello_agent/main.py`.

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

## What Happens

1. `RuntimeRunner` creates the kernel and sends `start_run`
2. The kernel returns `CallLLM` with `RenderedContext` and tool schemas
3. The provider streams tokens; if the model calls a tool, `ExecutionPlane` runs `read_file`
4. Tool results are fed back into the kernel for the next turn
5. On completion, the runner emits `DoneEvent`

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
