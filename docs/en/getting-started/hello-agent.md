# Hello Agent

Run your first tool-using Agent in five minutes. This example gives an Agent a model, one file-reading tool, streaming output, and a session.

## Code

```python
import asyncio
import os
from deepstrike import AnthropicProvider, create_agent, read_file

async def main(goal: str):
    agent = create_agent(
        "reader",
        model="anthropic/claude",
        runtime_binding={"provider": AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"])},
        tools=[read_file],
    )
    result = await agent.run(goal)
    print(result["output"])

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

## Advanced host control

If you do not need streaming events:

```python
from deepstrike import create_agent, AnthropicProvider

agent = create_agent("reader", model="anthropic/claude", runtime_binding={"provider": AnthropicProvider(api_key=...)})
result = await agent.run("Summarize README.md")
print(result["output"])
```

## Next Steps

- [Choosing an API](./run-agent-vs-runner)
- [Context Engineering](/en/guides/context-engineering)
