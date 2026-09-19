import json

from deepstrike.runtime.evolution import (
    EVOLUTION_REPORT_SCHEMA,
    EvolutionRuntime,
    create_evolution_runtime_adapter,
)


def test_evolution_runtime_delegates_to_the_canonical_adapter():
  requests = []

  def validate_json(request):
    requests.append(request)
    return json.dumps({"schema": EVOLUTION_REPORT_SCHEMA, "verdict": "pass", "violations": []})

  runtime = EvolutionRuntime(create_evolution_runtime_adapter(validate_json))
  report = runtime.validate({
    "artifacts": [], "artifact_sets": [], "proposals": [], "evaluations": [],
    "facts": [], "decisions": [], "activations": [],
  })
  assert report.verdict == "pass"
  assert json.loads(requests[0])["artifact_sets"] == []
