# Skill

Skill 是 Agent OS 的 **Capability Plane**。它把“能力说明”从默认上下文中移出去，只在 agent 明确需要时通过 meta-tool 加载，并在加载后收窄工具暴露面。

**代码**：
- Kernel：`crates/deepstrike-core/src/context/skill_catalog.rs`
- SDK：`python/deepstrike/skills/registry.py`

---

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 对 Context VM | 只把 skill metadata 放进 stable context，正文按需进入 knowledge / tool result |
| 对工具面 | `allowed_tools` 缩小可见工具集合，降低误调用概率 |
| 对治理面 | Skill gating 发生在 schema 暴露前，可与 Governance 叠加 |
| 对长任务 | 让不同阶段加载不同能力，而不是一次性塞满 system prompt |

Skill 的核心价值不是“多一份 Markdown”，而是让能力成为可寻址、可审计、可门控的 OS 资源。

![Skills Mechanisms](/skills_mechanisms.svg)

## 概念

1. `SkillRegistry.scan()` 扫描 `*.md`，解析 YAML frontmatter → `SkillMetadata`
2. Kernel 注入 `skill` meta-tool，description 嵌入 `<available_skills>` XML
3. Agent 调用 `skill(name="...")` → SDK 读文件正文 → 作为 tool result 返回
4. 加载后进入 `active_skills`，按 `allowed_tools` **收窄** 暴露的工具集

---

## Level 1：目录扫描

创建 skill 文件 `skills/code-review.md`：

```markdown
---
name: code-review
description: Review code for bugs and style issues
when_to_use: When reviewing pull requests
allowed_tools: read_file
---

# Code Review Skill

1. Read the target files
2. Check for bugs, security issues, style
3. Output structured findings
```

启用：

```python
RuntimeOptions(
    ...,
    skill_dir="./skills",
)
```

Runner 启动时 scan 并 `register_skills` 到 kernel。

---

## Level 2：Stable Core 工具

Skill 门控后，默认只暴露 skill 声明的工具 + meta-tools。若需始终保留基础工具：

```python
RuntimeOptions(
    ...,
    skill_dir="./skills",
    stable_core_tool_ids=["read_file", "grep"],
)
```

对应内核 `ContextManager.stable_core_tools`。

---

## Level 3：工具门控 telemetry

```python
def on_metrics(m: TurnMetrics):
    print(f"turn={m.turn} skill={m.active_skill} exposed={m.tools_exposed} called={m.tools_called}")

RuntimeOptions(..., on_turn_metrics=on_metrics)
```

`tools_exposed` vs `tools_called` 量化 over-exposure；连续相同 `active_skill` 测量 dwell time。

---

## Level 4：卸载与租约（K3）

激活不再是单程票。多阶段长任务里，早期阶段的 skill 正文不必永久占据 knowledge 槽位：

```python
RuntimeOptions(..., skill_lease_turns=8)   # 每次激活 8 轮后自动卸载
runner.deactivate_skill("code-review")     # 或宿主显式卸载
```

- 卸载后工具集在下一次 provider 调用**回宽**（与激活同为 epoch 事件，缓存成本同级）
- Skill 正文的 knowledge 钉（键 `skill:<name>`）在下一个 compaction/renewal 边界摘除（缓存安全）
- 之后模型再调 `skill(name)` 会重新激活并重新钉入正文
- **刻意不提供模型侧 unload 工具** —— 卸载只由宿主驱动，避免模型反复装卸抖动
- 租约到期与显式卸载走同一条路径；清扫节奏与 capability lease 相同（每个事件头部）

---

## SkillMetadata 字段

| 字段 | 说明 |
|------|------|
| `name` | 唯一标识 |
| `description` | 出现在 catalog XML |
| `when_to_use` | 可选，帮助模型选型 |
| `allowed_tools` | 加载后允许的工具 id 列表 |
| `effort` | 可选，难度 hint |
| `estimated_tokens` | 估算 token（默认 len/4） |

---

## 内核行为

- Catalog **不存正文** — 仅 `build_tool_schema()` 生成 meta-tool
- `active_skills` 是 `BTreeMap<name, Option<expires_at_turn>>`（多 skill 并存时 union tools；K3 起支持卸载/租约，见 Level 4）
- Skill 正文加载成功后额外钉入 knowledge 分区（键 `skill:<name>`，内核 upsert 跨 wake 去重）
- Skill 是 meta-tool，不计入 `recent_actions` progress log

---

## 延伸阅读

- [Context 工程](./context-engineering)
- Cursor Agent Skills 类似模式；DeepStrike 在内核层门控工具
