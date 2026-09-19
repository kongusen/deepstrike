import json

from deepstrike.runtime.evolution import (
    EVOLUTION_REPORT_SCHEMA,
    EvolutionRuntime,
    EvolutionStore,
    create_evolution_runtime_adapter,
)

BUNDLE = {
    "artifacts": [], "artifact_sets": [], "proposals": [], "evaluations": [], "facts": [], "decisions": [],
    "activations": [{"digest": "sha256:activation", "operation_id": "op-1", "artifact_set": "sha256:set", "promotion_decision": "sha256:decision"}],
}


def test_evolution_runtime_delegates_to_the_canonical_adapter():
  requests = []

  def validate_json(request):
    requests.append(request)
    return json.dumps({"schema": EVOLUTION_REPORT_SCHEMA, "verdict": "pass", "violations": []})

  runtime = EvolutionRuntime(create_evolution_runtime_adapter(validate_json))
  report = runtime.validate(BUNDLE)
  assert report.verdict == "pass"
  assert json.loads(requests[0])["artifact_sets"] == []


def test_evolution_runtime_validates_store_before_activation():
  class Store:
    def load_bundle(self):
      return BUNDLE

  runtime = EvolutionRuntime(create_evolution_runtime_adapter(
    lambda _request: json.dumps({"schema": EVOLUTION_REPORT_SCHEMA, "verdict": "pass", "violations": []})
  ))
  assert runtime.validate_store(Store()).verdict == "pass"
  assert runtime.activate(BUNDLE, "op-1")["artifact_set"] == "sha256:set"


def test_evolution_runtime_fails_closed_for_rejected_activation():
  runtime = EvolutionRuntime(create_evolution_runtime_adapter(
    lambda _request: json.dumps({"schema": EVOLUTION_REPORT_SCHEMA, "verdict": "fail", "violations": [{"code": "E7"}]})
  ))
  try:
    runtime.activate(BUNDLE, "op-1")
  except ValueError as error:
    assert "E7" in str(error)
  else:
    raise AssertionError("rejected evolution bundle must not activate")
