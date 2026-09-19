"""Framework Evolution Runtime façade backed by the Rust E1–E8 validator."""

from __future__ import annotations

from dataclasses import dataclass
import json
from typing import Any, Callable, Mapping, Protocol, TypedDict

EVOLUTION_REPORT_SCHEMA = "evolution-report/v1"
EvolutionValidateJson = Callable[[str], str]


EvolutionBundle = Mapping[str, Any]


class EvaluationContextBinding(TypedDict, total=False):
  """SDK mirror for the context evidence binding checked by the Rust core.

  The canonical payload requires ``digest``, ``operation_id``, ``context_policy``,
  ``input_snapshot``, ``rendered_snapshot``, and ``prompt_measurement``. ``cache_prefix``
  is optional and, when present, must also be listed in ``evidence_refs``.
  """

  digest: str
  operation_id: str
  context_policy: str
  input_snapshot: str
  rendered_snapshot: str
  prompt_measurement: str
  cache_prefix: str


class EvaluationRun(TypedDict):
  """Python SDK mirror for the canonical evaluation run shape."""

  digest: str
  proposal: str
  baseline_artifact_set: str
  candidate_artifact_set: str
  evaluator: str
  dataset: str
  operation_ids: list[str]
  contexts: list[EvaluationContextBinding]
  evidence_refs: list[str]


@dataclass(frozen=True, slots=True)
class EvolutionReport:
  schema: str
  verdict: str
  violations: tuple[Mapping[str, Any], ...]


class EvolutionRuntimeAdapter(Protocol):
  def validate(self, bundle: Mapping[str, Any]) -> EvolutionReport: ...


class EvolutionStore(Protocol):
  def load_bundle(self) -> Mapping[str, Any]: ...


def create_evolution_runtime_adapter(validate_json: EvolutionValidateJson) -> EvolutionRuntimeAdapter:
  class Adapter:
    def validate(self, bundle: Mapping[str, Any]) -> EvolutionReport:
      result = json.loads(validate_json(json.dumps(bundle, separators=(",", ":"))))
      return EvolutionReport(
        schema=str(result["schema"]),
        verdict=str(result["verdict"]),
        violations=tuple(result.get("violations", ())),
      )

  return Adapter()


def create_native_evolution_runtime_adapter() -> EvolutionRuntimeAdapter:
  from deepstrike._kernel import evolution_validate_json

  return create_evolution_runtime_adapter(evolution_validate_json)


class EvolutionRuntime:
  """Storage-neutral evolution handle. Persistence stays in host adapters."""

  def __init__(self, adapter: EvolutionRuntimeAdapter) -> None:
    self._adapter = adapter

  def validate(self, bundle: Mapping[str, Any]) -> EvolutionReport:
    return self._adapter.validate(bundle)

  def validate_store(self, store: EvolutionStore) -> EvolutionReport:
    return self.validate(store.load_bundle())

  def activate(self, bundle: Mapping[str, Any], operation_id: str) -> Mapping[str, Any]:
    report = self.validate(bundle)
    if report.verdict != "pass":
      codes = ", ".join(str(item.get("code", "")) for item in report.violations)
      raise ValueError(f"evolution bundle is not activatable: {codes}")
    activations = bundle.get("activations", ())
    for activation in activations:
      if activation.get("operation_id") == operation_id:
        return activation
    raise ValueError(f"no verified activation binding for operation {operation_id}")
