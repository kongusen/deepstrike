# Hello Agent

5 分钟跑通第一个 Agent。完整示例：`python/examples/hello_agent/main.py`。

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

## 发生了什么

1. `RuntimeRunner` 创建 kernel，启动 `start_run`
2. Kernel 返回 `CallLLM` + `RenderedContext` + 工具 schema
3. Provider 流式返回；若有 tool call，`ExecutionPlane` 执行 `read_file`
4. 工具结果回灌 kernel，进入下一 turn
5. 完成后 emit `DoneEvent`

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
