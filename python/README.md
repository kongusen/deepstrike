# DeepStrike Python SDK

Runtime framework built on a Rust kernel. The kernel handles loop control, context compression, skill routing, governance, signal prioritization — the SDK handles all I/O.

## Install

```bash
pip install deepstrike
```

Requires Python 3.10+. The Rust kernel is distributed as a pre-built wheel (`deepstrike._kernel`).

---

## Quick start

```python
import asyncio
from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    OpenAIProvider,
    RuntimeOptions,
    RuntimeRunner,
    collect_text,
    tool,
)

@tool
async def add(x: int, y: int) -> str:
    """Add two numbers."""
    return str(x + y)

plane = LocalExecutionPlane().register(add)
runner = RuntimeRunner(RuntimeOptions(
    provider=OpenAIProvider(api_key="sk-...", model="gpt-5-mini"),
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=4096,
    max_turns=25,
))

asyncio.run(collect_text(runner.run_streaming("What is 17 + 28?")))
# => "45"
```

Streaming:

```python
async for event in runner.run_streaming("Summarize README.md"):
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

## Context model (four slots)

The kernel renders context as four LLM API slots — only **history** is compressed.

| Slot | Source | Role |
|------|--------|------|
| `system_stable` | system partition | Identity, rules — never changes within a run |
| `system_knowledge` | knowledge partition | Preloaded memory, skill defs — low frequency |
| `turns[0]` | `task_state` + signals | Goal, plan, progress, compression log, runtime signals |
| `turns[1..N]` | history | Conversation transcript |

```python
runner = RuntimeRunner(RuntimeOptions(
    initial_memory=["User prefers chartreuse."],  # → Slot 2
    system_prompt="You are a helpful assistant.",  # → Slot 1
    # ...
))
```

Full reference: [docs/concepts/context-slots-compression.md](../docs/concepts/context-slots-compression.md)

---

## Runtime options

```python
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=4096,            # context window size
    max_turns=25,               # max turns (default 25)
    timeout_ms=60_000,          # timeout in ms (None = no limit)
    extensions={"temperature": 0.1},
    skill_dir="./skills",       # skill .md files directory
    knowledge_source=my_ks,     # KnowledgeSource implementation
    governance=gov,             # kernel Governance instance
    signal_source=gateway,      # SignalGateway or any SignalSource
    dream_store=my_store,       # DreamStore for long-term memory
    agent_id="my-agent",        # required with dream_store for memory meta-tool
    initial_memory=["..."],     # preloaded blocks → Slot 2 (system_knowledge)
    sub_agent_harness=SubAgentHarnessConfig(  # optional: HarnessLoop for spawned sub-agents
        eval_provider=eval_provider,
        max_attempts=3,
    ),
))
```

---

## Tools

```python
from deepstrike import tool, read_file

plane.register(tool(name="search", description="Search.", parameters=schema)(my_fn))
plane.register(read_file)
plane.unregister("search")
```

---

## Skills

Set `skill_dir` — the kernel auto-injects a `skill` meta-tool, and the LLM loads skills by name on demand.

```python
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=4096,
    max_turns=25,
    skill_dir="./skills",
))
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

Implement `KnowledgeSource` — the kernel injects a `knowledge` meta-tool. **Runtime retrieval results land in history** as tool results. Use `initial_memory` for durable preload into Slot 2.

```python
from deepstrike import KnowledgeSource

class VectorSearch(KnowledgeSource):
    async def init(self) -> None:
        await vector_db.connect()

    async def retrieve(self, query: str, top_k: int = 5) -> list[str]:
        return await vector_db.search(query, top_k)

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=4096,
    max_turns=25,
    knowledge_source=VectorSearch(),
))
```

---

## Memory

### WorkingMemory (SDK-side scratch pad)

`WorkingMemory` is an SDK helper — not the kernel `working` partition (removed). Kernel task state renders into Slot 3 (`turns[0]`).

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
    async def save_session(self, data): ...

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=4096,
    max_turns=25,
    dream_store=MyStore(),
    agent_id="my-agent",
))

# In-session: LLM calls memory(query) → DreamStore.search() → history tool result
# Preload:    initial_memory → Slot 2 (system_knowledge)
# Post-session: trigger memory consolidation
result = await runner.dream("my-agent", now_ms=int(time.time() * 1000))
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

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=4096,
    max_turns=25,
    governance=gov,
))
# Every tool call: Permission → Veto → RateLimit → Constraint → Audit
```

---

## Signals

```python
from deepstrike import SignalGateway, ScheduledPrompt, RuntimeSignal

gw = SignalGateway()

gw.schedule(ScheduledPrompt(goal="standup", run_at_ms=target_time))
gw.ingest(RuntimeSignal(kind="interrupt", payload={}, urgency="critical"))

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=4096,
    max_turns=25,
    signal_source=gw,
))

runner.interrupt()  # direct interrupt
gw.destroy()
```

---

## Harness (evaluation framework)

```python
from deepstrike import SinglePassHarness, EvalLoopHarness, HarnessLoop, HarnessRequest

# 1. SinglePass — run once, always passes
outcome = await SinglePassHarness(runner).run(HarnessRequest(goal="Say hello"))

# 2. EvalLoop — retry until QualityGate passes
class ContainsHello(QualityGate):
    async def evaluate(self, request, outcome) -> bool:
        return "hello" in outcome.result.lower()

outcome = await EvalLoopHarness(runner, gate=ContainsHello(), max_attempts=3).run(req)

# 3. HarnessLoop — LLM-as-judge with feedback injection + skill extraction
loop = HarnessLoop(runner, eval_provider=eval_provider, max_attempts=3, skill_dir="./skills")

# Sub-agents: pass sub_agent_harness on RuntimeOptions to auto-evaluate spawned children
from deepstrike import SubAgentHarnessConfig
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    sub_agent_harness=SubAgentHarnessConfig(eval_provider=eval_provider, max_attempts=3),
    # ...
))
async for event in loop.run_streaming(HarnessRequest(goal="Write a haiku")):
    if event.type == "done":
        print(event.verdict.passed, event.verdict.feedback)
```

---

## Stream events

| Class | Key fields |
|-------|------------|
| `TextDelta` | `delta` |
| `ThinkingDelta` | `delta` |
| `ToolCallEvent` | `id`, `name`, `arguments` |
| `ToolDeltaEvent` | `call_id`, `name`, `delta`, `chunk?` |
| `ToolSuspendEvent` | `call_id`, `name`, `suspension_id`, `payload?` |
| `ToolResultEvent` | `call_id`, `content`, `is_error` |
| `DoneEvent` | `iterations`, `total_tokens`, `status` |
| `ErrorEvent` | `message` |

`status`: `completed` · `max_turns` · `token_budget` · `timeout` · `user_abort` · `error`
