# DeepStrike Python SDK — API 使用指南

## 目录

1. [快速开始](#1-快速开始)
2. [Provider 配置](#2-provider-配置)
3. [Agent 基础](#3-agent-基础)
4. [工具调用 (Tools)](#4-工具调用-tools)
5. [技能 (Skills)](#5-技能-skills)
6. [知识检索 (Knowledge)](#6-知识检索-knowledge)
7. [记忆系统 (Memory)](#7-记忆系统-memory)
8. [治理管线 (Governance)](#8-治理管线-governance)
9. [信号系统 (Signals)](#9-信号系统-signals)
10. [评估框架 (Harness)](#10-评估框架-harness)

---

## 1. 快速开始

```bash
pip install deepstrike
```

```python
import asyncio
from deepstrike import Agent, OpenAIProvider

provider = OpenAIProvider(
    api_key="sk-your-key",
    model="gpt-5-mini",
    base_url="https://api.openai.com/v1",
)

agent = Agent(provider, max_tokens=4096, max_turns=25)

async def main():
    result = await agent.run("用一句话解释什么是 Python")
    print(result)  # => "done in 1 turns (completed)"

asyncio.run(main())
```

---

## 2. Provider 配置

```python
from deepstrike import (
    OpenAIProvider, AnthropicProvider,
    QwenProvider, DeepSeekProvider, MiniMaxProvider, OllamaProvider, KimiProvider,
)

# OpenAI 或兼容代理
provider = OpenAIProvider(
    api_key="sk-xxx",
    model="gpt-5-mini",
    base_url="https://xiaoai.plus/v1",
)

# 快捷构造
qwen     = QwenProvider(api_key="key")
deepseek = DeepSeekProvider(api_key="key")
anthropic = AnthropicProvider(api_key="key")
ollama   = OllamaProvider(model="llama3")
kimi     = KimiProvider(api_key="key")
```

### 自定义 Provider

继承 `LLMProvider` 基类：

```python
from deepstrike import LLMProvider, StreamEvent, TextDelta

class MyProvider(LLMProvider):
    async def stream(self, messages, tools=None, extensions=None):
        # 返回 AsyncIterator[StreamEvent]
        yield TextDelta(delta="Hello!")
```

---

## 3. Agent 基础

### 3.1 同步运行

```python
result = await agent.run("Say hello")
# => "done in 1 turns (completed)"
```

### 3.2 流式运行

```python
from deepstrike import TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent

text = ""
async for event in agent.run_streaming("What is 2+2?"):
    if isinstance(event, TextDelta):
        print(event.delta, end="", flush=True)
        text += event.delta
    elif isinstance(event, ToolCallEvent):
        print(f"\nTool: {event.name}")
    elif isinstance(event, ToolResultEvent):
        print(f"Result: {event.content}")
    elif isinstance(event, DoneEvent):
        print(f"\n--- {event.iterations} turns, {event.total_tokens} tokens, {event.status}")
    elif isinstance(event, ErrorEvent):
        print(f"Error: {event.message}")
```

### 3.3 带 Criteria 运行

```python
async for event in agent.run_streaming("打个招呼", criteria=["必须包含 hello", "不超过 20 字"]):
    ...
```

### 3.4 Extensions

```python
agent = Agent(provider, max_tokens=4096, max_turns=25,
              extensions={"temperature": 0.1, "top_p": 0.9})
```

### 3.5 中断

```python
import asyncio

async def interrupt_later():
    await asyncio.sleep(5)
    agent.interrupt()

asyncio.create_task(interrupt_later())
result = await agent.run("Write a long essay...")
```

### 3.6 Agent 构造参数

```python
Agent(
    provider,
    max_tokens=4096,        # 上下文窗口大小
    max_turns=25,           # 最大轮次
    timeout_ms=60_000,      # 超时（毫秒），None 则无限
    skill_dir="./skills",   # 技能目录
    extensions={},          # LLM 参数透传
    governance=None,        # 内核 Governance 实例
    signal_router=None,     # SignalRouter 实例
    knowledge_source=None,  # KnowledgeSource 实例
    dream_store=None,       # DreamStore 实例
    agent_id=None,          # Agent 标识
)
```

---

## 4. 工具调用 (Tools)

### 4.1 使用 `@tool` 装饰器

```python
from deepstrike import tool

@tool(
    name="add",
    description="Add two integers and return the sum.",
    parameters={
        "type": "object",
        "properties": {
            "x": {"type": "integer", "description": "First number"},
            "y": {"type": "integer", "description": "Second number"},
        },
        "required": ["x", "y"],
    },
)
async def add(args):
    return str(args["x"] + args["y"])

agent.register(add)
```

### 4.2 内置工具

```python
from deepstrike import read_file

agent.register(read_file())
```

### 4.3 取消注册

```python
agent.unregister("add")
```

### 4.4 手动执行

```python
from deepstrike import execute_tools

results = await execute_tools(tool_calls, registered_tools)
for r in results:
    print(r.output, r.is_error)
```

### 4.5 屏蔽工具

```python
agent.block_tool("dangerous_tool")
```

---

## 5. 技能 (Skills)

```python
from deepstrike import SkillRegistry

# 扫描技能目录
registry = SkillRegistry()
skills = registry.scan("./skills")
for s in skills:
    print(f"{s.name}: {s.description}")

# Agent 自动加载
agent = Agent(provider, max_tokens=4096, max_turns=25, skill_dir="./skills")
# 内核注入 `skill` meta-tool，LLM 按名称加载
```

技能文件格式 (`skills/summarize.md`)：

```markdown
---
name: summarize
description: Summarize text into 2-3 concise bullet points
when_to_use: When you need to condense long text
effort: 1
estimated_tokens: 200
---

To summarize text effectively:
1. Identify the 2-3 most important points
2. Express each as a concise bullet starting with "•"
```

---

## 6. 知识检索 (Knowledge)

```python
from deepstrike import KnowledgeSource

class MyKnowledge(KnowledgeSource):
    async def retrieve(self, query: str, top_k: int = 5) -> list[str]:
        # 向量搜索、API 调用等
        return ["DeepStrike 是一个 Agent 框架。"]

agent = Agent(provider, max_tokens=4096, max_turns=25,
              knowledge_source=MyKnowledge())
# 内核注入 `knowledge` meta-tool
```

---

## 7. 记忆系统 (Memory)

### 7.1 WorkingMemory

```python
from deepstrike import WorkingMemory

mem = WorkingMemory()
mem.set("user_name", "Alice")
mem.get("user_name")  # "Alice"
mem.delete("user_name")
mem.clear()
```

### 7.2 DreamStore

```python
from deepstrike import DreamStore, SessionData, MemoryEntry, CurationResult

class MyStore(DreamStore):
    async def load_sessions(self, agent_id: str) -> list[SessionData]:
        ...
    async def load_memories(self, agent_id: str) -> list[MemoryEntry]:
        ...
    async def commit(self, agent_id: str, result: CurationResult, existing: list[MemoryEntry]) -> None:
        ...
    async def search(self, agent_id: str, query: str, top_k: int) -> list[MemoryEntry]:
        ...

agent = Agent(provider, max_tokens=4096, max_turns=25,
              dream_store=MyStore(), agent_id="my-agent-1")

# 触发记忆整合
result = await agent.dream("my-agent-1", now_ms=int(time.time() * 1000))
print(f"{result.sessions_processed} sessions, {result.insights_extracted} insights")
```

---

## 8. 治理管线 (Governance)

### 8.1 SDK PermissionManager

```python
from deepstrike import PermissionManager, PermissionMode

pm = PermissionManager(PermissionMode.DEFAULT)
pm.grant("fs", "read")
pm.grant("fs", "*")
pm.revoke("db", "drop")

decision = pm.evaluate("fs", "read")
assert decision.allowed
```

### 8.2 内核 Governance

```python
from deepstrike import Governance

gov = Governance("allow")  # 默认策略
gov.add_permission_rule("danger.*", "deny")
gov.block_tool("rm_rf")
gov.set_rate_limit("api_call", max_calls=10, window_ms=60_000)

agent = Agent(provider, max_tokens=4096, max_turns=25, governance=gov)
# 每次工具调用自动经过 Permission → Veto → RateLimit → Constraint 管线
```

### 8.3 Agent 级屏蔽

```python
agent.register(dangerous_tool)
agent.block_tool("dangerous_tool")
# LLM 调用被屏蔽工具 → 返回 ErrorEvent
```

---

## 9. 信号系统 (Signals)

```python
from deepstrike import SignalGateway, ScheduledPrompt, RuntimeSignal

gw = SignalGateway()

# 定时调度
gw.schedule(ScheduledPrompt(goal="daily standup", run_at_ms=target_time_ms))

# 订阅
rx = gw.subscribe()

# 注入外部信号
gw.ingest(RuntimeSignal(kind="interrupt", payload={}, priority=10))

# Agent 集成
from deepstrike import SignalRouter
router = SignalRouter(max_queue_size=256)
agent = Agent(provider, max_tokens=4096, max_turns=25, signal_router=router)

gw.destroy()
```

---

## 10. 评估框架 (Harness)

### 10.1 SinglePassHarness

```python
from deepstrike import SinglePassHarness, HarnessRequest

harness = SinglePassHarness(agent)
outcome = await harness.run(HarnessRequest(goal="Say hello"))
assert outcome.passed
print(outcome.result)
```

### 10.2 EvalLoopHarness

```python
from deepstrike import EvalLoopHarness, QualityGate, HarnessRequest, HarnessOutcome

class ContainsHello(QualityGate):
    async def evaluate(self, request: HarnessRequest, outcome: HarnessOutcome) -> bool:
        return "hello" in outcome.result.lower()

harness = EvalLoopHarness(agent, gate=ContainsHello(), max_attempts=3)
outcome = await harness.run(HarnessRequest(goal="Greet the user"))
```

### 10.3 HarnessLoop（LLM-as-Judge）

```python
from deepstrike import HarnessLoop, HarnessRequest

harness = HarnessLoop(
    agent,
    eval_provider=eval_provider,
    max_attempts=3,
    skill_dir="./skills",
)

outcome = await harness.run(HarnessRequest(
    goal="Write a haiku about the ocean",
    criteria=["Must be exactly 3 lines"],
))
print(f"Passed: {outcome.passed}, Feedback: {outcome.feedback}")
```

---

## 流式事件类型

| 类 | 主要字段 |
|----|----------|
| `TextDelta` | `delta: str` |
| `ThinkingDelta` | `delta: str` |
| `ToolCallEvent` | `id, name, arguments` |
| `ToolResultEvent` | `call_id, content, is_error` |
| `DoneEvent` | `iterations, total_tokens, status` |
| `ErrorEvent` | `message: str` |
