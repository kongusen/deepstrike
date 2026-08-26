from deepstrike.providers.request_plan import (
  PricingSnapshot, ProviderRequestEndpoint, create_provider_request_plan,
  measurement_for_plan, normalize_provider_usage, price_provider_usage, record_prompt_measurement,
)
from deepstrike.providers.usage import ProviderUsage
import json
from pathlib import Path


def _plan(**patch):
  base = {
    "provider_id": "openai", "model_id": "gpt-4o",
    "endpoint": ProviderRequestEndpoint("openai.chat", "openai-chat", "https://api.openai.com/v1"),
    "context": {"system": "Be precise", "turns": [{"role": "user", "content": "你好"}]},
    "tools": [{"name": "lookup", "parameters": {"type": "object"}}],
    "options": {"temperature": 0.2, "api_key": "secret", "retry": {"max": 3}},
  }
  base.update(patch)
  return create_provider_request_plan(**base)


def test_request_fingerprint_covers_material_input_but_never_secret_or_retry_state():
  first = _plan()
  retry = _plan(options={"temperature": 0.2, "api_key": "other", "retry": {"max": 99}})
  changed = _plan(tools=[{"name": "other", "parameters": {"type": "object"}}])
  assert first.fingerprint == retry.fingerprint
  assert first.fingerprint != changed.fingerprint
  assert "secret" not in str(first)
  assert first.options == {"temperature": 0.2}


def test_request_fingerprint_invalidates_on_model_endpoint_option_and_state_turn_drift():
  """spc_024-06 INV-024-09: only a byte-identical wire plan may reuse a measurement."""
  base = _plan()
  same = _plan()
  assert base.fingerprint == same.fingerprint
  assert base.fingerprint != _plan(model_id="gpt-4o-mini").fingerprint
  assert base.fingerprint != _plan(
    endpoint=ProviderRequestEndpoint("openai.responses", "openai-responses", "https://api.openai.com/v1"),
  ).fingerprint
  assert base.fingerprint != _plan(options={"temperature": 0.9, "api_key": "secret"}).fingerprint

  state_context = {
    "system_text": "Be precise.",
    "turns": [{"role": "user", "content": "你好"}],
    "state_turn": {"role": "user", "content": "signals v1"},
  }
  with_state = _plan(context=state_context)
  assert with_state.fingerprint != _plan(context={
    **state_context, "state_turn": {"role": "user", "content": "signals v2"},
  }).fingerprint


def test_stable_prefix_fingerprint_ignores_append_only_tail_but_detects_drift():
  context = {
    "system_text": "Be precise.",
    "system_stable": "stable",
    "system_knowledge": "knowledge",
    "frozen_prefix_len": 1,
    "turns": [
      {"role": "user", "content": "frozen"},
      {"role": "assistant", "content": "volatile tail"},
    ],
  }
  base = _plan(context=context)
  appended = _plan(context={**context, "turns": [*context["turns"], {"role": "user", "content": "append"}]})
  changed = _plan(context={**context, "system_knowledge": "changed"})
  rewritten = _plan(context={**context, "turns": [{"role": "user", "content": "rewritten"}, context["turns"][1]]})

  assert appended.fingerprint != base.fingerprint
  assert appended.stable_prefix_fingerprint == base.stable_prefix_fingerprint
  assert changed.stable_prefix_fingerprint != base.stable_prefix_fingerprint
  assert rewritten.stable_prefix_fingerprint != base.stable_prefix_fingerprint


def test_request_plan_uses_shared_cross_sdk_sha256_fixture():
  fixture = json.loads((Path(__file__).parents[2] / "tests/fixtures/provider-request-plan/canonical.json").read_text(encoding="utf-8"))
  source = fixture["input"]
  plan = create_provider_request_plan(
    provider_id=source["providerId"], model_id=source["modelId"],
    endpoint=ProviderRequestEndpoint(source["endpoint"]["id"], source["endpoint"]["protocol"], source["endpoint"]["baseURL"]),
    context=source["context"], tools=source["tools"], options=source["options"],
  )
  assert plan.fingerprint == fixture["fingerprint"]
  assert plan.options == {"temperature": 0.2, "auth": {"mode": "request"}, "transport": {}}


def test_measurement_replay_usage_and_pricing_contracts_are_explicit():
  plan = _plan()
  record = record_prompt_measurement(plan, input_tokens=42, source={"kind": "native", "provider": "openai"}, confidence="exact")
  assert measurement_for_plan(plan, record) == record
  assert measurement_for_plan(_plan(model_id="gpt-5"), record) is None
  durable = {
    "request_fingerprint": record.request_fingerprint,
    "input_tokens": record.input_tokens,
    "source": record.source,
    "confidence": record.confidence,
  }
  assert measurement_for_plan(plan, durable) == record
  usage = normalize_provider_usage(ProviderUsage(120, 30, 20, 10, 6))
  assert usage.uncached_input_tokens == 90
  snapshot = PricingSnapshot("v1", "USD", "global", "2026-08-01T00:00:00Z", {"input": 2, "output": 8, "cache_read": 0.2, "cache_creation": 2.5}, "2026-09-01T00:00:00Z")
  assert price_provider_usage(usage, snapshot, "2026-08-13T00:00:00Z") == {"source": "snapshot", "currency": "USD", "amount": 0.000449, "pricing_version": "v1"}
  assert price_provider_usage(usage, snapshot, "2026-10-01T00:00:00Z") == {"source": "unpriced", "reason": "pricing_snapshot_expired"}


def test_invalid_pricing_and_secret_shapes_fail_closed():
  usage = normalize_provider_usage(ProviderUsage(1, 1))
  invalid = PricingSnapshot("bad", "USD", "global", "2026-01-01T00:00:00Z", {"input": float("nan"), "output": 1})
  assert price_provider_usage(usage, invalid, "2026-01-02T00:00:00Z")["source"] == "unpriced"
  plan = _plan(options={"headers": {"Authorization": "Bearer secret"}, "accessToken": "secret", "temperature": 0.2})
  assert "secret" not in str(plan)
