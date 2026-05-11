# DeepStrike Python SDK

Agent framework built on a Rust kernel. The kernel handles loop control, context compression, skill selection, and termination — the SDK handles all I/O.

## Install

```bash
pip install deepstrike
```

Requires Python 3.10+. The Rust kernel is distributed as a pre-built wheel (`deepstrike._kernel`).

---

## Quick start

```python
import asyncio
from deepstrike import Agent, AnthropicProvider, tool

@tool
def add(x: int, y: int) -> int:
    """Add two numbers."""
    return x + y

agent = Agent(
    AnthropicProvider(api_key="..."),
    max_tokens=32_000,
    max_turns=10,
)
agent.register(add)

asyncio.run(agent.run("What is 2 + 3?"))
```

Streaming:

```python
async for event in agent.run_streaming("Summarize README.md"):
    if event.type == "text_delta":
        print(event.delta, end="", flush=True)
    elif event.type == "tool_call":
        print(f"\n[→ {event.name}]")
    elif event.type == "done":
        print(f"\ndone in {event.iterations} turns ({event.status})")
```

---

## Architecture

```
deepstrike/
├── agent.py          # Agent — top-level entry point
├── providers/        # LLM adapters (HTTP + streaming)
├── tools/            # @tool decorator, execution, built-ins
├── skills/           # SkillRegistry — .md file scanning
├── memory/           # WorkingMemory + MemorySource/Extractor protocols
├── knowledge/        # KnowledgeSource protocol
├── harness/          # Run control: SinglePass, EvalLoop, QualityGate
├── signals/          # RuntimeSignal, SignalSource, ScheduledPrompt
└── safety/           # PermissionManager (DEFAULT / PLAN / AUTO)
```

The kernel (`deepstrike._kernel`, Rust/PyO3) owns:
- `LoopStateMachine` — drives `call_llm → execute_tools → load_skills → done`
- `ContextEngine` — 5-partition context with pressure-based compression
- `Governance` — tool veto authority
- `SignalRouter` — external interrupt queue

---

## Providers

| Class | Backend | Notes |
|-------|---------|-------|
| `AnthropicProvider` | Anthropic API | Native SSE, `ThinkingDelta` support |
| `OpenAIProvider` | OpenAI API | SSE tool-call accumulation |
| `QwenProvider` | DashScope | `enable_thinking` via `extensions` |
| `DeepSeekProvider` | DeepSeek API | Reasoner models strip tools automatically |
| `MiniMaxProvider` | MiniMax API | M1 reasoning via `expose_reasoning` |
| `OllamaProvider` | Local Ollama | `http://localhost:11434` default |

All providers accept `RetryConfig` for exponential backoff and share a `CircuitBreaker`.

```python
from deepstrike import AnthropicProvider, RetryConfig

provider = AnthropicProvider(
    api_key="...",
    model="claude-opus-4-7",
    retry_config=RetryConfig(max_retries=5, base_delay=2.0),
)
```

Thinking / reasoning:

```python
async for event in agent.run_streaming("...", extensions={"enable_thinking": True}):
    if event.type == "thinking_delta":
        print(event.delta, end="")
```

---

## Tools

```python
from deepstrike import tool, Agent

@tool
def search(query: str, top_k: int = 5) -> str:
    """Search the knowledge base."""
    return my_search(query, top_k)

agent.register(search)
agent.unregister("search")
agent.block_tool("bash")   # Governance veto — permanent for this agent instance
```

Built-in tools: `read_file`.

---

## Skills

Skills are `.md` files with YAML frontmatter. The kernel selects and injects them into `C_skill` automatically.

```markdown
---
name: debug
description: Step-by-step debugging guide
when_to_use: error, traceback, exception
effort: 2
estimated_tokens: 800
---

## Debug protocol
1. Read the traceback carefully ...
```

```python
from deepstrike import Agent, SkillLoader, SkillRegistry

# Register available skills with the kernel
registry = SkillRegistry("~/.deepstrike/skills")
skills = registry.scan()

# Load skill content on demand
loader = SkillLoader("~/.deepstrike/skills")
agent = Agent(provider, max_tokens=32_000, max_turns=10, skill_loader=loader)
```

---

## Memory

Implement `MemorySource` to inject persistent context before a run, and `MemoryExtractor` to persist what was learned after.

```python
from deepstrike import MemorySource, MemoryExtractor, Agent

class MyMemorySource:
    async def load(self, goal: str) -> list[str]:
        return db.query(goal)          # your storage backend

class MyMemoryExtractor:
    async def extract(self, goal: str, final_text: str, turns: int) -> None:
        db.save(goal, final_text)      # your storage backend

agent = Agent(
    provider,
    max_tokens=32_000,
    max_turns=10,
    memory_source=MyMemorySource(),
    memory_extractor=MyMemoryExtractor(),
)
```

`WorkingMemory` is an in-process scratch pad for within-run state:

```python
from deepstrike import WorkingMemory
mem = WorkingMemory()
mem.set("step", 1)
mem.get("step")   # 1
```

---

## Knowledge

Inject run-scoped evidence (RAG results, API responses) without polluting long-term memory:

```python
from deepstrike import KnowledgeSource, Agent

class VectorSearch:
    async def retrieve(self, goal: str, top_k: int = 5) -> list[str]:
        return vector_db.search(goal, top_k)

agent = Agent(provider, max_tokens=32_000, max_turns=10,
              knowledge_source=VectorSearch())
```

Snippets are prepended as a system message before the first LLM call.

---

## Harness

Control how runs are attempted:

```python
from deepstrike import Agent, SinglePassHarness, EvalLoopHarness, HarnessRequest, QualityGate

# Single pass (default)
harness = SinglePassHarness(agent)
outcome = await harness.run(HarnessRequest(goal="Write a haiku"))

# Eval loop — retry until QualityGate passes (max 3 attempts)
class LengthGate:
    async def evaluate(self, request, outcome) -> bool:
        return len(outcome.result) > 50

harness = EvalLoopHarness(agent, gate=LengthGate(), max_attempts=3)
outcome = await harness.run(HarnessRequest(goal="Write a haiku"))
print(outcome.passed, outcome.iterations)
```

---

## Signals & interrupts

```python
from deepstrike import RuntimeSignal, ScheduledPrompt
import asyncio

# Interrupt a running agent from another coroutine
async def watchdog(agent):
    await asyncio.sleep(30)
    agent.interrupt()

# Convert a scheduled prompt to a RuntimeSignal
prompt = ScheduledPrompt(goal="Daily standup summary", run_at_ms=1_700_000_000_000)
signal = prompt.to_signal()
# signal.kind == "scheduled"
# signal.payload == {"goal": "Daily standup summary", "criteria": []}
```

Implement `SignalSource` to feed signals from any external source (cron, webhook, queue):

```python
from deepstrike import SignalSource, RuntimeSignal

class WebhookSource:
    async def next_signal(self) -> RuntimeSignal | None:
        event = await webhook_queue.get()
        return RuntimeSignal(kind="external", payload=event)
```

---

## Permissions

```python
from deepstrike import PermissionManager, PermissionMode

pm = PermissionManager(mode=PermissionMode.DEFAULT)
pm.grant("bash", "execute")
pm.grant("fs", "*")          # wildcard: all actions on fs
pm.revoke("bash", "execute")

decision = pm.evaluate("bash", "execute")
decision.allowed   # bool
decision.reason    # str
```

Modes: `DEFAULT` (evaluate grants), `PLAN` (block all), `AUTO` (allow all).

---

## Governance

```python
from deepstrike import Governance, Agent

gov = Governance()
gov.block_tool("bash")

agent = Agent(provider, max_tokens=32_000, max_turns=10, governance=gov)
agent.block_tool("write_file")   # also tracked on the Python side
```

---

## Stream events

| Event type | Fields |
|------------|--------|
| `text_delta` | `delta: str` |
| `thinking_delta` | `delta: str` |
| `tool_call` | `id, name, arguments` |
| `tool_result` | `call_id, name, content, is_error` |
| `done` | `iterations, total_tokens, status` |
| `error` | `message: str` |

`status` mirrors the kernel termination reason: `completed` / `max_turns` / `token_budget` / `timeout` / `user_abort` / `error`.
