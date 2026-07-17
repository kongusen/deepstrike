from __future__ import annotations

import json
import re
import time
import uuid
import weakref
from dataclasses import dataclass
from types import SimpleNamespace
from typing import Any

from deepstrike._kernel import ContentPartObj, Message, TaskUpdate, ToolCall, ToolResult, ToolSchema
from deepstrike.providers.base import ContextBudgetOverflow, RenderedContext


KERNEL_ABI_VERSION = 2
# Keyed by the runtime OBJECT, not id(runtime): a plain dict keyed by id() aliases recycled
# addresses (a new runtime inherits a dead one's operation identity — fatal once a durable log
# keys chains by that identity) and leaks an entry per runtime. WeakKeyDictionary mirrors the
# Node/WASM WeakMap; the pyo3 KernelRuntime declares `weakref` support for exactly this.
_wire_states: "weakref.WeakKeyDictionary[Any, tuple[str, int]]" = weakref.WeakKeyDictionary()


def snapshot_kernel_runtime(runtime: Any) -> dict[str, Any]:
  return json.loads(runtime.snapshot())


def restore_kernel_runtime(runtime: Any, snapshot: dict[str, Any]) -> None:
  runtime.restore(json.dumps(snapshot))
  operation_id = snapshot.get("operation_id")
  if not operation_id:
    _wire_states.pop(runtime, None)
    return
  next_sequence = 1
  for accepted in snapshot.get("accepted_inputs") or []:
    match = re.search(r"-event-(\d+)$", str(accepted.get("event_id") or ""))
    if match:
      next_sequence = max(next_sequence, int(match.group(1)) + 1)
  _wire_states[runtime] = (str(operation_id), next_sequence)


@dataclass
class KernelRunnerAction:
  kind: str
  effect_id: str = ""
  context: RenderedContext | None = None
  tools: list[ToolSchema] | None = None
  calls: list[ToolCall] | None = None
  phase_id: str | None = None
  criteria: list[str] | None = None
  required_evidence: list[str] | None = None
  result: Any | None = None
  requests: list[dict[str, Any]] | None = None
  nodes: list[dict[str, Any]] | None = None
  budget: dict[str, Any] | None = None
  agent_ids: list[str] | None = None
  reason: str | None = None
  memory: dict[str, Any] | None = None
  query: dict[str, Any] | None = None
  requested_k: int | None = None
  call_id: str | None = None
  tool: str | None = None
  output: str | None = None
  original_size: int | None = None
  preview_size: int | None = None
  turn: int | None = None
  action: str | None = None
  summary: str | None = None
  archived: list[Message] | None = None
  tier: str | None = None


def _try_parse_json(value: str) -> Any:
  try:
    return json.loads(value)
  except Exception:
    return {}


def tool_schema_to_kernel(schema: ToolSchema) -> dict[str, Any]:
  return {
    "name": schema.name,
    "description": schema.description,
    "parameters": _try_parse_json(schema.parameters),
  }


def tool_result_to_kernel(result: ToolResult) -> dict[str, Any]:
  out = {
    "call_id": result.call_id,
    "output": result.output,
    "is_error": result.is_error,
    "is_fatal": getattr(result, "is_fatal", False),
    "token_count": result.token_count,
  }
  error_kind = getattr(result, "error_kind", None)
  if error_kind is not None:
    out["error_kind"] = error_kind
  return out


def task_update_to_kernel(update: TaskUpdate) -> dict[str, Any]:
  return {
    "plan": update.plan,
    "current_step": update.current_step,
    "progress": update.progress,
    "scratchpad": update.scratchpad,
    "blocked_on": update.blocked_on,
    "preserved_refs": update.preserved_refs,
  }


def skill_metadata_to_kernel(skill: Any) -> dict[str, Any]:
  out: dict[str, Any] = {
    "name": skill.name,
    "description": skill.description,
    "estimated_tokens": getattr(skill, "estimated_tokens", 0) or 0,
  }
  when_to_use = getattr(skill, "when_to_use", None)
  effort = getattr(skill, "effort", None)
  if when_to_use:
    out["when_to_use"] = when_to_use
  if effort is not None:
    out["effort"] = effort
  # P1-B: forward declared tool ids (additive; omitted when empty so existing skills' wire is unchanged).
  allowed_tools = getattr(skill, "allowed_tools", None)
  if allowed_tools:
    out["allowed_tools"] = list(allowed_tools)
  return out


def message_to_kernel(message: Message) -> dict[str, Any]:
  out: dict[str, Any] = {
    "role": message.role,
    "tool_calls": [
      {"id": c.id, "name": c.name, "arguments": _try_parse_json(c.arguments)}
      for c in (message.tool_calls or [])
    ],
  }
  if message.token_count is not None:
    out["token_count"] = message.token_count
  if message.content_parts:
    parts = []
    for part in message.content_parts:
      if part.type == "text":
        parts.append({"type": "text", "text": part.text or ""})
      elif part.type == "tool_result":
        parts.append({
          "type": "tool_result",
          "call_id": part.call_id,
          "output": part.output or "",
          "is_error": bool(part.is_error),
        })
      elif part.type == "image":
        parts.append({
          "type": "image",
          "url": part.url,
          "data": part.data,
          "media_type": part.media_type,
          "detail": part.detail,
        })
      elif part.type == "audio":
        parts.append({
          "type": "audio",
          "data": part.data or "",
          "media_type": part.media_type or "audio/wav",
        })
    out["content"] = parts
  else:
    out["content"] = message.content
  return out


def capability_tool(schema: ToolSchema) -> dict[str, Any]:
  return {
    "id": schema.name,
    "kind": "tool",
    "description": schema.description,
    "tool_schema": tool_schema_to_kernel(schema),
  }


def capability_skill(name: str, description: str) -> dict[str, Any]:
  return {
    "id": name,
    "kind": "skill",
    "description": description,
    "skill": {"name": name, "description": description, "estimated_tokens": 0},
  }


def capability_marker(kind: str, id: str, description: str) -> dict[str, Any]:
  return {"id": id, "kind": kind, "description": description}


def _content_parts_from_kernel(parts: list[dict[str, Any]]) -> list[ContentPartObj]:
  out: list[ContentPartObj] = []
  for part in parts:
    kind = part.get("type")
    if kind == "text":
      out.append(ContentPartObj(type="text", text=str(part.get("text") or "")))
    elif kind == "tool_result":
      out.append(ContentPartObj(
        type="tool_result",
        call_id=str(part.get("call_id") or ""),
        output=str(part.get("output") or ""),
        is_error=bool(part.get("is_error")),
      ))
    elif kind == "image":
      out.append(ContentPartObj(
        type="image",
        url=part.get("url"),
        data=part.get("data"),
        media_type=part.get("media_type"),
        detail=part.get("detail"),
      ))
    elif kind == "audio":
      out.append(ContentPartObj(
        type="audio",
        data=str(part.get("data") or ""),
        media_type=str(part.get("media_type") or "audio/wav"),
      ))
  return out


def _message_from_kernel(raw: dict[str, Any]) -> Message:
  content = raw.get("content", "")
  content_parts = _content_parts_from_kernel(content) if isinstance(content, list) else None
  text = (
    "".join(str(p.get("text") or "") for p in content if isinstance(p, dict) and p.get("type") == "text")
    if isinstance(content, list)
    else str(content or "")
  )
  return Message(
    role=str(raw.get("role") or "user"),
    content=text,
    token_count=raw.get("token_count"),
    tool_calls=[
      ToolCall(
        id=str(c.get("id") or ""),
        name=str(c.get("name") or ""),
        arguments=json.dumps(c.get("arguments") or {}),
      )
      for c in raw.get("tool_calls", []) or []
    ],
    content_parts=content_parts,
  )


def _context_from_kernel(raw: dict[str, Any]) -> RenderedContext:
  state_raw = raw.get("state_turn") or raw.get("stateTurn")
  frozen_raw = raw.get("frozen_prefix_len")
  if frozen_raw is None:
    frozen_raw = raw.get("frozenPrefixLen")
  overflow_raw = raw.get("budget_overflow") or raw.get("budgetOverflow")
  return RenderedContext(
    system_text=str(raw.get("system_text") or raw.get("systemText") or ""),
    system_stable=str(raw.get("system_stable") or raw.get("systemStable") or ""),
    system_knowledge=str(raw.get("system_knowledge") or raw.get("systemKnowledge") or ""),
    turns=[_message_from_kernel(m) for m in raw.get("turns", []) or []],
    state_turn=_message_from_kernel(state_raw) if state_raw else None,
    frozen_prefix_len=int(frozen_raw) if isinstance(frozen_raw, (int, float)) else None,
    budget_overflow=ContextBudgetOverflow(
      kind=str(overflow_raw.get("kind") or ""),
      required_tokens=int(overflow_raw.get("required_tokens") or overflow_raw.get("requiredTokens") or 0),
      max_tokens=int(overflow_raw.get("max_tokens") or overflow_raw.get("maxTokens") or 0),
    ) if isinstance(overflow_raw, dict) else None,
  )


def _action_from_kernel(raw: dict[str, Any]) -> KernelRunnerAction:
  kind = raw.get("kind")
  effect_id = str(raw.get("effect_id") or "")
  if not effect_id:
    raise RuntimeError(f"kernel action {kind} is missing effect_id")
  if kind == "call_provider":
    return KernelRunnerAction(
      kind="call_provider",
      effect_id=effect_id,
      context=_context_from_kernel(raw.get("context") or {}),
      tools=[
        ToolSchema(
          name=str(t.get("name") or ""),
          description=str(t.get("description") or ""),
          parameters=json.dumps(t.get("parameters") or {}),
        )
        for t in raw.get("tools", []) or []
      ],
    )
  if kind == "execute_tool":
    return KernelRunnerAction(
      kind="execute_tool",
      effect_id=effect_id,
      calls=[
        ToolCall(
          id=str(c.get("id") or ""),
          name=str(c.get("name") or ""),
          arguments=json.dumps(c.get("arguments") or {}),
        )
        for c in raw.get("calls", []) or []
      ],
    )
  if kind == "request_approval":
    return KernelRunnerAction(
      kind=kind,
      effect_id=effect_id,
      requests=[{
        "call_id": str(request.get("call_id") or ""),
        "tool": str(request.get("tool") or ""),
        "arguments": json.dumps(request.get("arguments") or {}),
        "reason": str(request.get("reason") or ""),
      } for request in raw.get("requests", []) or []],
    )
  if kind == "spawn_workflow":
    return KernelRunnerAction(
      kind=kind, effect_id=effect_id,
      nodes=list(raw.get("nodes") or []), budget=raw.get("budget"),
    )
  if kind == "preempt_sub_agents":
    return KernelRunnerAction(
      kind=kind, effect_id=effect_id,
      agent_ids=list(raw.get("agent_ids") or []), reason=str(raw.get("reason") or ""),
    )
  if kind == "persist_memory":
    return KernelRunnerAction(kind=kind, effect_id=effect_id, memory=dict(raw.get("memory") or {}))
  if kind == "query_memory":
    return KernelRunnerAction(
      kind=kind, effect_id=effect_id, query=dict(raw.get("query") or {}),
      requested_k=int(raw.get("requested_k") or 0),
    )
  if kind == "spool_large_result":
    return KernelRunnerAction(
      kind=kind, effect_id=effect_id,
      call_id=str(raw.get("call_id") or ""), tool=str(raw.get("tool") or ""),
      output=str(raw.get("output") or ""), original_size=int(raw.get("original_size") or 0),
      preview_size=int(raw.get("preview_size") or 0),
    )
  if kind == "archive_page_out":
    return KernelRunnerAction(
      kind=kind, effect_id=effect_id, turn=int(raw.get("turn") or 0),
      action=str(raw.get("action") or "auto_compact"), summary=raw.get("summary"),
      archived=[_message_from_kernel(message) for message in raw.get("archived", []) or []],
      tier=str(raw.get("tier") or "durable"),
    )
  if kind == "evaluate_milestone":
    return KernelRunnerAction(
      kind="evaluate_milestone",
      effect_id=effect_id,
      phase_id=str(raw.get("phase_id") or ""),
      criteria=list(raw.get("criteria") or []),
      required_evidence=list(raw.get("required_evidence") or []),
    )
  if kind == "done":
    result = raw.get("result") or {}
    # ③ loop-agent: the kernel-adjudicated after-round decision (absent on non-loop runs).
    pace = result.get("pace_decision")
    pace_decision = (
      {
        "action": str(pace.get("action") or "stop"),
        "delay_ms": pace.get("delay_ms"),
        "reason": str(pace.get("reason") or ""),
        "coerced_from": pace.get("coerced_from"),
      }
      if isinstance(pace, dict)
      else None
    )
    return KernelRunnerAction(
      kind="done",
      effect_id=effect_id,
      result=SimpleNamespace(
        termination=str(result.get("termination") or "error"),
        turns_used=int(result.get("turns_used") or 0),
        total_tokens_used=int(result.get("total_tokens_used") or 0),
        pace_decision=pace_decision,
      ),
    )
  raise RuntimeError(f"unknown KernelAction kind: {kind}")


def _step_input(runtime: Any, event: dict[str, Any]) -> str:
  state = _wire_states.get(runtime)
  if state is None:
    state = (f"python-operation-{uuid.uuid4()}", 1)
  operation_id, sequence = state
  _wire_states[runtime] = (operation_id, sequence + 1)
  correlated_event = (
    {**event, "operation_id": operation_id}
    if event.get("kind") == "cancel_operation"
    else event
  )
  return json.dumps({
    "version": KERNEL_ABI_VERSION,
    "operation_id": operation_id,
    "event_id": f"{operation_id}-event-{sequence}",
    "observed_at_ms": int(time.time() * 1000),
    "event": correlated_event,
  })


def _kernel_step(runtime: Any, event: dict[str, Any]) -> dict[str, Any]:
  step = json.loads(runtime.step(_step_input(runtime, event)))
  faults = step.get("faults") or []
  if faults:
    fault = faults[0]
    raise RuntimeError(f"{fault.get('code', 'kernel_fault')}: {fault.get('message', 'kernel transition failed')}")
  return step


def kernel_apply(runtime: Any, pending: list[dict[str, Any]], event: dict[str, Any]) -> list[dict[str, Any]]:
  step = _kernel_step(runtime, event)
  observations = list(step.get("observations") or [])
  pending.extend(observations)
  return observations


def kernel_action(runtime: Any, pending: list[dict[str, Any]], event: dict[str, Any]) -> KernelRunnerAction:
  step = _kernel_step(runtime, event)
  pending.extend(step.get("observations") or [])
  actions = step.get("actions") or []
  if not actions:
    raise RuntimeError("kernel transition must return one action")
  return _action_from_kernel(actions[0])


def kernel_maybe_action(
  runtime: Any,
  pending: list[dict[str, Any]],
  event: dict[str, Any],
) -> KernelRunnerAction | None:
  step = _kernel_step(runtime, event)
  pending.extend(step.get("observations") or [])
  actions = step.get("actions") or []
  if not actions:
    return None
  return _action_from_kernel(actions[0])
