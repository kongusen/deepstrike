import json
from pathlib import Path

from deepstrike.runtime.canonical_kernel_step import canonical_action_from_projection_json


def test_python_adapter_consumes_shared_current_projection_selector() -> None:
  fixture = json.loads((Path(__file__).parents[2] / "tests/fixtures/abi/current_projection_multi_effect.json").read_text())
  expected = fixture["expected"]
  action = canonical_action_from_projection_json(json.dumps({
    "state": "action",
    "action": {
      "kind": expected["action_kind"],
      "effect_id": expected["effect_id"],
      "causation_input_id": expected["causation_input_id"],
      "payload": {"requested_k": expected["payload_requested_k"], "query": {"text": expected["payload_query_text"]}},
    },
  }))
  assert action is not None
  assert action.kind == "query_memory"
  assert action.effect_id == expected["effect_id"]
