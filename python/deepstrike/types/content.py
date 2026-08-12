"""Canonical structured tool content for the pure-Python runtime.

Mirrors the Node TS-only design (spc_012-N-01): the pyo3 `ContentPartObj` binding
deliberately gained no new field (spc_012-R-07 premise correction — that binding only
serves the kernel-JSON conversion path, which is text-only by design). Structured tool
output therefore lives on this pure-Python shim, duck-typed against `ContentPartObj`.
The runner carries non-durable blocks only in an operation-scoped overlay, so reusing
the Runner cannot leak blocks across sessions.

ContentBlocks are plain dicts (no class hierarchy — they only ever cross into provider
serializers, which pattern-match on `block["type"]`):

  {"type": "text", "text": str}
  {"type": "image", "source": {"kind": "base64", "data": str} | {"kind": "url", "url": str}, "media_type": str?}
  {"type": "audio", "source": {...}, "media_type": str?}
"""
from __future__ import annotations

from dataclasses import dataclass
import base64
import binascii
from typing import Any


class ContentValidationError(ValueError):
  pass


class ToolResultProjectionConflictError(ContentValidationError):
  pass


@dataclass(frozen=True)
class CanonicalToolResult:
  call_id: str
  blocks: tuple[dict, ...]
  is_error: bool


def _require_non_empty(value: Any, label: str) -> None:
  if not isinstance(value, str) or not value:
    raise ContentValidationError(f"{label} must be a non-empty string")


def validate_tool_output_blocks(blocks: list[dict] | tuple[dict, ...]) -> None:
  for block in blocks:
    if not isinstance(block, dict):
      raise ContentValidationError("tool output block must be an object")
    kind = block.get("type")
    if kind == "text":
      if not isinstance(block.get("text"), str):
        raise ContentValidationError("tool output text must be a string")
      continue
    if kind == "tool_result":
      raise ContentValidationError("nested tool_result blocks are forbidden")
    if kind not in {"image", "audio", "video", "file"}:
      raise ContentValidationError(f"unknown tool output block type: {kind}")
    source = block.get("source")
    if not isinstance(source, dict):
      raise ContentValidationError(f"{kind} source is required")
    source_kind = source.get("kind")
    field = {"url": "url", "base64": "data", "fileId": "id", "object": "handle"}.get(source_kind)
    if field is None:
      raise ContentValidationError(f"{kind} source kind is invalid")
    _require_non_empty(source.get(field), f"{kind} {source_kind}")
    if source_kind == "base64":
      try:
        base64.b64decode(source[field], validate=True)
      except (binascii.Error, ValueError) as exc:
        raise ContentValidationError(f"{kind} base64 data is not valid base64") from exc


def project_tool_output_to_text(blocks: list[dict] | tuple[dict, ...]) -> str:
  validate_tool_output_blocks(blocks)
  return "\n".join(
    block["text"] if block["type"] == "text" else f"[{block['type']}]"
    for block in blocks
  )


def normalize_tool_result(
  call_id: str,
  output: str,
  is_error: bool,
  content_parts: list[dict] | None,
) -> CanonicalToolResult:
  if content_parts is None:
    return CanonicalToolResult(call_id, ({"type": "text", "text": output},), is_error)
  projection = project_tool_output_to_text(content_parts)
  if projection != output:
    raise ToolResultProjectionConflictError(
      f"Tool result projection conflict for {call_id}: output does not match content_parts"
    )
  return CanonicalToolResult(call_id, tuple(content_parts), is_error)


@dataclass
class StructuredToolResultPart:
  """Legacy carrier normalized and validated at provider boundaries."""
  type: str = "tool_result"
  call_id: str = ""
  output: str = ""
  is_error: bool = False
  content_parts: list[dict] | None = None


@dataclass
class RenderedMessage:
  """Duck-typed stand-in for the pyo3 `_kernel.Message` on the provider-facing rendered
  path. The pyo3 Message constructor enforces `ContentPartObj` parts, so a structured
  tool_result part (a pure-Python `StructuredToolResultPart`) cannot be embedded in one;
  the runner's operation-scoped overlay attachment
  therefore swaps affected messages for this plain carrier. Provider serializers
  (`providers/base.py` `to_anthropic_messages`/`to_openai_message_params`, gemini/ollama
  equivalents) are all attribute-based (`getattr`/`msg.role`/...), never
  `isinstance(msg, Message)` — duck-typing is sufficient and no provider is
  isinstance-strict on turns."""
  role: str = "user"
  content: str = ""
  token_count: int | None = None
  tool_calls: list | None = None
  content_parts: list | None = None
