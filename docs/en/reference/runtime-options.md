---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  node: [RuntimeRunner, LLMProvider, SessionLog, Governance]
  fields:
    "python:RuntimeOptions": [provider, session_log, execution_plane, max_tokens, max_turns, max_total_tokens, timeout_ms, system_prompt, agent_id, compression_store, result_spool, initial_memory, tokenizer, enable_plan_tool, skill_dir, stable_core_tool_ids, dream_store, memory_policy, pre_query_memory, knowledge_source, dream_provider, dream_summarizer, governance, governance_policy, resource_quota, scheduler_budget, run_group, attention_policy, allowed_tool_ids, on_permission_request, provider_for, worktree_manager, sub_agent_orchestrator, sub_agent_harness, is_workflow_node, reducers, milestone_policy, milestone_contract, on_milestone_evaluate, signal_source, os_profile, on_turn_metrics, on_tool_suspend, extensions]
---

# RuntimeOptions Reference

Configuration hub for Python `RuntimeRunner`. Definition: `python/deepstrike/runtime/runner.py`.

## Required

| Field | Type | Description |
|-------|------|-------------|
| `provider` | `LLMProvider` | Default LLM |
| `session_log` | `SessionLog` | Event persistence |

## Basics

| Field | Default | Description |
|-------|---------|-------------|
| `execution_plane` | `LocalExecutionPlane()` | Tool execution |
| `max_tokens` | `32000` | Context window |
| `max_turns` | `25` | Maximum turns |
| `max_total_tokens` | None | Cumulative token cap |
| `timeout_ms` | None | Wall-clock timeout |
| `system_prompt` | None | System prompt |
| `agent_id` | None | Memory / agent identifier |
| `session_id` | Passed at run time | — |

## Context

| Field | Description |
|-------|-------------|
| `compression_store` | Compaction archive `ArchiveStore` |
| `result_spool` | Large tool result spool |
| `initial_memory` | Knowledge injected at startup |
| `tokenizer` | Token counter selection |
| `enable_plan_tool` | Enable `update_plan` meta-tool |

## Skill / Memory / Knowledge

| Field | Description |
|-------|-------------|
| `skill_dir` | Skill `.md` directory |
| `stable_core_tool_ids` | Tools always exposed under Skill gating |
| `dream_store` | Long-term memory store |
| `memory_policy` | Validation and retrieval config |
| `pre_query_memory` | Pre-run memory prefetch hook |
| `knowledge_source` | External knowledge source |
| `dream_provider` | Idle pipeline synthesis LLM |
| `dream_summarizer` | Custom summarizer |

## Governance

| Field | Description |
|-------|-------------|
| `governance` | `Governance` wrapper |
| `governance_policy` | Declarative policy |
| `resource_quota` | Subagent / memory write quota |
| `scheduler_budget` | Scheduler wall-clock budget |
| `run_group` | Cross-run shared governance domain |
| `attention_policy` | Signal attention policy |
| `allowed_tool_ids` | Static tool profile (P0-A) |
| `on_permission_request` | ask_user callback |

## Workflow / Sub-Agent

| Field | Description |
|-------|-------------|
| `provider_for` | Route provider by `model_hint` |
| `worktree_manager` | Worktree isolation |
| `sub_agent_orchestrator` | Custom spawn logic |
| `sub_agent_harness` | Sub-agent harness retries |
| `is_workflow_node` | Mark workflow node runner |
| `reducers` | Custom reduce nodes |
| `milestone_policy` | Milestone policy |
| `milestone_contract` | Top-level milestone |
| `on_milestone_evaluate` | Milestone evaluation callback |

## Signals / Reactive

| Field | Description |
|-------|-------------|
| `signal_source` | `SignalGateway`, etc. |
| `os_profile` | OS-native profile |

## Observability

| Field | Description |
|-------|-------------|
| `on_turn_metrics` | Per-turn telemetry (`TurnMetrics`) |
| `on_tool_suspend` | Tool suspend callback |
| `extensions` | Extension dict passed to provider |

## Related types

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

Full fields are defined in source; this document is updated as versions evolve.
