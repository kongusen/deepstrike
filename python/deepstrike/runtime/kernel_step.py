from __future__ import annotations

import base64
import json
from dataclasses import dataclass
from typing import Any

from deepstrike._kernel import ContentPartObj, Message, TaskUpdate, ToolCall, ToolResult, ToolSchema
from deepstrike.providers.base import ContextBudgetOverflow, RenderedContext


CANONICAL_CONTENT_PARTS_PREFIX = "[[deepstrike-content-parts:v1]]"


def encode_canonical_content_parts(parts: list[Any]) -> str:
  """Encode multimodal content parts for ABI-v3 wire (mirrors Node ``encodeCanonicalContentParts``)."""
  payload = base64.urlsafe_b64encode(
    json.dumps(parts, separators=(",", ":")).encode("utf-8"),
  ).decode("ascii").rstrip("=")
  return f"{CANONICAL_CONTENT_PARTS_PREFIX}{payload}"


def decode_canonical_content_parts(content: str) -> list[dict[str, Any]] | None:
  if not content.startswith(CANONICAL_CONTENT_PARTS_PREFIX):
    return None
  raw = content[len(CANONICAL_CONTENT_PARTS_PREFIX):]
  pad = "=" * (-len(raw) % 4)
  try:
    decoded = json.loads(base64.urlsafe_b64decode(raw + pad).decode("utf-8"))
  except (ValueError, json.JSONDecodeError):
    return None
  if not isinstance(decoded, list):
    return None
  return [part for part in decoded if isinstance(part, dict)]
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
  handle_id: str | None = None
  payload_ref: str | None = None
  attempts: list[dict[str, Any]] | None = None
  effect_kind: str | None = None


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
  canonical_parts = decode_canonical_content_parts(content) if isinstance(content, str) else None
  structured = canonical_parts if canonical_parts is not None else (content if isinstance(content, list) else None)
  content_parts = _content_parts_from_kernel(structured) if isinstance(structured, list) else None
  if canonical_parts is not None:
    text = "".join(str(p.get("text") or "") for p in canonical_parts if p.get("type") == "text")
  elif isinstance(content, list):
    text = "".join(
      str(p.get("text") or "") for p in content if isinstance(p, dict) and p.get("type") == "text"
    )
  else:
    text = str(content or "")
  return Message(
    role=str(raw.get("role") or "user"),
    content=text,
    token_count=raw.get("token_count") if raw.get("token_count") is not None else raw.get("tokens"),
    tool_calls=[
      ToolCall(
        id=str(c.get("id") or c.get("call_id") or ""),
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
