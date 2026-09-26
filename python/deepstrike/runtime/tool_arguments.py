"""Tool-call argument text is model-authored.

A call whose arguments are not a JSON object (a truncated stream, prose, an array) must reach the
executor as-is so it fails as an invalid call — rewriting it to ``{}`` would run the tool with
empty arguments the model never wrote. Mirrors Node ``runtime/tool-arguments.ts``.
"""
from __future__ import annotations

import json
from typing import Any

_UNPARSED = object()


def _try_parse(text: str) -> Any:
  try:
    return json.loads(text)
  except (TypeError, ValueError):
    return _UNPARSED


def tool_arguments_text(raw: str | None) -> str:
  """Canonical text: re-serialized when it is a JSON object, ``{}`` when empty, raw otherwise."""
  text = raw or ""
  if not text.strip():
    return "{}"
  parsed = _try_parse(text)
  return json.dumps(parsed) if isinstance(parsed, dict) else text


def tool_arguments_to_wire(raw: str | None) -> dict[str, Any] | str:
  """The object when the text is one, the raw string otherwise (the kernel carries it opaquely)."""
  text = raw or ""
  if not text.strip():
    return {}
  parsed = _try_parse(text)
  return parsed if isinstance(parsed, dict) else text


def tool_arguments_from_wire(value: Any) -> str:
  """Inverse of :func:`tool_arguments_to_wire`: kernel-carried arguments back to SDK text."""
  if isinstance(value, str):
    return value
  return json.dumps(value if value is not None else {})


def malformed_tool_arguments(raw: str | None) -> str | None:
  """The raw text when it is non-empty and not a JSON object, else ``None``."""
  text = raw or ""
  if not text.strip():
    return None
  return None if isinstance(_try_parse(text), dict) else text


def parse_tool_call_arguments(raw: str | None) -> tuple[dict[str, Any] | None, str | None]:
  """``(args, None)`` for a JSON object (or empty text), ``(None, error)`` for anything else."""
  text = raw or ""
  if not text.strip():
    return {}, None
  try:
    parsed = json.loads(text)
  except (TypeError, ValueError) as err:
    return None, f"arguments are not valid JSON ({err})"
  if not isinstance(parsed, dict):
    return None, "arguments must be a JSON object"
  return parsed, None
