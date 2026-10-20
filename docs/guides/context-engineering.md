# Context 工程

Context 工程决定 Agent 工作时哪些信息应该保留。它把身份、Knowledge、对话历史、召回 Memory 和临时任务状态分开，让长任务持续可用。

**代码**：`crates/deepstrike-core/src/context/`（`ContextManager`、`renderer`、`compression`）

---

## Agent Context 中有什么

| Context 区域 | 内容 |
| --- | --- |
| 稳定指令 | Agent 身份、系统规则和长期任务框架 |
| Knowledge | 已加载 Skill、固定参考和 initial memory |
| History | 对话 turn、工具结果和检索事实 |
| 当前状态 | 计划、阻塞原因、Signal 和下一步动作 |

DeepStrike 会压缩旧 turn、保持稳定前缀适合缓存，并分页处理大工具结果，避免每轮 prompt 被结果撑满。

![Context 管理与压缩机制](/context_vm_mechanisms.svg)

## 概念

`RenderedContext` 四槽位：

| 槽位 | 内容 | 缓存策略 |
|------|------|----------|
| `system_stable` | Identity / system prompt | 长期 cache |
| `system_knowledge` | Skill 正文、`initial_memory`、宿主钉住的耐久参考（见 Level 5） | 中期 cache |
| `turns` | 对话历史（含运行时 memory/knowledge 检索命中与预取） | 前缀 frozen，尾部增长 |
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
from deepstrike import PayloadStore

RuntimeOptions(
    ...,
    compression_store=ArchiveStore("./archives"),
    payload_store=PayloadStore("./payloads"),
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

## Level 5：Knowledge 生命周期（严格动态控制）

`knowledge` 分区是**耐久槽位**（渲染进带缓存断点的 `system[1]`），但耐久 ≠ 永生。内容按生命周期路由：

| 内容类型 | 去向 | 生命周期 |
| ------ | ------ | ------ |
| Skill 正文（方法类，整个 run 复用） | `knowledge`，键 `skill:<name>` | 常驻，直到 deactivate / lease 到期 |
| memory / knowledge 工具检索命中（事实类，用完即弃） | `history` 普通轮次 | 随压缩金字塔自然衰减 |
| `pre_query_memory` 预取 | `history` 普通轮次 | 同上；renewal 后自动重查 |
| 宿主钉住的参考材料 | `knowledge`（`push_knowledge(key=..., pinned=True)`） | pinned 不受预算驱逐 |

**键控条目（K1）**：`push_knowledge(content, key="ref")` 同键重推为 upsert，`remove_knowledge("ref")` 定向移除。两者都**延迟到下一个 compaction/renewal 边界生效** —— `system[1]` 的既有字节只在 prompt-cache 前缀反正要重写的时刻改动（追加除外：追加只是延长缓存前缀，立即可见）。

**知识预算（K2）**：`knowledge_budget_ratio`（默认 0.25 × max_tokens，0 关闭）。超限时发出一次 `knowledge_budget_exceeded` observation，并把**最旧的未钉住、非 skill** 条目标记为边界驱逐；pinned 与 skill 钉永不被预算驱逐。

**Skill 卸载 / 租约（K3）**：`deactivate_skill(name)`（仅宿主驱动，无模型侧 unload）—— 工具集在下一次 provider 调用回宽，knowledge 钉在下一个边界摘除；`skill_lease_turns=N` 让每次激活 N 轮后自动走同一条路径。多阶段长任务不再单调累积早期阶段的 skill。

**Renewal 记忆重查（K4）**：sprint renewal 会整体重建 history（连同早先的 memory 命中一起丢弃），`pre_query_memory` 钩子会以 `phase="renewal"` 自动重发一次，让新 sprint 从新鲜的召回开始（turn-1 那次为 `phase="initial"`）。

```python
RuntimeOptions(
    ...,
    knowledge_budget_ratio=0.2,   # K2：knowledge 最多占窗口 20%
    skill_lease_turns=8,          # K3：skill 激活 8 轮后自动卸载
)
runner.push_knowledge("API 速查表…", key="api-ref", pinned=True)   # K1：键控 + 钉住
runner.remove_knowledge("api-ref")                                  # 下一边界移除
runner.deactivate_skill("debug")                                    # K3：显式卸载
```

---

## 运行时行为摘要

1. **压缩**：Snip 截断过长消息，Micro 缩减旧工具结果，Collapse / Auto 归档较低价值的旧对话，并由内核 `RuleSummarizer` 生成确定性摘要。无需配置 LLM 摘要器；Node 的 `asyncSummarizer` 可在后台升级摘要。
2. **Renewal**：分级压缩后仍超过续期阈值，才重建 history。按 carryover 预算保留近期完整对话事务，避免拆开工具调用与结果；单个事务放不进预算时不会保留半个事务。系统、Knowledge 与任务状态继续保留。
3. **Meta-tools 排除**：`skill`、`memory`、`submit_workflow_nodes` 等不计入 progress footer

### Node 上下文预算与恢复

`contextPolicy` 的阈值、`targetAfterCompress`、`preserveRecentTurns`、`renewalCarryover` 和 idle 设置均在创建 kernel 时安装。大段继承历史在首次 Provider 请求前向压缩目标收缩；近期保留下限和不可压缩的系统、任务状态可能使结果高于目标。

Node 的条目、系统提示、Knowledge 和请求启发式预算统一使用 kernel 的 cl100k BPE 加 10% 余量。它不是 DeepSeek 等模型的原生 tokenizer。适配器提供已编码请求时，请求估算包含最终 body，包括回放的 reasoning 字段；自定义 adapter-input Provider 则估算 `context` 与工具 schema。

默认 tokenizer 的只读表在操作间共享，避免每次启动或恢复重新载入。长文本按有界 UTF-8 分块计数和截断，限制无空格输出的 BPE 合并成本；分块边界可能略微改变估算，因此仍标记为启发式值。

Provider 实际 input usage 写入同一 fingerprint 的 `postflight` 测量，并反馈压力判断。为避免已裁剪的请求掩盖完整内部历史，压缩取实际观测压力与驻留内容估算压力的较大值；压缩或续期改写上下文后废弃旧请求的压力观测。真实计费仍只使用 Provider usage。

---

## 延伸阅读

- [执行平面与工具](./execution-plane-and-tools) — 大工具结果、external payload、handle 投影
- [Skill 门控](./skills) — `active_skills` 收窄工具暴露
- [Memory](./memory) — knowledge 分区注入
- 源码：`context/manager.rs`、`context/renderer.rs`
