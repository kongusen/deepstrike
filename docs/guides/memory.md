# Memory

Memory 让 Agent 把有用事实带到下一次对话。DeepStrike 将 scratch memory、Session 历史和持久记录分开，Agent 可以召回重要内容，而不必重放所有旧消息。

**代码**：
- Runtime：`crates/deepstrike-core/src/memory/`
- SDK：`python/deepstrike/memory/`、`RuntimeRunner.write_memory` / `query_memory`

---

## 三种 Memory

| 层 | 用途 |
|----|---------|
| Working | 当前运行的 scratch pad |
| Session | 一次运行发生了什么，包括工具和 Agent 活动 |
| Durable | 未来运行可用的事实、偏好和项目知识 |

Memory 不是自动归档的 transcript。应用决定保存什么、如何校验，以及如何检索。

![Memory Mechanisms](/memory_mechanisms.svg)

## 概念

| 层 | 说明 |
|----|------|
| Working | `WorkingMemory` scratch pad |
| Session | 单次 run 的 session data |
| Durable | `MemoryStore` 持久化 + session extraction 整理 |

Agent 可以通过 runner API 或内置的 `memory` 工具写入和查询 Memory。

---

## Level 1：write / query

实现 `MemoryStore` 协议（`memory/protocols.py`），传入 runner：

```python
class MyStore:
    async def put(self, agent_id, record): ...
    async def get(self, agent_id, record_id): return None
    async def delete(self, agent_id, record_id): ...
    async def save_session(self, data): ...
    async def search(self, agent_id, query): return []

runner = RuntimeRunner(RuntimeOptions(
    ...,
    agent_id="my-agent",
    memory_store=MyStore(),
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

校验失败时 runtime emit observation，**不 commit** 到 store。

---

## Level 3：Run 前预取（+ Renewal 重查）

```python
def pre_query(goal: str, phase: str | None = None):
    # phase == "initial"：turn-1 前的一次性预取
    # phase == "renewal"：sprint renewal 之后自动重发（旧 history 连同早先的命中已被丢弃）
    return ["user preferences", "project conventions"]

RuntimeOptions(
    ...,
    pre_query_memory=pre_query,
    memory_store=store,
    agent_id="my-agent",
)
```

启动前搜索 durable `MemoryStore`，hits 作为**普通轮次注入 history**（单次使用的事实内容，随压缩金字塔自然衰减，不钉进 knowledge 分区）。sprint renewal 会整体重建 history，钩子随即以 `phase="renewal"` 重发一次，让新 sprint 从新鲜召回开始。

---

## Level 4：Session extraction

Runner 在 session 结束后保存 transcript，并通过 provider 或 `memory_summarizer` 提取候选记录；每条记录仍回到 runtime `write_memory` gate 后写入 `MemoryStore`。

SDK 配置：

```python
RuntimeOptions(
    ...,
    memory_provider=synthesis_provider,
    memory_summarizer=custom_summarizer,
    memory_system_prompt="Extract durable insights from sessions...",
)
```

---

## Level 5：召回 journaling 与 retention

召回是一次带反馈的打分查询，遗忘是基于 retention 的驱逐，两者都由应用的 `MemoryStore` 掌控。

- **Recall journaling。** 当 `query_memory` 命中一条记录时，runtime 依据这次命中推导出该记录的下一个 `recall_count`，并 emit 一个 `memory_recalled` observation。应用的 `MemoryStore.recordRecall` 把它折回，因此一条被反复召回的记录会累积使用度，而无需 runtime 持有 durable ledger。
- **达到阈值即提升。** 越过 `MemoryPolicy.promotion_recall_threshold` 会 emit 一个 `promotion_suggested` observation（边沿触发，仅在越过的那一刻一次），通过 `onPromotionSuggested` 回调呈现给应用，好让一条被频繁召回的记录钉进 durable knowledge。
- **Retention 与驱逐。** `memory_retention_score` 按使用度、kind、confidence、recency 和 size 给记录排名（钉选记录排到最前）。应用的 `MemoryStore` 用它把冷记录驱逐到容量以内，遗忘是一次确定性排名，不是 FIFO。

```python
RuntimeOptions(
    memory_policy=MemoryPolicy(promotion_recall_threshold=3),
    on_promotion_suggested=lambda rec: memory_store.set_pinned(rec.record_id, True),
)
```

打分词汇的应用侧镜像：`node/src/memory/retention.ts`、`python/deepstrike/memory/retention.py`。

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
- `InMemoryMemoryStore` — 开发用实现
