"""Provider-visible request and accounting contracts (spc_016-01).

This module is Host-side only. It deliberately has no provider HTTP, credentials, retry state,
or Kernel imports, so a durable measurement can be replayed without another token-count request.
"""
from __future__ import annotations

from dataclasses import asdict, dataclass
from datetime import datetime, timezone
import math
from hashlib import sha256
import json
from typing import Any, Literal, Mapping
from urllib.parse import urlsplit, urlunsplit

from .usage import ProviderUsage


@dataclass(frozen=True)
class ProviderRequestEndpoint:
  id: str
  protocol: str
  base_url: str


@dataclass(frozen=True)
class ProviderRequestPlan:
  provider_id: str
  model_id: str
  endpoint: ProviderRequestEndpoint
  context: Any
  tools: tuple[Any, ...]
  options: dict[str, Any]
  fingerprint: str
  stable_prefix_fingerprint: str
  execution: Any | None = None


@dataclass(frozen=True)
class NormalizedProviderUsage:
  input_tokens: int
  uncached_input_tokens: int
  output_tokens: int
  cache_read_input_tokens: int = 0
  cache_creation_input_tokens: int = 0
  reasoning_tokens: int | None = None


@dataclass(frozen=True)
class ResolvedProviderRoute:
  """P4 §1.3: the resolved route one provider execution is pinned to.

  Content-addressed (route_id = stable hash of every field except capabilities_ref);
  the capability table's authority stays the static vocabulary — this holds a reference,
  never a copy.
  """
  route_id: str
  provider: str
  protocol: str
  model: str
  endpoint: ProviderRequestEndpoint
  adapter_version: str
  capabilities_ref: str


@dataclass(frozen=True)
class PromptMeasurementRecord:
  request_fingerprint: str
  input_tokens: int
  source: dict[str, str]
  confidence: Literal["exact", "high_confidence", "low_confidence"]


@dataclass(frozen=True)
class PricingSnapshot:
  version: str
  currency: str
  region: str
  effective_from: str
  rates_per_million: dict[str, float]
  expires_at: str | None = None


def estimate_provider_prompt_tokens(context: Any, tools: list[Any] | tuple[Any, ...]) -> int:
  """Conservative host-only fallback for providers without a native meter."""
  payload = {"context": _json_value(context), "tools": [_json_value(tool) for tool in tools]}
  size = len(_canonical_json(payload).encode("utf-8"))
  return max(1, (size + 3) // 4)


def create_provider_request_plan(
  *, provider_id: str, model_id: str, endpoint: ProviderRequestEndpoint,
  context: Any, tools: list[Any] | tuple[Any, ...], options: dict[str, Any] | None = None,
  execution: Any | None = None,
) -> ProviderRequestPlan:
  material = _material_options(options or {})
  context_value = _json_value(context)
  tools_value = [_json_value(tool) for tool in tools]
  endpoint_value = {"id": endpoint.id, "protocol": endpoint.protocol, "baseURL": _safe_endpoint(endpoint.base_url)}
  value = {
    "providerId": provider_id, "modelId": model_id,
    "endpoint": endpoint_value, "context": context_value,
    "tools": tools_value, "options": material,
  }
  if execution is not None:
    value["execution"] = _json_value(execution)
  fingerprint = "sha256:" + sha256(_canonical_json(value).encode()).hexdigest()
  stable_prefix = {
    "providerId": provider_id,
    "modelId": model_id,
    "endpoint": endpoint_value,
    "context": _stable_prefix_context(context_value),
    "tools": tools_value,
    "options": material,
  }
  stable_prefix_fingerprint = "sha256:" + sha256(_canonical_json(stable_prefix).encode()).hexdigest()
  return ProviderRequestPlan(
    provider_id=provider_id, model_id=model_id,
    endpoint=ProviderRequestEndpoint(endpoint.id, endpoint.protocol, _safe_endpoint(endpoint.base_url)),
    context=_json_value(context), tools=tuple(_json_value(tool) for tool in tools),
    options=material, fingerprint=fingerprint, stable_prefix_fingerprint=stable_prefix_fingerprint,
    execution=_json_value(execution) if execution is not None else None,
  )


def record_prompt_measurement(
  plan: ProviderRequestPlan, *, input_tokens: int, source: dict[str, str],
  confidence: Literal["exact", "high_confidence", "low_confidence"],
) -> PromptMeasurementRecord:
  return PromptMeasurementRecord(plan.fingerprint, _non_negative(input_tokens, "input_tokens"), dict(source), confidence)


def measurement_for_plan(
  plan: ProviderRequestPlan,
  record: PromptMeasurementRecord | Mapping[str, Any] | None,
) -> PromptMeasurementRecord | None:
  """Return a validated durable fact only when it belongs to this exact request plan."""
  if record is None:
    return None
  if isinstance(record, Mapping):
    try:
      record = PromptMeasurementRecord(
        request_fingerprint=record["request_fingerprint"],
        input_tokens=record["input_tokens"],
        source=dict(record["source"]),
        confidence=record["confidence"],
      )
    except (KeyError, TypeError, ValueError):
      return None
  if (
    not isinstance(record.request_fingerprint, str)
    or record.request_fingerprint != plan.fingerprint
    or record.confidence not in {"exact", "high_confidence", "low_confidence"}
  ):
    return None
  try:
    _non_negative(record.input_tokens, "input_tokens")
  except ValueError:
    return None
  source = record.source
  if not isinstance(source, dict) or source.get("kind") not in {"native", "local_exact", "postflight", "heuristic"}:
    return None
  if source["kind"] == "native" and not isinstance(source.get("provider"), str):
    return None
  if source["kind"] == "local_exact" and not isinstance(source.get("tokenizer"), str):
    return None
  return record


def normalize_provider_usage(usage: ProviderUsage) -> NormalizedProviderUsage:
  input_tokens = _non_negative(usage.input_tokens, "input_tokens")
  output_tokens = _non_negative(usage.output_tokens, "output_tokens")
  cache_read = _non_negative(usage.cache_read_input_tokens, "cache_read_input_tokens")
  cache_creation = _non_negative(usage.cache_creation_input_tokens, "cache_creation_input_tokens")
  if cache_read + cache_creation > input_tokens:
    raise ValueError("cache token subsets cannot exceed input_tokens")
  reasoning = usage.reasoning_tokens
  if reasoning is not None and _non_negative(reasoning, "reasoning_tokens") > output_tokens:
    raise ValueError("reasoning_tokens must be a subset of output_tokens")
  return NormalizedProviderUsage(input_tokens, input_tokens - cache_read - cache_creation, output_tokens, cache_read, cache_creation, reasoning)


def price_provider_usage(usage: NormalizedProviderUsage, snapshot: PricingSnapshot, observed_at: str) -> dict[str, Any]:
  try:
    at = _parse_time(observed_at)
    start = _parse_time(snapshot.effective_from)
    end = _parse_time(snapshot.expires_at) if snapshot.expires_at else None
    rates = snapshot.rates_per_million
    required = ("input", "output")
    if (not snapshot.version or not snapshot.currency or any(name not in rates for name in required)
        or any(isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0 for value in rates.values())):
      raise ValueError
  except (TypeError, ValueError):
    return {"source": "unpriced", "reason": "invalid_pricing_snapshot"}
  if at < start:
    return {"source": "unpriced", "reason": "pricing_snapshot_not_effective"}
  if end is not None and at >= end:
    return {"source": "unpriced", "reason": "pricing_snapshot_expired"}
  amount = (
    usage.uncached_input_tokens * rates["input"]
    + usage.output_tokens * rates["output"]
    + usage.cache_read_input_tokens * rates.get("cache_read", rates["input"])
    + usage.cache_creation_input_tokens * rates.get("cache_creation", rates["input"])
  ) / 1_000_000
  return {"source": "snapshot", "currency": snapshot.currency, "amount": amount, "pricing_version": snapshot.version}


_TRANSPORT_ONLY = frozenset({"apikey", "api_key", "bearertoken", "bearer_token", "authorization", "credential", "credentials", "retry", "maxretries", "basedelay", "timeout", "signal", "access_token", "refresh_token", "token", "secret", "x-api-key"})


def _material_options(options: dict[str, Any]) -> dict[str, Any]:
  sanitized = _sanitize_material_value(options)
  assert isinstance(sanitized, dict)
  return sanitized


def _sanitize_material_value(value: Any) -> Any:
  if callable(value):
    return _OMIT
  if value is None:
    return None
  if isinstance(value, dict):
    return {
      str(key): sanitized for key, item in sorted(value.items())
      if not _transport_only_key(str(key))
      for sanitized in (_sanitize_material_value(item),)
      if sanitized is not _OMIT
    }
  if isinstance(value, (list, tuple)):
    return [sanitized for item in value for sanitized in (_sanitize_material_value(item),) if sanitized is not _OMIT]
  return _json_value(value)


_OMIT = object()


def _stable_prefix_context(context: Any) -> dict[str, Any]:
  value = context if isinstance(context, dict) else {}
  frozen_prefix_len = value.get("frozen_prefix_len", value.get("frozenPrefixLen", 0)) or 0
  turns = value.get("turns") if isinstance(value.get("turns"), list) else []
  return {
    "systemText": value.get("system_text", value.get("systemText", "")),
    "systemStable": value.get("system_stable", value.get("systemStable", "")),
    "systemKnowledge": value.get("system_knowledge", value.get("systemKnowledge", "")),
    "frozenPrefixLen": frozen_prefix_len,
    "turns": turns[:frozen_prefix_len],
  }


def _safe_endpoint(value: str) -> str:
  try:
    parsed = urlsplit(value)
  except ValueError:
    return ""
  if not parsed.scheme or not parsed.hostname:
    return ""
  return urlunsplit((parsed.scheme, parsed.hostname, parsed.path.rstrip("/"), "", ""))


def _transport_only_key(key: str) -> bool:
  normalized = "".join(character for character in key.lower() if character.isalnum())
  return (key.lower() in _TRANSPORT_ONLY or "authorization" in normalized or "credential" in normalized
          or "accesstoken" in normalized or "refreshtoken" in normalized or "apikey" in normalized
          or normalized in {"bearer", "token", "secret", "xapikey"})


def _json_value(value: Any) -> Any:
  # pyo3 Kernel DTOs deliberately expose attributes without a Python ``__dict__``.
  # Read their public wire fields explicitly instead of asking dataclasses.asdict to deepcopy them.
  if type(value).__module__ == "builtins" and type(value).__name__ == "ModelMessage":
    return {
      "role": value.role,
      "content": _json_value(value.content),
      "tool_calls": _json_value(value.tool_calls),
      "content_parts": _json_value(value.content_parts),
    }
  if type(value).__module__ == "builtins" and type(value).__name__ == "ToolSchema":
    return {"name": value.name, "description": value.description, "parameters": _json_value(value.parameters)}
  if type(value).__module__ == "builtins" and type(value).__name__ == "ToolCall":
    return {"id": value.id, "name": value.name, "arguments": _json_value(value.arguments)}
  if type(value).__module__ == "builtins" and type(value).__name__ == "ContentPartObj":
    result = {"type": value.type}
    for key in (
      "text", "url", "source_kind", "source_data", "media_type", "detail", "call_id", "output", "is_error",
      "file_id", "provider_id", "endpoint_id",
    ):
      item = getattr(value, key)
      if item is not None:
        result[key] = _json_value(item)
    return result
  if hasattr(value, "__dataclass_fields__"):
    return {key: _json_value(item) for key, item in vars(value).items()}
  if isinstance(value, dict):
    return {str(key): _json_value(item) for key, item in value.items()}
  if isinstance(value, (list, tuple)):
    return [_json_value(item) for item in value]
  if isinstance(value, (str, int, float, bool)) or value is None:
    return value
  if hasattr(value, "__dict__"):
    return {key: _json_value(item) for key, item in vars(value).items()}
  raise TypeError(f"request plan cannot serialize {type(value).__name__}")


def _canonical_json(value: Any) -> str:
  return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _non_negative(value: int, name: str) -> int:
  if isinstance(value, bool) or not isinstance(value, int) or value < 0:
    raise ValueError(f"{name} must be a non-negative integer")
  return value


def _parse_time(value: str | None) -> datetime:
  if not value:
    raise ValueError("missing time")
  parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
  return parsed if parsed.tzinfo is not None else parsed.replace(tzinfo=timezone.utc)


KNOWN_GENERATION_PROTOCOLS = frozenset({"anthropic-messages", "openai-chat", "openai-responses", "gemini", "ollama-chat"})


def resolve_provider_route(provider: Any) -> ResolvedProviderRoute:
  """P4-S1: assemble the run's route once at runner construction.

  The provider is fixed for the run today (P4 §0.2); a future failover resolver
  re-evaluates per attempt with the same shape. Never throws: evidence assembly
  degrades to "unknown" fields rather than breaking a run.
  """
  try:
    descriptor = getattr(provider, "descriptor", lambda: {"provider": "unknown", "protocol": "unknown", "model": "unknown"})()
    identity = getattr(provider, "request_plan_identity", lambda: {})()
    protocol_raw = identity.get("endpoint", {}).get("protocol") or descriptor.get("protocol", "unknown")
    protocol = protocol_raw
    endpoint = _sanitize_endpoint({
      "id": identity.get("endpoint", {}).get("id") or f"{descriptor.get('provider', 'unknown')}.{descriptor.get('protocol', 'unknown')}",
      "protocol": protocol_raw,
      "base_url": identity.get("endpoint", {}).get("base_url", ""),
    })
    route = {
      "provider": identity.get("provider_id") or descriptor.get("provider", "unknown"),
      "protocol": protocol,
      "model": identity.get("model_id") or descriptor.get("model", "unknown"),
      # Content addressing hashes the JSON shape, not the dataclass.
      "endpoint": {"id": endpoint.id, "protocol": endpoint.protocol, "base_url": endpoint.base_url},
      "adapter_version": _adapter_package_version(),
      "capabilities_ref": protocol if protocol in KNOWN_GENERATION_PROTOCOLS else "unknown",
    }
    # §1.3: content addressing covers every field EXCEPT capabilities_ref
    addressed = {k: v for k, v in route.items() if k != "capabilities_ref"}
    route_id = "sha256:" + sha256(_canonical_json(addressed).encode()).hexdigest()
    return ResolvedProviderRoute(
      route_id=route_id,
      provider=route["provider"],
      protocol=route["protocol"],
      model=route["model"],
      endpoint=endpoint,
      adapter_version=route["adapter_version"],
      capabilities_ref=route["capabilities_ref"],
    )
  except Exception:
    fallback = {
      "provider": "unknown",
      "protocol": "unknown",
      "model": "unknown",
      "endpoint": {"id": "unknown", "protocol": "unknown", "base_url": ""},
      "adapter_version": "unknown",
    }
    route_id = "sha256:" + sha256(_canonical_json(fallback).encode()).hexdigest()
    return ResolvedProviderRoute(
      route_id=route_id,
      provider="unknown",
      protocol="unknown",
      model="unknown",
      endpoint=ProviderRequestEndpoint("unknown", "unknown", ""),
      adapter_version="unknown",
      capabilities_ref="unknown",
    )


def _adapter_package_version() -> str:
  """The adapter's package version from pyproject.toml. 'unknown' when unreadable."""
  try:
    from importlib.metadata import version
    return version("deepstrike")
  except Exception:
    return "unknown"


def _sanitize_endpoint(endpoint: dict[str, Any]) -> ProviderRequestEndpoint:
  """Strip credentials from endpoint baseURL."""
  try:
    base_url = endpoint.get("base_url", "")
    if not base_url:
      return ProviderRequestEndpoint(
        id=str(endpoint.get("id", "unknown")),
        protocol=str(endpoint.get("protocol", "unknown")),
        base_url="",
      )
    parsed = urlsplit(base_url)
    clean = urlunsplit((parsed.scheme, parsed.netloc.split("@")[-1], parsed.path, parsed.query, ""))
    return ProviderRequestEndpoint(
      id=str(endpoint.get("id", "unknown")),
      protocol=str(endpoint.get("protocol", "unknown")),
      base_url=clean,
    )
  except Exception:
    return ProviderRequestEndpoint(
      id=str(endpoint.get("id", "unknown")),
      protocol=str(endpoint.get("protocol", "unknown")),
      base_url="",
    )
