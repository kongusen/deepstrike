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
  from deepstrike.providers.model_registry import ResolvedProviderRuntime


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
  resolved: "ResolvedProviderRuntime | None" = None


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


def _validate_file_part(part: Any, resolved: "ResolvedProviderRuntime | None") -> None:
  _require_non_empty(getattr(part, "file_id", None), "fileId")
  _require_non_empty(getattr(part, "provider_id", None), "fileId providerId")
  _require_non_empty(getattr(part, "endpoint_id", None), "fileId endpointId")
  _reject_unsupported_capability(resolved, "file", "fileId")
  if resolved is not None and (
    part.provider_id != resolved.provider_id or part.endpoint_id != resolved.endpoint_id
  ):
    raise ContentValidationError(
      f"Provider file {part.file_id} belongs to {part.provider_id}/{part.endpoint_id}, "
      f"not {resolved.provider_id}/{resolved.endpoint_id}"
    )


def _reject_unsupported_capability(
  resolved: "ResolvedProviderRuntime | None",
  modality: str,
  source_kind: str | None = None,
) -> None:
  """Apply the runtime's explicit denials while preserving unknown fail-open semantics."""
  if resolved is None:
    return
  capabilities = resolved.effective_capabilities
  modality_cap = capabilities.input_modalities.get(modality)
  if modality_cap is not None and modality_cap.state == "unsupported":
    raise ContentValidationError(f"{modality} is explicitly unsupported by {resolved.provider_id}/{resolved.model_id}")
  if source_kind is None:
    return
  source_attr = {
    "url": f"{modality}_url",
    "base64": f"{modality}_base64",
    "fileId": "file_id",
    "object": "file_id",
  }.get(source_kind)
  source_cap = getattr(capabilities, source_attr, None) if source_attr else None
  if source_cap is not None and source_cap.state == "unsupported":
    raise ContentValidationError(
      f"{modality} {source_kind} source is explicitly unsupported by {resolved.provider_id}/{resolved.model_id}"
    )


def _preflight_tool_blocks(
  blocks: list[dict] | tuple[dict, ...],
  resolved: "ResolvedProviderRuntime | None",
) -> None:
  for block in blocks:
    if block.get("type") not in {"image", "audio", "video", "file"}:
      continue
    source = block.get("source") or {}
    _reject_unsupported_capability(resolved, block["type"], source.get("kind"))
    affinity = source.get("affinity") if source.get("kind") == "fileId" else None
    if affinity is not None:
      if not isinstance(affinity, dict):
        raise ContentValidationError("fileId affinity must be an object")
      provider_id = affinity.get("providerId")
      endpoint_id = affinity.get("endpointId")
      if not isinstance(provider_id, str) or not isinstance(endpoint_id, str):
        raise ContentValidationError("fileId affinity requires providerId and endpointId")
      if resolved is not None and (
        provider_id != resolved.provider_id or endpoint_id != resolved.endpoint_id
      ):
        raise ContentValidationError(
          f"Provider file {source.get('id')} belongs to {provider_id}/{endpoint_id}, "
          f"not {resolved.provider_id}/{resolved.endpoint_id}"
        )


def validate_rendered_message(
  message: Any,
  resolved: "ResolvedProviderRuntime | None" = None,
) -> None:
  """Validate one legacy message before its canonical provider projection."""
  parts = getattr(message, "content_parts", None) or []
  for part in parts:
    kind = getattr(part, "type", "unknown")
    if kind == "text":
      if not isinstance(getattr(part, "text", None), str):
        raise ContentValidationError("text content must be a string")
    elif kind in {"image", "audio"}:
      _validate_media_part(part)
      _reject_unsupported_capability(resolved, kind, "base64" if getattr(part, "data", None) else "url")
    elif kind == "file":
      _validate_file_part(part, resolved)
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
      if getattr(part, "content_parts", None) is not None:
        _preflight_tool_blocks(getattr(part, "content_parts"), resolved)
    else:
      raise ContentValidationError(f"unknown content part type: {kind}")


def validate_rendered_context(
  context: "RenderedContext",
  resolved: "ResolvedProviderRuntime | None" = None,
) -> None:
  for message in [*context.turns, *([context.state_turn] if context.state_turn is not None else [])]:
    validate_rendered_message(message, resolved)


def normalize_canonical_adapter_input(
  context: "RenderedContext",
  tools: list["ToolSchema"] | tuple["ToolSchema", ...],
  *,
  extensions: dict[str, Any] | None = None,
  resolved: "ResolvedProviderRuntime | None" = None,
) -> CanonicalAdapterInput:
  """Validate compatibility carriers once before protocol-specific projection."""
  validate_rendered_context(context, resolved)
  return CanonicalAdapterInput(
    context=context,
    tools=tuple(tools),
    extensions=dict(extensions or {}),
    resolved=resolved,
  )


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
