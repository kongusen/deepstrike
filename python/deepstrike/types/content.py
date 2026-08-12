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
from typing import Any, TYPE_CHECKING

if TYPE_CHECKING:
  from deepstrike.providers.base import RenderedContext
  from deepstrike._kernel import ToolSchema


class ContentValidationError(ValueError):
  pass


class ToolResultProjectionConflictError(ContentValidationError):
  pass


@dataclass(frozen=True)
class CanonicalToolResult:
  call_id: str
  blocks: tuple[dict, ...]
  is_error: bool


@dataclass(frozen=True)
class CanonicalAdapterInput:
  """Provider-ready input with one validated content representation.

  The existing Python bindings remain the public carriers. This boundary validates
  their content tree before a protocol serializer projects it to a vendor wire.
  """
  context: "RenderedContext"
  tools: tuple["ToolSchema", ...]
  extensions: dict[str, Any]


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


def _validate_media_part(part: Any) -> None:
  kind = getattr(part, "type", "unknown")
  data = getattr(part, "data", None)
  url = getattr(part, "url", None)
  if bool(data) == bool(url):
    raise ContentValidationError(f"{kind} source must contain exactly one of data or url")
  _require_non_empty(data if data else url, f"{kind} source")


def validate_rendered_message(message: Any) -> None:
  """Validate one legacy message before its canonical provider projection."""
  parts = getattr(message, "content_parts", None) or []
  for part in parts:
    kind = getattr(part, "type", "unknown")
    if kind == "text":
      if not isinstance(getattr(part, "text", None), str):
        raise ContentValidationError("text content must be a string")
    elif kind in {"image", "audio"}:
      _validate_media_part(part)
    elif kind == "tool_result":
      output = getattr(part, "output", "")
      if not isinstance(output, str):
        raise ContentValidationError("tool result output must be a string")
      normalize_tool_result(
        getattr(part, "call_id", ""),
        output,
        bool(getattr(part, "is_error", False)),
        getattr(part, "content_parts", None),
      )
    else:
      raise ContentValidationError(f"unknown content part type: {kind}")


def validate_rendered_context(context: "RenderedContext") -> None:
  for message in [*context.turns, *([context.state_turn] if context.state_turn is not None else [])]:
    validate_rendered_message(message)


def normalize_canonical_adapter_input(
  context: "RenderedContext",
  tools: list["ToolSchema"] | tuple["ToolSchema", ...],
  *,
  extensions: dict[str, Any] | None = None,
) -> CanonicalAdapterInput:
  """Validate compatibility carriers once before protocol-specific projection."""
  validate_rendered_context(context)
  return CanonicalAdapterInput(context=context, tools=tuple(tools), extensions=dict(extensions or {}))


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
