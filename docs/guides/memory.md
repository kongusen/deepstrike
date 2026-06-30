# Memory

Memory 是 Agent OS 的 **Memory Plane**。它把短期推理状态、session 证据和 durable knowledge 分层管理，写入路径走 kernel syscall 校验，读取路径再回到 Context VM 的 knowledge 槽位。

**代码**：
- Kernel：`crates/deepstrike-core/src/memory/`
- SDK：`python/deepstrike/memory/`、`RuntimeRunner.write_memory` / `query_memory`

---

## 在 Agent OS 中的位置

| 层 | OS 语义 |
|----|---------|
| Working | 当前 run 的 scratch pad，不承诺跨 session 持久化 |
| Session | 证据链的一部分，可审计、可恢复 |
| Durable | DreamStore 承担长期知识与跨 session 检索 |
| Syscall | `write_memory` / `query_memory` 先经 kernel 校验再由 SDK 执行 |

Memory 不应该被理解成“自动塞历史消息”。它是一个受策略约束的知识设备：写什么、何时写、如何检索，都需要能被审计和回放。

![Memory Mechanisms](/memory_mechanisms.svg)

## 概念

| 层 | 说明 |
|----|------|
| Working | `WorkingMemory` scratch pad |
| Session | 单次 run 的 session data |
| Durable | `DreamStore` 持久化 + idle pipeline 整理 |

Meta-tool / syscall：`memory` 工具 + `write_memory` / `query_memory` kernel events。

---

## Level 1：write / query

实现 `DreamStore` 协议（`memory/protocols.py`），传入 runner：

```python
class MyStore:
    async def load_memories(self, agent_id): return []
    async def load_sessions(self, agent_id): return []
    async def commit(self, agent_id, result, existing): ...
    async def save_session(self, data): ...
    async def search(self, agent_id, query, top_k=5): return []

runner = RuntimeRunner(RuntimeOptions(
    ...,
    agent_id="my-agent",
    dream_store=MyStore(),
))

await runner.write_memory({
    "metadata": {
        "name": "prefers-small-tests",
        "description": "User prefers focused unit tests",
        "kind": "feedback",
        "created_at": 1,
        "updated_at": 1,
    },
    "content": "User prefers focused unit tests for SDK behavior.",
}, session_id="s1")

hits = await runner.query_memory({
    "current_context": "Need memory about tests",
    "active_tools": [],
    "already_surfaced": [],
    "top_k": 3,
}, session_id="s1")
```

参考测试：`python/tests/test_memory_syscall.py`

---

## Level 2：MemoryPolicy

```python
from deepstrike import MemoryPolicy

RuntimeOptions(
    ...,
    memory_policy=MemoryPolicy(
        validation_enabled=True,
        max_content_bytes=4096,
        max_name_length=64,
        retrieval_top_k=5,
        stale_warning_days=30,
    ),
)
```

校验失败时 kernel  emit observation，**不 commit** 到 store。

---

## Level 3：Run 前预取

```python
def pre_query(goal: str):
    return ["user preferences", "project conventions"]

RuntimeOptions(
    ...,
    pre_query_memory=pre_query,
    dream_store=store,
    agent_id="my-agent",
)
```

启动前 search dream store，hits 注入 knowledge 分区。

---

## Level 4：Idle Pipeline（Dreaming）

内核 `idle_pipeline.rs` 两阶段：

```
Phase 1: TraceAnalyzer（规则）→ SynthesizeInsights（SDK 调 LLM）
Phase 2: SynthesisResult → MemoryCurator（去重/冲突）→ CommitMemories
```

SDK 配置：

```python
RuntimeOptions(
    ...,
    dream_provider=synthesis_provider,
    dream_summarizer=custom_summarizer,
    dream_system_prompt="Extract durable insights from sessions...",
)
```

---

## ResourceQuota 写频率限制

```python
from deepstrike import ResourceQuota, MemoryWriteRateLimit

RuntimeOptions(
    ...,
    resource_quota=ResourceQuota(
        memory_writes_per_window=MemoryWriteRateLimit(max_writes=10, window_ms=60_000),
    ),
)
```

---

## 延伸阅读

- [Context 工程](./context-engineering) — knowledge 分区
- [Governance](./governance) — syscall trap
- `InMemoryDreamStore` — 开发用实现
