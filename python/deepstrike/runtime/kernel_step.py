from __future__ import annotations

import json
from dataclasses import dataclass
from types import SimpleNamespace
from typing import Any

from deepstrike._kernel import ContentPartObj, Message, TaskUpdate, ToolCall, ToolResult, ToolSchema
from deepstrike.providers.base import RenderedContext


KERNEL_ABI_VERSION = 1


@dataclass
class KernelRunnerAction:
  kind: str
  context: RenderedContext | None = None
  tools: list[ToolSchema] | None = None
  calls: list[ToolCall] | None = None
  phase_id: str | None = None
  criteria: list[str] | None = None
  result: Any | None = None


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
  return {
    "call_id": result.call_id,
    "output": result.output,
    "is_error": result.is_error,
    "is_fatal": False,
    "token_count": result.token_count,
  }


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
  return RenderedContext(
    system_text=str(raw.get("system_text") or raw.get("systemText") or ""),
    turns=[_message_from_kernel(m) for m in raw.get("turns", []) or []],
  )


def _action_from_kernel(raw: dict[str, Any]) -> KernelRunnerAction:
  kind = raw.get("kind")
  if kind == "call_provider":
    return KernelRunnerAction(
      kind="call_provider",
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
      calls=[
        ToolCall(
          id=str(c.get("id") or ""),
          name=str(c.get("name") or ""),
          arguments=json.dumps(c.get("arguments") or {}),
        )
        for c in raw.get("calls", []) or []
      ],
    )
  if kind == "evaluate_milestone":
    return KernelRunnerAction(
      kind="evaluate_milestone",
      phase_id=str(raw.get("phase_id") or ""),
      criteria=list(raw.get("criteria") or []),
    )
  if kind == "done":
    result = raw.get("result") or {}
    return KernelRunnerAction(
      kind="done",
      result=SimpleNamespace(
        termination=str(result.get("termination") or "error"),
        turns_used=int(result.get("turns_used") or 0),
        total_tokens_used=int(result.get("total_tokens_used") or 0),
      ),
    )
  raise RuntimeError(f"unknown KernelAction kind: {kind}")


def _step_input(event: dict[str, Any]) -> str:
  return json.dumps({"version": KERNEL_ABI_VERSION, "event": event})


def kernel_apply(runtime: Any, pending: list[dict[str, Any]], event: dict[str, Any]) -> list[dict[str, Any]]:
  step = json.loads(runtime.step(_step_input(event)))
  observations = list(step.get("observations") or [])
  pending.extend(observations)
  return observations


def kernel_action(runtime: Any, pending: list[dict[str, Any]], event: dict[str, Any]) -> KernelRunnerAction:
  step = json.loads(runtime.step(_step_input(event)))
  pending.extend(step.get("observations") or [])
  actions = step.get("actions") or []
  if not actions:
    raise RuntimeError("kernel transition must return one action")
  return _action_from_kernel(actions[0])


def force_compact(runtime: Any, pending: list[dict[str, Any]]) -> bool:
  return any(o.get("kind") == "compressed" for o in kernel_apply(runtime, pending, {"kind": "force_compact"}))
