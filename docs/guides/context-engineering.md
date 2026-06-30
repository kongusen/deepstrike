# Context 工程

Context 工程是 Agent OS 的 **Context VM 运行面**。它不只是把 message 拼起来，而是把 identity、knowledge、history、ephemeral state 拆成可渲染、可压缩、可缓存、可分页的工作集。

**代码**：`crates/deepstrike-core/src/context/`（`ContextManager`、`renderer`、`compression`）

---

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 对 kernel | 提供每轮 `CallLLM` 前的确定性 render 结果 |
| 对 provider | 保持 stable prefix，提升 prompt cache 命中 |
| 对 memory / skill / signals | 把长期知识、按需能力和外部事件放入不同槽位 |
| 对工具结果 | 通过 handle / spool 控制大结果驻留方式，避免上下文被工具输出撑爆 |

这意味着 Context VM 是 agent 的“虚拟内存管理器”：它决定哪些信息 inline、哪些信息归档、哪些信息只作为下一轮状态注入。

![Context VM & Compaction Mechanisms](/context_vm_mechanisms.svg)

## 概念

`RenderedContext` 四槽位：

| 槽位 | 内容 | 缓存策略 |
|------|------|----------|
| `system_stable` | Identity / system prompt | 长期 cache |
| `system_knowledge` | Memory 检索、Skill 正文、Knowledge | 中期 cache |
| `turns` | 对话历史 | 前缀 frozen，尾部增长 |
| `state_turn` | task_state + signals | 每 turn 重建，不 cache |

`state_turn` 与 history 分离，保证 history 前缀 **字节稳定**，利于 Anthropic prompt cache。

---

## Level 1：只设 token 上限

```python
RuntimeOptions(
    provider=provider,
    session_log=session_log,
    max_tokens=32_000,   # 上下文窗口
    max_turns=25,
)
```

内核 `PressureMonitor` 在压力超阈值时自动触发 `CompressionPipeline`（Snip → Drop → Summarize）。

---

## Level 2：系统提示与初始记忆

```python
RuntimeOptions(
    ...,
    system_prompt="你是一个代码审查助手。",
    initial_memory=["用户偏好：简洁回答"],
)
```

`initial_memory` 写入 knowledge 分区，随 run 启动注入。

---

## Level 3：压缩归档 + 大结果分页

```python
from deepstrike.runtime.archive import ArchiveStore

RuntimeOptions(
    ...,
    compression_store=ArchiveStore("./archives"),
    result_spool=large_result_spool,  # Layer-1 大工具结果 spool
)
```

Handle 表（`mm/handle.rs`）按 residency 投影工具结果 — 热数据 inline，冷数据 page-out，原始 partition 不被 mutation 破坏。

---

## Level 4：Prompt Cache 指纹

内核每轮 render 产出 `PrefixFingerprint`（`renderer.rs`）：

- `system_stable_hash` / `system_knowledge_hash`
- `turn_hashes[]` — 前缀匹配 = cache 可复用

`RuntimeOptions.on_turn_metrics` 可观测 `cache_read_tokens`：

```python
def on_metrics(m):
    print(m.turn, m.cache_read_tokens, m.active_skill)

RuntimeOptions(..., on_turn_metrics=on_metrics)
```

详见 [Prompt Cache 设计](../concepts/prompt-cache-design)。

---

## 内核行为摘要

1. **压缩**：`SnipCompactor` 截断 oversized message → `DropCompactor` 丢弃旧 turn → `SummarizeCompactor` LLM 摘要（SDK 侧 summarizer）
2. **Renewal**：超长期 run 可 handoff（`HandoffArtifact`）
3. **Meta-tools 排除**：`skill`、`memory`、`submit_workflow_nodes` 等不计入 progress footer

---

## 延伸阅读

- [执行平面与工具](./execution-plane-and-tools) — 大工具结果、spool、handle 投影
- [Skill 门控](./skills) — `active_skills` 收窄工具暴露
- [Memory](./memory) — knowledge 分区注入
- 源码：`context/manager.rs`、`context/renderer.rs`
