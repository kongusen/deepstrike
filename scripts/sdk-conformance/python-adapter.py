#!/usr/bin/env python3
"""SPC-017 Python SDK conformance adapter.

The adapter is intentionally a process boundary: one fixture path in, one JSON envelope out.
It exercises the public Python contracts without provider calls, network access, or persistence.
"""
from __future__ import annotations

import json
import sys
import asyncio
import tempfile
from pathlib import Path
import re
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
FIXTURES_ROOT = (ROOT / "tests" / "fixtures").resolve()
# The CI conformance job installs the wheel built from this checkout before
# launching the adapter.  Do not prepend ``python/`` here: doing so shadows the
# installed wheel with the pure-Python checkout and leaves its compiled
# ``deepstrike._kernel`` extension unavailable.  Local runs should likewise use
# an environment where the checkout is installed (``maturin develop`` or a
# freshly built wheel), keeping the process boundary identical to CI.

try:
  from deepstrike.advanced import (
    create_native_context_preparation_adapter,
    InMemorySessionLog,
    SESSION_EVENT_KINDS,
    decode_canonical_content_parts,
    decode_durable_content,
    decode_durable_tool_result,
    encode_canonical_content_parts,
    lower_agent,
    normalize_agent,
  )
  from deepstrike.providers import (
    ProviderRequestEndpoint,
    create_provider_request_plan,
    record_prompt_measurement,
  )
  from deepstrike.runtime.execution_evidence import provider_attempt_to_record
  from deepstrike.runtime.session_log import FileSessionLog
except ModuleNotFoundError as error:
  if error.name != "deepstrike":
    raise
  from deepstrike.advanced import (
    create_native_context_preparation_adapter,
    InMemorySessionLog,
    SESSION_EVENT_KINDS,
    decode_canonical_content_parts,
    decode_durable_content,
    decode_durable_tool_result,
    encode_canonical_content_parts,
    lower_agent,
    normalize_agent,
  )
  from deepstrike.providers import (
    ProviderRequestEndpoint,
    create_provider_request_plan,
    record_prompt_measurement,
  )
  from deepstrike.runtime.execution_evidence import provider_attempt_to_record
  from deepstrike.runtime.session_log import FileSessionLog


STOP_REASONS = {"end_turn", "tool_use", "max_tokens", "stop_sequence", "content_filter", "other"}


class ConformanceError(ValueError):
  def __init__(self, code: str, path: str, message: str):
    super().__init__(message)
    self.code = code
    self.path = path


def fixture_path_for(relative_path: Any) -> Path:
  if (
    not isinstance(relative_path, str)
    or not relative_path
    or Path(relative_path).is_absolute()
    or relative_path.startswith(("/", "\\"))
    or re.match(r"^[A-Za-z]:[\\/]", relative_path)
    or ".." in relative_path.replace("\\", "/").split("/")
  ):
    raise ConformanceError("invalid_fixture_reference", "/input/fixture", "fixture reference must be a relative path under tests/fixtures")
  try:
    candidate = (FIXTURES_ROOT / relative_path).resolve(strict=True)
    candidate.relative_to(FIXTURES_ROOT)
    if candidate == FIXTURES_ROOT:
      raise ValueError("fixture reference resolves to fixtures root")
  except (OSError, RuntimeError, ValueError) as error:
    raise ConformanceError("invalid_fixture_reference", "/input/fixture", "fixture reference must resolve under tests/fixtures") from error
  return candidate


async def replay_session_event(event: dict[str, Any], content: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
  session_log = InMemorySessionLog()
  await session_log.append("spc-017", {
    "kind": "tool_completed",
    "turn": 0,
    "results": [{
      "call_id": event.get("callId"),
      "output": "",
      "is_error": event.get("isError"),
      "content": content,
    }],
  })
  entries = await session_log.read("spc-017")
  if not entries or entries[0].event["kind"] != "tool_completed":
    raise ConformanceError("invalid_session_event", "/event", "session event did not replay as tool_completed")
  recorded = entries[0].event["results"][0]
  return entries[0].event, recorded


def snake_top_level_attempt(attempt: dict[str, Any]) -> dict[str, Any]:
  """Object-model (camelCase) attempt → the snake_case dict python's record builder takes.
  Nested route/usage pass through verbatim — builders never respell nested objects."""
  key_map = {
    "effectId": "effect_id",
    "attemptSeq": "attempt_seq",
    "requestFingerprint": "request_fingerprint",
    "transportRungs": "transport_rungs",
    "lastErrorClass": "last_error_class",
    "startedAtMs": "started_at_ms",
    "finishedAtMs": "finished_at_ms",
    "wireEvidence": "wire_evidence",
  }
  return {key_map.get(key, key): value for key, value in attempt.items()}


def project_attempt_record(record: dict[str, Any]) -> dict[str, Any]:
  """Canonical comparison form (0.2.64 S3): snake_case top level, nested objects verbatim
  (camelCase wire-family spelling). Only the pinned field set survives — `kind` is the
  SessionLog envelope, not part of the record."""
  projected = {
    "effect_id": record.get("effect_id"),
    "attempt_seq": record.get("attempt_seq"),
    "route": record.get("route"),
    "request_fingerprint": record.get("request_fingerprint"),
    "status": record.get("status"),
    "transport_rungs": record.get("transport_rungs"),
    "started_at_ms": record.get("started_at_ms"),
    "finished_at_ms": record.get("finished_at_ms"),
  }
  for optional in ("last_error_class", "usage", "wire_evidence", "accounting_policy_id"):
    if record.get(optional) is not None:
      projected[optional] = record[optional]
  return projected


def canonical_for(fixture: dict[str, Any]) -> dict[str, Any]:
  input_value = fixture.get("input", {})
  domain = fixture["domain"]
  if domain == "context_execution":
    adapter = create_native_context_preparation_adapter()
    prepared = adapter.prepare(input_value["request"])
    return {
      "input_digest": prepared["execution_input"]["input_digest"],
      "plan_digest": prepared["plan"]["plan_id"],
      "verified": adapter.verify(input_value["request"]["effect"], prepared),
    }
  if domain == "agent_ir":
    source = json.loads(fixture_path_for(input_value.get("fixture")).read_text(encoding="utf-8"))
    lowered = lower_agent(normalize_agent(source))
    return {
      "name": lowered["name"],
      **({"capabilityFilter": lowered["capabilityFilter"]} if "capabilityFilter" in lowered else {}),
      "effectiveCapabilities": lowered["effectiveCapabilities"],
    }

  if domain == "provider_request_plan":
    source = json.loads(fixture_path_for(input_value.get("fixture")).read_text(encoding="utf-8"))
    source = source["input"]
    endpoint = source["endpoint"]
    plan = create_provider_request_plan(
      provider_id=source["providerId"],
      model_id=source["modelId"],
      endpoint=ProviderRequestEndpoint(endpoint["id"], endpoint["protocol"], endpoint["baseURL"]),
      context=source["context"],
      tools=source["tools"],
      options=source.get("options"),
      execution=source.get("execution"),
    )
    return {"fingerprint": plan.fingerprint}

  if domain == "durable_tool_result":
    value = input_value.get("value")
    if "fixture" in input_value:
      value = json.loads(fixture_path_for(input_value.get("fixture")).read_text(encoding="utf-8"))
    result = decode_durable_tool_result(value)
    canonical = {
      "call_id": result["call_id"],
      "is_error": result["is_error"],
      "blockTypes": [block["type"] for block in result["blocks"]],
    }
    return canonical

  if domain == "prompt_measurement":
    value = input_value.get("value", {})
    # The public helper supplies the same validation path used by runtime-recorded facts.
    record = record_prompt_measurement(
      type("Plan", (), {"fingerprint": value.get("requestFingerprint")})(),
      input_tokens=value.get("inputTokens"),
      source=value.get("source"),
      confidence=value.get("confidence"),
    )
    canonical = {
      "requestFingerprint": record.request_fingerprint,
      "inputTokens": record.input_tokens,
      "source": record.source,
      "confidence": record.confidence,
    }
    return canonical

  if domain == "content_parts_v1":
    # F14/B5 byte contract: encode pins exact bytes; decode of an unknown prefix or an
    # undecodable payload must return None (the text stays literal — never guess).
    parts = input_value.get("parts")
    if isinstance(parts, list):
      encoded = encode_canonical_content_parts(parts)
      decoded = decode_canonical_content_parts(encoded)
      return {"encoded": encoded, "roundtrip": decoded == parts}
    literal = input_value.get("decode")
    if isinstance(literal, str):
      return {"decoded": decode_canonical_content_parts(literal)}
    raise ConformanceError("invalid_content_parts", "/input", "content_parts_v1 input must carry parts or decode")

  if domain == "provider_attempt_record":
    # P4 §3 (0.2.64 S3): the attempt record is pinned in its canonical JSON form — snake_case
    # top-level fields (the wire convention in every SDK), nested objects in the TS-native
    # camelCase spelling (the wire-family convention the provider_request_plan fingerprint
    # already pins). `attempt` exercises the SDK's record builder (top-level keys re-spelled to
    # python's snake_case builder input; nested route/usage pass through verbatim); `record`
    # roundtrips a wire record through the durable codec, whose read path carries the teeth (G2).
    attempt = input_value.get("attempt")
    if isinstance(attempt, dict):
      return project_attempt_record(provider_attempt_to_record(
        snake_top_level_attempt(attempt), input_value.get("policyId")))
    wire_record = input_value.get("record")
    if isinstance(wire_record, dict):
      log = FileSessionLog(Path(tempfile.mkdtemp(prefix="ds-conf-")))
      asyncio.run(log.append("spc-017", {"kind": "provider_attempt", **wire_record}))
      entries = asyncio.run(log.read("spc-017"))
      return project_attempt_record(entries[0].event)
    raise ConformanceError("invalid_provider_attempt", "/input", "provider_attempt_record input must carry attempt or record")

  if domain == "session_event_vocabulary":
    # F9/S3 (P7-S4): the local registered vocabulary, sorted for byte-stable comparison.
    # The manifest fixture pins this list across SDKs — extra or missing kinds both fail.
    kinds = sorted(SESSION_EVENT_KINDS)
    return {"kinds": kinds, "count": len(kinds)}

  if domain == "provider_error":
    stop_reason = input_value.get("stopReason")
    if not isinstance(stop_reason, str) or stop_reason not in STOP_REASONS:
      raise ConformanceError("unknown_stop_reason", "/stopReason", f"unknown stop reason: {stop_reason!r}")
    return {"stopReason": stop_reason}

  if domain == "session_event":
    event = input_value.get("event", {})
    content = decode_durable_content(event.get("content"))
    recorded_event, result = asyncio.run(replay_session_event(event, content))
    recorded_content = decode_durable_content(result.get("content"))
    canonical = {
      "kind": recorded_event["kind"],
      "callId": result.get("call_id"),
      "isError": result.get("is_error", False),
      "blockTypes": [block["type"] for block in recorded_content["blocks"]],
    }
    return canonical

  raise ConformanceError("unsupported_domain", "/domain", f"unsupported conformance domain: {domain!r}")


def main() -> None:
  if len(sys.argv) != 2:
    raise SystemExit("usage: python-adapter.py <fixture.json>")
  if not Path(sys.argv[1]).is_absolute():
    raise SystemExit("fixture path must be absolute")
  fixture_path = Path(sys.argv[1]).resolve()
  fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
  base = {
    "sdk": "python",
    "fixture": fixture.get("id"),
  }
  try:
    print(json.dumps({"ok": True, **base, "canonical": canonical_for(fixture)}, separators=(",", ":")))
  except ConformanceError as error:
    print(json.dumps({"ok": False, **base, "error": {"code": error.code, "path": error.path, "message": str(error)}}, separators=(",", ":")))
  except Exception as error:
    if fixture.get("domain") == "durable_tool_result":
      code = "invalid_durable_tool_result"
      path = "/is_error" if "is_error" in str(error) else ""
    elif fixture.get("domain") == "provider_attempt_record":
      code = "invalid_provider_attempt"
      # Native messages name the rejected field: "provider_attempt effect_id is required".
      match = re.search(r"provider_attempt (\w+)", str(error))
      path = f"/{match.group(1)}" if match else ""
    else:
      code = "conformance_error"
      path = ""
    print(json.dumps({"ok": False, **base, "error": {"code": code, "path": path, "message": str(error)}}, separators=(",", ":")))


if __name__ == "__main__":
  main()
