# Hello Agent

5 分钟跑通第一个使用工具的 Agent。这个示例为 Agent 配置模型、文件读取工具、流式输出和 Session。

## 代码

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

## 运行

```bash
cd python
pip install -e .
ANTHROPIC_API_KEY=sk-... python examples/hello_agent/main.py "Read README.md and summarize"
```

## Agent 做了什么

1. Agent 收到目标和 `read_file` 能力。
2. 模型判断是否需要读取文件，并发起工具请求。
3. 应用执行工具，把结果返回给 Agent。
4. Agent 根据结果生成回答，应用同时收到流式事件。
5. Session 结束时产生包含运行摘要的 `DoneEvent`。

## 更简单的方式

若不需要流式事件：

```python
from deepstrike import run_agent, AnthropicProvider

text = await run_agent(
    provider=AnthropicProvider(api_key=...),
    goal="Summarize README.md",
)
print(text)
```

## 下一步

- [API 选型](./run-agent-vs-runner)
- [Context 工程](../guides/context-engineering)
