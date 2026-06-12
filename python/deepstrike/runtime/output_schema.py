"""G3 structured output: a small, dependency-free JSON-Schema subset validator + helpers used by the
workflow runner to enforce a node's ``output_schema``.

The kernel carries the schema verbatim (it is zero-I/O and never validates); enforcement lives here,
SDK-side, where the agent output exists. Supported keywords (the common structured-output subset):
``type`` (object | array | string | number | integer | boolean | null), ``required``, ``properties``
(recursive), ``items`` (recursive), ``enum``. Unknown keywords are ignored, not rejected.
"""

from __future__ import annotations

import json
from typing import Any


def _type_of(v: Any) -> str:
  if v is None:
    return "null"
  if isinstance(v, bool):  # bool is an int subclass — check first
    return "boolean"
  if isinstance(v, list):
    return "array"
  if isinstance(v, dict):
    return "object"
  if isinstance(v, (int, float)):
    return "number"
  if isinstance(v, str):
    return "string"
  return "unknown"


def _matches_type(v: Any, t: str) -> bool:
  if t == "integer":
    return isinstance(v, int) and not isinstance(v, bool)
  if t == "number":
    return isinstance(v, (int, float)) and not isinstance(v, bool)
  return _type_of(v) == t


def validate_against_schema(value: Any, schema: dict[str, Any], path: str = "$") -> list[str]:
  """Return a list of validation errors (empty ⇒ valid)."""
  errors: list[str] = []

  t = schema.get("type")
  if isinstance(t, str) and not _matches_type(value, t):
    return [f"{path}: expected {t}, got {_type_of(value)}"]
  if isinstance(t, list) and not any(isinstance(x, str) and _matches_type(value, x) for x in t):
    return [f"{path}: expected one of {t}, got {_type_of(value)}"]

  enum = schema.get("enum")
  if isinstance(enum, list) and value not in enum:
    errors.append(f"{path}: value not in enum")

  if isinstance(value, dict):
    for key in schema.get("required", []) or []:
      if key not in value:
        errors.append(f"{path}.{key}: required property missing")
    for key, sub in (schema.get("properties") or {}).items():
      if key in value and isinstance(sub, dict):
        errors.extend(validate_against_schema(value[key], sub, f"{path}.{key}"))

  if isinstance(value, list):
    items = schema.get("items")
    if isinstance(items, dict):
      for i, el in enumerate(value):
        errors.extend(validate_against_schema(el, items, f"{path}[{i}]"))

  return errors


def schema_instruction(schema: dict[str, Any]) -> str:
  """The instruction appended to a node's goal so its agent produces schema-conforming JSON."""
  return (
    "You MUST return ONLY a single JSON value that conforms to this JSON Schema, with no prose, "
    "no markdown, and no code fences:\n" + json.dumps(schema)
  )


def schema_retry_instruction(schema: dict[str, Any], errors: list[str]) -> str:
  """A stronger re-prompt for a retry after a validation failure."""
  return (
    schema_instruction(schema)
    + "\n\nYour previous output did NOT conform: "
    + "; ".join(errors)
    + ". Return ONLY the corrected JSON value."
  )


def extract_json_value(text: str) -> Any:
  """Best-effort extraction of a JSON value from agent output (raw, fenced, or embedded)."""
  trimmed = (text or "").strip()
  if not trimmed:
    return None

  def _try(s: str) -> Any:
    try:
      return json.loads(s)
    except Exception:
      return _SENTINEL

  whole = _try(trimmed)
  if whole is not _SENTINEL:
    return whole

  import re

  fence = re.search(r"```(?:json)?\s*([\s\S]*?)```", trimmed, re.IGNORECASE)
  if fence:
    fenced = _try(fence.group(1).strip())
    if fenced is not _SENTINEL:
      return fenced

  for open_c, close_c in (("{", "}"), ("[", "]")):
    start = trimmed.find(open_c)
    end = trimmed.rfind(close_c)
    if start != -1 and end > start:
      sliced = _try(trimmed[start : end + 1])
      if sliced is not _SENTINEL:
        return sliced
  return None


_SENTINEL = object()
