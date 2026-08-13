"""Strict versioned durable content ABI (016-02).

This module is provider-neutral. It validates durable records before provider-specific projection;
legacy text and ``output``-only tool results are migrated to one text block.
"""
from __future__ import annotations

import base64
import binascii
from typing import Any


class DurableContentError(ValueError):
  pass


def _obj(value: Any, label: str) -> dict[str, Any]:
  if not isinstance(value, dict):
    raise DurableContentError(f"{label} must be an object")
  return value


def _keys(value: dict[str, Any], allowed: set[str], label: str) -> None:
  unknown = set(value) - allowed
  if unknown:
    raise DurableContentError(f"{label} has unknown field {sorted(unknown)[0]}")


def _string(value: Any, label: str) -> str:
  if not isinstance(value, str) or not value:
    raise DurableContentError(f"{label} must be a non-empty string")
  return value


def _boolean(value: Any, label: str) -> bool:
  if not isinstance(value, bool):
    raise DurableContentError(f"{label} must be a boolean")
  return value


def _source(value: Any, label: str) -> dict[str, Any]:
  raw = _obj(value, f"{label} source")
  kind = raw.get("kind")
  allowed = {
    "url": {"kind", "url"},
    "base64": {"kind", "data"},
    "file_id": {"kind", "id", "affinity"},
    "object": {"kind", "handle", "owner", "payload_ref"},
  }.get(kind)
  if allowed is None:
    raise DurableContentError(f"{label} source kind is invalid")
  _keys(raw, allowed, f"{label} source")
  if kind == "url":
    return {"kind": kind, "url": _string(raw.get("url"), f"{label} URL")}
  if kind == "base64":
    data = _string(raw.get("data"), f"{label} base64 data")
    try:
      base64.b64decode(data, validate=True)
    except (binascii.Error, ValueError) as exc:
      raise DurableContentError(f"{label} base64 data is not valid base64") from exc
    return {"kind": kind, "data": data}
  if kind == "file_id":
    out = {"kind": kind, "id": _string(raw.get("id"), f"{label} file id")}
    affinity = _obj(raw.get("affinity"), f"{label} affinity")
    _keys(affinity, {"provider_id", "endpoint_id"}, f"{label} affinity")
    out["affinity"] = {
      "provider_id": _string(affinity.get("provider_id"), f"{label} affinity provider_id"),
      "endpoint_id": _string(affinity.get("endpoint_id"), f"{label} affinity endpoint_id"),
    }
    return out
  return {
    "kind": kind,
    "handle": _string(raw.get("handle"), f"{label} object handle"),
    "owner": _string(raw.get("owner"), f"{label} object owner"),
    "payload_ref": _string(raw.get("payload_ref"), f"{label} payload_ref"),
  }


def _block(value: Any, index: int) -> dict[str, Any]:
  raw = _obj(value, f"content block {index}")
  kind = raw.get("type")
  if kind == "text":
    _keys(raw, {"type", "text"}, f"content block {index}")
    if not isinstance(raw.get("text"), str):
      raise DurableContentError(f"content block {index} text must be a string")
    return {"type": kind, "text": raw["text"]}
  if kind == "tool_result":
    raise DurableContentError("nested tool_result blocks are forbidden")
  if kind not in {"image", "audio", "video", "file"}:
    raise DurableContentError(f"unknown content block type: {kind}")
  _keys(raw, {"type", "source", "media_type", "provider_options"}, f"content block {index}")
  out = {"type": kind, "source": _source(raw.get("source"), f"content block {index}")}
  if "media_type" in raw:
    out["media_type"] = _string(raw["media_type"], f"content block {index} media_type")
  if "provider_options" in raw:
    out["provider_options"] = _obj(raw["provider_options"], f"content block {index} provider_options")
  return out


def _blocks(value: Any) -> list[dict[str, Any]]:
  if not isinstance(value, list):
    raise DurableContentError("content blocks must be an array")
  return [_block(item, index) for index, item in enumerate(value)]


def decode_durable_content(value: Any) -> dict[str, Any]:
  raw = _obj(value, "durable content")
  _keys(raw, {"schema_version", "blocks"}, "durable content")
  if raw.get("schema_version") != 1:
    raise DurableContentError(f"unsupported durable content schema_version {raw.get('schema_version')}")
  return {"schema_version": 1, "blocks": _blocks(raw.get("blocks"))}


def decode_durable_tool_result(value: Any) -> dict[str, Any]:
  raw = _obj(value, "durable tool result")
  if "blocks" not in raw and "output" in raw:
    _keys(raw, {"call_id", "output", "is_error"}, "legacy tool result")
    return {
      "schema_version": 1,
      "call_id": _string(raw.get("call_id"), "tool result call_id"),
      "is_error": _boolean(raw.get("is_error", False), "legacy tool result is_error"),
      "blocks": [{"type": "text", "text": _string(raw.get("output"), "legacy tool result output") if raw.get("output") != "" else ""}],
    }
  _keys(raw, {"schema_version", "call_id", "is_error", "blocks"}, "durable tool result")
  if raw.get("schema_version") != 1:
    raise DurableContentError(f"unsupported durable tool result schema_version {raw.get('schema_version')}")
  return {
    "schema_version": 1,
    "call_id": _string(raw.get("call_id"), "tool result call_id"),
    "is_error": _boolean(raw.get("is_error"), "tool result is_error"),
    "blocks": _blocks(raw.get("blocks")),
  }


def migrate_legacy_content(text: str) -> dict[str, Any]:
  if not isinstance(text, str):
    raise DurableContentError("legacy content must be a string")
  return {"schema_version": 1, "blocks": [{"type": "text", "text": text}]}


def encode_durable_content(content: dict[str, Any]) -> dict[str, Any]:
  return decode_durable_content(content)


def encode_durable_tool_result(result: dict[str, Any]) -> dict[str, Any]:
  return decode_durable_tool_result(result)


def durable_blocks_to_runtime(blocks: list[dict[str, Any]]) -> list[dict[str, Any]]:
  out: list[dict[str, Any]] = []
  for block in blocks:
    if block["type"] == "text":
      out.append(block)
      continue
    source = dict(block["source"])
    if source["kind"] == "file_id":
      source["kind"] = "fileId"
      if source.get("affinity"):
        source["affinity"] = {
          "providerId": source["affinity"]["provider_id"],
          "endpointId": source["affinity"]["endpoint_id"],
        }
    elif source["kind"] == "object":
      source["payloadRef"] = source.pop("payload_ref")
    out.append({"type": block["type"], "source": source,
      **({"media_type": block["media_type"]} if "media_type" in block else {}),
      **({"provider_options": block["provider_options"]} if "provider_options" in block else {})})
  return out


def runtime_blocks_to_durable(blocks: list[dict[str, Any]] | tuple[dict, ...]) -> list[dict[str, Any]]:
  """Freeze provider-carrier blocks with the durable ABI's snake_case source contract."""
  durable: list[dict[str, Any]] = []
  for block in blocks:
    kind = block.get("type")
    if kind == "text":
      durable.append({"type": "text", "text": block.get("text")})
      continue
    source = dict(block.get("source") or {})
    if source.get("kind") == "fileId":
      source["kind"] = "file_id"
      if source.get("affinity"):
        source["affinity"] = {
          "provider_id": source["affinity"].get("providerId"),
          "endpoint_id": source["affinity"].get("endpointId"),
        }
      else:
        raise DurableContentError("runtime fileId source requires endpoint affinity")
    elif source.get("kind") == "object":
      source["payload_ref"] = source.pop("payloadRef", source.get("payload_ref"))
    if source.get("kind") == "object" and (not source.get("owner") or not source.get("payload_ref")):
      raise DurableContentError("runtime object source requires owner and payloadRef")
    durable.append({
      "type": kind, "source": source,
      **({"media_type": block.get("media_type", block.get("mediaType"))} if block.get("media_type", block.get("mediaType")) else {}),
      **({"provider_options": block.get("provider_options", block.get("providerOptions"))} if block.get("provider_options", block.get("providerOptions")) else {}),
    })
  return decode_durable_content({"schema_version": 1, "blocks": durable})["blocks"]
