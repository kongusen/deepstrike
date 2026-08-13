#!/usr/bin/env python3
"""SPC-017 Python SDK conformance adapter.

The adapter is intentionally a process boundary: one fixture path in, one JSON envelope out.
It exercises the public Python contracts without provider calls, network access, or persistence.
"""
from __future__ import annotations

import json
import sys
import asyncio
from pathlib import Path
import re
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
FIXTURES_ROOT = (ROOT / "tests" / "fixtures").resolve()
if str(ROOT / "python") not in sys.path:
  sys.path.insert(0, str(ROOT / "python"))

from deepstrike import (
  InMemorySessionLog,
  decode_durable_content,
  decode_durable_tool_result,
  lower_agent,
  normalize_agent,
)
from deepstrike.providers import (
  ProviderRequestEndpoint,
  create_provider_request_plan,
  record_prompt_measurement,
)


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


def canonical_for(fixture: dict[str, Any]) -> dict[str, Any]:
  input_value = fixture.get("input", {})
  domain = fixture["domain"]
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
    else:
      code = "conformance_error"
      path = ""
    print(json.dumps({"ok": False, **base, "error": {"code": code, "path": path, "message": str(error)}}, separators=(",", ":")))


if __name__ == "__main__":
  main()
