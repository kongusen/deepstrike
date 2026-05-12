# DeepStrike Python SDK

Agent framework built on a Rust kernel. The kernel handles loop control, context compression, skill routing, governance, signal prioritization — the SDK handles all I/O.

## Install

```bash
pip install deepstrike
```

Requires Python 3.10+. The Rust kernel is distributed as a pre-built wheel (`deepstrike._kernel`).

---

## Quick start

```python
import asyncio
from deepstrike import Agent, OpenAIProvider, tool

@tool(name="add", description="Add two numbers.", parameters={
    "type": "object",
    "properties": {"x": {"type": "integer"}, "y": {"type": "integer"}},
    "required": ["x", "y"],
})
async def add(args):
    return str(args["x"] + args["y"])

agent = Agent(OpenAIProvider(api_key="sk-...", model="gpt-5-mini"), max_tokens=4096, max_turns=25)
agent.register(add)

asyncio.run(agent.run("What is 17 + 28?"))
# => "done in 2 turns (completed)"
```

Streaming:

```python
async for event in agent.run_streaming("Summarize README.md"):
    if isinstance(event, TextDelta):
        print(event.delta, end="", flush=True)
    elif isinstance(event, ToolCallEvent):
        print(f"\n[→ {event.name}]")
    elif isinstance(event, DoneEvent):
        print(f"\ndone in {event.iterations} turns ({event.status})")
```

---

## Providers

| Class | Backend | Notes |
|-------|---------|-------|
| `OpenAIProvider` | OpenAI API | SSE tool-call accumulation |
| `AnthropicProvider` | Anthropic API | Native SSE, `ThinkingDelta` support |
| `QwenProvider` | DashScope | `enable_thinking` via extensions |
| `DeepSeekProvider` | DeepSeek API | Reasoner models strip tools automatically |
| `MiniMaxProvider` | MiniMax API | M1 reasoning via `expose_reasoning` |
| `OllamaProvider` | Local Ollama | `http://localhost:11434` default |
| `KimiProvider` | Moonshot API | |

All providers accept `RetryConfig` for exponential backoff and share a `CircuitBreaker`.

---

## Agent options

```python
agent = Agent(
    provider,
    max_tokens=4096,            # context window size
    max_turns=25,               # max turns (default 25)
    timeout_ms=60_000,          # timeout in ms (None = no limit)
    extensions={"temperature": 0.1},
    skill_dir="./skills",       # skill .md files directory
    knowledge_source=my_ks,     # KnowledgeSource implementation
    governance=gov,             # kernel Governance instance
    signal_router=router,       # SignalRouter for external signals
    dream_store=my_store,       # DreamStore for long-term memory
    agent_id="my-agent",        # required with dream_store for memory meta-tool
)
```

---

## Tools

```python
from deepstrike import tool, read_file

agent.register(tool(name="search", description="Search.", parameters=schema)(my_fn))
agent.register(read_file())
agent.unregister("search")
agent.block_tool("bash")
```

---

## Skills

Set `skill_dir` — the kernel auto-injects a `skill` meta-tool, and the LLM loads skills by name on demand.

```python
agent = Agent(provider, max_tokens=4096, max_turns=25, skill_dir="./skills")
```

```markdown
---
name: summarize
description: Summarize text into 2-3 concise bullet points
when_to_use: When you need to condense long text
effort: 1
---
1. Identify the 2-3 most important points
2. Express each as a concise bullet
```

---

## Knowledge

Implement `KnowledgeSource` — the kernel injects a `knowledge` meta-tool.

```python
from deepstrike import KnowledgeSource

class VectorSearch(KnowledgeSource):
    async def retrieve(self, query: str, top_k: int = 5) -> list[str]:
        return await vector_db.search(query, top_k)

agent = Agent(provider, max_tokens=4096, max_turns=25, knowledge_source=VectorSearch())
```

---

## Memory

### WorkingMemory (in-session scratch pad)

```python
from deepstrike import WorkingMemory

mem = WorkingMemory()
mem.set("step", 1)
mem.get("step")  # 1
mem.clear()
```

### DreamStore (long-term memory + dreaming pipeline)

```python
from deepstrike import DreamStore

class MyStore(DreamStore):
    async def load_sessions(self, agent_id): ...
    async def load_memories(self, agent_id): ...
    async def commit(self, agent_id, result, existing): ...
    async def search(self, agent_id, query, top_k): ...

agent = Agent(provider, max_tokens=4096, max_turns=25,
              dream_store=MyStore(), agent_id="my-agent")

# In-session: LLM calls memory(query) → DreamStore.search()
# Post-session: trigger memory consolidation
result = await agent.dream("my-agent", now_ms=int(time.time() * 1000))
```

---

## Governance

### SDK PermissionManager

```python
from deepstrike import PermissionManager, PermissionMode

pm = PermissionManager(PermissionMode.DEFAULT)
pm.grant("fs", "read")
pm.revoke("db", "drop")
pm.evaluate("fs", "read")  # PermissionDecision(allowed=True, ...)
```

### Kernel Governance (full pipeline)

```python
from deepstrike import Governance

gov = Governance("allow")
gov.add_permission_rule("danger.*", "deny")
gov.block_tool("rm_rf")
gov.set_rate_limit("api_call", max_calls=10, window_ms=60_000)

agent = Agent(provider, max_tokens=4096, max_turns=25, governance=gov)
# Every tool call: Permission → Veto → RateLimit → Constraint → Audit
```

---

## Signals

```python
from deepstrike import SignalGateway, ScheduledPrompt, RuntimeSignal

gw = SignalGateway()
rx = gw.subscribe()

gw.schedule(ScheduledPrompt(goal="standup", run_at_ms=target_time))
gw.ingest(RuntimeSignal(kind="interrupt", payload={}, priority=10))

agent.interrupt()  # direct interrupt
gw.destroy()
```

---

## Harness (evaluation framework)

```python
from deepstrike import SinglePassHarness, EvalLoopHarness, HarnessLoop, HarnessRequest

# 1. SinglePass — run once, always passes
outcome = await SinglePassHarness(agent).run(HarnessRequest(goal="Say hello"))

# 2. EvalLoop — retry until QualityGate passes
class ContainsHello(QualityGate):
    async def evaluate(self, request, outcome) -> bool:
        return "hello" in outcome.result.lower()

outcome = await EvalLoopHarness(agent, gate=ContainsHello(), max_attempts=3).run(req)

# 3. HarnessLoop — LLM-as-judge with feedback injection + skill extraction
loop = HarnessLoop(agent, eval_provider=eval_provider, max_attempts=3, skill_dir="./skills")
outcome = await loop.run(HarnessRequest(goal="Write a haiku", criteria=["Must be 3 lines"]))
print(outcome.passed, outcome.feedback)
```

---

## Stream events

| Class | Key fields |
|-------|------------|
| `TextDelta` | `delta` |
| `ThinkingDelta` | `delta` |
| `ToolCallEvent` | `id`, `name`, `arguments` |
| `ToolResultEvent` | `call_id`, `content`, `is_error` |
| `DoneEvent` | `iterations`, `total_tokens`, `status` |
| `ErrorEvent` | `message` |

`status`: `completed` · `max_turns` · `token_budget` · `timeout` · `user_abort` · `error`
