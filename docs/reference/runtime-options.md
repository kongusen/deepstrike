---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  node: [RuntimeRunner, LLMProvider, SessionLog, Governance]
  fields:
    "python:RuntimeOptions": [provider, session_log, execution_plane, max_tokens, max_turns, max_total_tokens, timeout_ms, system_prompt, agent_id, compression_store, result_spool, initial_memory, tokenizer, enable_plan_tool, knowledge_budget_ratio, skill_dir, stable_core_tool_ids, skill_lease_turns, dream_store, memory_policy, pre_query_memory, knowledge_source, dream_provider, dream_summarizer, governance, governance_policy, resource_quota, scheduler_budget, run_group, attention_policy, allowed_tool_ids, on_permission_request, repeat_fuse, criteria_gate, provider_for, worktree_manager, sub_agent_orchestrator, sub_agent_harness, is_workflow_node, reducers, milestone_policy, milestone_contract, on_milestone_evaluate, signal_source, os_profile, on_turn_metrics, on_tool_suspend, extensions]
---

# RuntimeOptions 参考

Python `RuntimeRunner` 的配置中心。定义：`python/deepstrike/runtime/runner.py`。

## 必填

| 字段 | 类型 | 说明 |
|------|------|------|
| `provider` | `LLMProvider` | 默认 LLM |
| `session_log` | `SessionLog` | 事件持久化 |

## 基础

| 字段 | 默认 | 说明 |
|------|------|------|
| `execution_plane` | `LocalExecutionPlane()` | 工具执行 |
| `max_tokens` | `32000` | 上下文窗口 |
| `max_turns` | `25` | 最大回合 |
| `max_total_tokens` | None | 累计 token 上限 |
| `timeout_ms` | None | 墙钟超时 |
| `system_prompt` | None | 系统提示 |
| `agent_id` | None | Memory / agent 标识 |
| `session_id` | run 时传入 | — |

## Context

| 字段 | 说明 |
|------|------|
| `compression_store` | 压缩归档 `ArchiveStore` |
| `result_spool` | 大工具结果 spool |
| `initial_memory` | 启动注入 knowledge |
| `tokenizer` | token 计数器选择 |
| `enable_plan_tool` | 启用 `update_plan` meta-tool |
| `knowledge_budget_ratio` | K2：knowledge 分区最大占窗口比例（默认 0.25，0 关闭）；超限边界驱逐最旧未钉住非 skill 条目 |

## Skill / Memory / Knowledge

| 字段 | 说明 |
|------|------|
| `skill_dir` | Skill `.md` 目录 |
| `stable_core_tool_ids` | Skill 门控下始终暴露的工具 |
| `skill_lease_turns` | K3：skill 激活 N 轮后自动卸载（工具集回宽 + knowledge 钉边界摘除）；None=永久 |
| `dream_store` | 长期记忆 store |
| `memory_policy` | 校验与 retrieval 配置 |
| `pre_query_memory` | run 前 memory 预取钩子 |
| `knowledge_source` | 外部知识源 |
| `dream_provider` | idle pipeline 合成 LLM |
| `dream_summarizer` | 自定义 summarizer |

## Governance

| 字段 | 说明 |
|------|------|
| `governance` | `Governance` wrapper |
| `governance_policy` | 声明式策略 |
| `resource_quota` | subagent / memory write 配额 |
| `scheduler_budget` | 调度器墙钟预算 |
| `run_group` | 跨 run 共享治理域 |
| `attention_policy` | 信号注意力策略 |
| `allowed_tool_ids` | 静态工具 profile（P0-A） |
| `on_permission_request` | ask_user 回调 |
| `repeat_fuse` | O6：同签名重复调用熔断（默认开；dict 调阈值，False 关闭） |
| `criteria_gate` | O4：完成前 criteria 自检门（默认开；False 无条件接受首次完成） |

## Workflow / Sub-Agent

| 字段 | 说明 |
|------|------|
| `provider_for` | 按 `model_hint` 路由 provider |
| `worktree_manager` | worktree 隔离 |
| `sub_agent_orchestrator` | 自定义 spawn 逻辑 |
| `sub_agent_harness` | 子 agent harness 重试 |
| `is_workflow_node` | 标记 workflow 节点 runner |
| `reducers` | 自定义 reduce 节点 |
| `milestone_policy` | milestone 策略 |
| `milestone_contract` | 顶层 milestone |
| `on_milestone_evaluate` | milestone 评估回调 |

## Signals / Reactive

| 字段 | 说明 |
|------|------|
| `signal_source` | `SignalGateway` 等 |
| `os_profile` | OS 原生 profile |

## Observability

| 字段 | 说明 |
|------|------|
| `on_turn_metrics` | 每 turn telemetry（`TurnMetrics`） |
| `on_tool_suspend` | 工具挂起回调 |
| `extensions` | 传给 provider 的扩展 dict |

## 相关类型

```python
@dataclass
class TurnMetrics:
    turn: int
    tools_exposed: int
    tools_called: int
    input_tokens: int
    cache_read_tokens: int
    cache_creation_tokens: int
    active_skill: str | None = None
```

完整字段以源码为准；本文档随版本迭代更新。
