# DeepStrike Python SDK — API 使用指南

> Runtime v1：公共入口为 `RuntimeRunner` + `SessionLog` + `ExecutionPlane`。详见 `python/README.md` 与 `docs/spec-runtime-v1.md`。

## 目录

1. [快速开始](#1-快速开始)
2. [Provider 配置](#2-provider-配置)
3. [RuntimeRunner 基础](#3-runtimerunner-基础)
4. [工具调用 (Tools)](#4-工具调用-tools)
5. [技能 (Skills)](#5-技能-skills)
6. [知识检索 (Knowledge)](#6-知识检索-knowledge)
7. [记忆系统 (Memory)](#7-记忆系统-memory)
8. [治理管线 (Governance)](#8-治理管线-governance)
9. [信号系统 (Signals)](#9-信号系统-signals)
10. [评估框架 (Harness)](#10-评估框架-harness)
11. [协作层 (Collaboration)](#11-协作层-collaboration)

---

## 1. 快速开始

```bash
pip install deepstrike
```

```python
import asyncio
from deepstrike import (
    OpenAIProvider, InMemorySessionLog, LocalExecutionPlane,
    RuntimeOptions, RuntimeRunner, collect_text,
)

provider = OpenAIProvider(
    api_key="sk-your-key",
    model="gpt-5-mini",
    base_url="https://api.openai.com/v1",
)

async def main():
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=4096,
        max_turns=25,
    ))
    result = await collect_text(runner.run_streaming("用一句话解释什么是 Python"))
    print(result)

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

## 3. RuntimeRunner 基础

### 3.1 同步运行

```python
result = await collect_text(runner.run_streaming("Say hello"))
```

### 3.2 流式运行

```python
from deepstrike import TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent

text = ""
async for event in runner.run_streaming("What is 2+2?"):
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
async for event in runner.run_streaming("打个招呼", criteria=["必须包含 hello", "不超过 20 字"]):
    ...
```

### 3.4 Extensions

```python
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=4096,
    max_turns=25,
    extensions={"temperature": 0.1, "top_p": 0.9},
))
```

### 3.5 中断

```python
import asyncio

async def interrupt_later():
    await asyncio.sleep(5)
    runner.interrupt()

asyncio.create_task(interrupt_later())
result = await collect_text(runner.run_streaming("Write a long essay..."))
```

### 3.6 RuntimeOptions 主要字段

```python
RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=4096,
    max_turns=25,
    timeout_ms=60_000,
    skill_dir="./skills",
    extensions={},
    governance=None,
    signal_source=None,
    knowledge_source=None,
    dream_store=None,
    agent_id=None,
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

plane = LocalExecutionPlane().register(add)
runner = RuntimeRunner(RuntimeOptions(provider=provider, session_log=InMemorySessionLog(), execution_plane=plane, max_tokens=4096))
```

### 4.2 内置工具

```python
from deepstrike import read_file

plane.register(read_file())
```

### 4.3 取消注册

```python
plane.unregister("add")
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
gov.block_tool("dangerous_tool")  # 通过 Governance 屏蔽
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

# skill_dir 在 RuntimeOptions 上配置
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=4096,
    max_turns=25,
    skill_dir="./skills",
))
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

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=4096,
    max_turns=25,
    knowledge_source=MyKnowledge(),
))
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

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=4096,
    max_turns=25,
    dream_store=MyStore(),
    agent_id="my-agent-1",
))

# 触发记忆整合
result = await runner.dream("my-agent-1", now_ms=int(time.time() * 1000))
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

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=4096,
    max_turns=25,
    governance=gov,
))
# 每次工具调用自动经过 Permission → Veto → RateLimit → Constraint 管线
```

### 8.3 工具级屏蔽

```python
plane.register(dangerous_tool)
gov.block_tool("dangerous_tool")  # 通过 Governance 屏蔽
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

# RuntimeRunner 集成
from deepstrike import SignalGateway
rx = SignalGateway().subscribe()
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=4096,
    max_turns=25,
    signal_source=rx,
))

gw.destroy()
```

---

## 10. 评估框架 (Harness)

### 10.1 SinglePassHarness

```python
from deepstrike import SinglePassHarness, HarnessRequest

harness = SinglePassHarness(runner)
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

harness = EvalLoopHarness(runner, gate=ContainsHello(), max_attempts=3)
outcome = await harness.run(HarnessRequest(goal="Greet the user"))
```

### 10.3 HarnessLoop（LLM-as-Judge）

```python
from deepstrike import HarnessLoop, HarnessRequest

harness = HarnessLoop(
    runner,
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
| --- | --- |
| `TextDelta` | `delta: str` |
| `ThinkingDelta` | `delta: str` |
| `ToolCallEvent` | `id, name, arguments` |
| `ToolResultEvent` | `call_id, content, is_error` |
| `DoneEvent` | `iterations, total_tokens, status` |
| `ErrorEvent` | `message: str` |

---

## 11. 协作层 (Collaboration)

协作层提供多 Agent 协调能力。完整 API 参见 [collaboration.md](./collaboration.md)。

### 11.1 VerificationContract — 验证契约

```python
from deepstrike import ContractBuilder

contract = (ContractBuilder("report-v1", "撰写关于 X 的研究报告")
    .criterion("has-sources",      "报告引用至少 3 个来源", weight=0.4)
    .criterion("no-hallucination", "所有结论均可追溯至引用", weight=0.6)
    .anti_pattern("不得伪造引用")
    .evidence("最终报告正文")
    .build())
```

### 11.2 AgentPool — 角色隔离的代理池

```python
from deepstrike import AgentPool, RuntimeRunner, RuntimeOptions, InMemorySessionLog, LocalExecutionPlane

def make_runner(**kw):
    return RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=4096,
        **kw,
    ))

pool = (AgentPool()
    .add("executor", make_runner(max_tokens=32_000, skill_dir="./skills"))
    .add("verifier", make_runner(max_tokens=8_000)))
```

### 11.3 CreatorVerifierMode — 双 Agent 协作

```python
from deepstrike import CreatorVerifierMode, HandoffBus

mode = CreatorVerifierMode(pool, max_attempts=3)
outcome = await mode.run(contract)

print(outcome.success)           # True / False
print(outcome.attempts_used)     # 实际尝试次数
print(outcome.check_results)     # list[ContractCheckResult] — 每条标准的审核结果
print(outcome.handoff)           # HandoffArtifact — 可传递给下一个 sprint

# 漂移监控
metrics = mode.get_metrics()     # CreatorVerifierMetrics(total, failed, drift_rate)
if mode.is_drifting(0.05):
    pass  # drift_rate > 5% — 暂停自动委派，升级人工审核

# 交接协议
if HandoffBus.requires_escalation(outcome.handoff):
    print("Blocked on:", outcome.handoff.blocked_on)
note = HandoffBus.to_context_note(outcome.handoff)
# 注入下一轮 Agent 的 working 分区
```

### 11.4 OrchestrationMode — 三角色完整流

编排者（orchestrator）从原始目标生成 VerificationContract，然后由 CreatorVerifierMode 执行。

```python
from deepstrike import AgentPool, OrchestrationMode

def runner_for(p, **kw):
    return RuntimeRunner(RuntimeOptions(provider=p, session_log=InMemorySessionLog(), execution_plane=LocalExecutionPlane(), max_tokens=4096, **kw))

pool = (AgentPool()
    .add("orchestrator", runner_for(reasoner_provider, max_tokens=8_000))
    .add("executor",     runner_for(executor_provider, max_tokens=32_000))
    .add("verifier",     runner_for(verifier_provider, max_tokens=8_000)))

mode = OrchestrationMode(pool)
outcome, contract = await mode.run("为新能源汽车行业撰写市场分析")

print(contract.id, outcome.success)
```

### 11.5 HandoffBus — 统一交接面

```python
from deepstrike import HandoffBus, ContractOutcomeInput

# 从 ContractDrivenHarness 结果构建
handoff = HandoffBus.from_contract_outcome(
    ContractOutcomeInput(contract, check_results, artifact, success=True)
)

# 从子 Agent 最终消息构建
handoff = HandoffBus.from_sub_agent_result(goal=goal, final_message=msg, sprint=2)

# 从 dream 整合结果构建
handoff = HandoffBus.from_dream(goal=goal, dream_result=result)

# 渲染为上下文注入字符串
note = HandoffBus.to_context_note(handoff)

# 检查是否需要升级
if HandoffBus.requires_escalation(handoff, drift_threshold=0.05):
    ...
```
