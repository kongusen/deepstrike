import json
import pytest
from deepstrike.runtime.context import create_context_preparation_adapter
from deepstrike.runtime.canonical_kernel_step import _action_from_core_step


def test_preserves_canonical_effect_before_provider_projection():
  effect = {"kind": "call_provider", "context": {"system_text": "policy", "turns": []}, "tools": [], "context_candidate": {"marker": "kernel-owned"}}
  action = _action_from_core_step({"disposition": {"kind": "effects", "effects": [{"effect_id": "effect-1", "effect": effect}]}})
  assert action.context_effect == {key: value for key, value in effect.items() if key != "kind"}


def test_context_preparation_delegates_to_core_and_preserves_failure():
  request = {"effect": {"context_candidate": {"marker": "kernel-owned"}}, "request_fingerprint": "request-1", "provider_route": {"route_id": "route-1"}, "prompt_measurement": {"requestFingerprint": "request-1", "inputTokens": 2}}
  result = {"execution_input": {"input_digest": "core-owned"}, "plan": {}, "binding": {}}
  def prepare(raw):
    assert json.loads(raw) == request
    return json.dumps(result)
  assert create_context_preparation_adapter(prepare, lambda _: "true").prepare(request) == result
  def reject(raw):
    raise ValueError("candidate mismatch")
  with pytest.raises(ValueError, match="candidate mismatch"):
    create_context_preparation_adapter(reject, lambda _: "true").prepare(request)


def test_context_verification_delegates_and_propagates_rejection():
  effect = {"context_candidate": {"marker": "kernel-owned"}}
  preparation = {"execution_input": {"input_digest": "recorded"}}
  def verify(raw):
    request = json.loads(raw)
    assert request["effect"] == effect
    if request["preparation"]["execution_input"]["input_digest"] != "recorded":
      raise ValueError("preparation mismatch")
    return "true"
  adapter = create_context_preparation_adapter(lambda _: "{}", verify)
  assert adapter.verify(effect, preparation)
  with pytest.raises(ValueError, match="preparation mismatch"):
    adapter.verify(effect, {"execution_input": {"input_digest": "altered"}})
