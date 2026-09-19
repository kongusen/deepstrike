"""Framework Evolution Runtime façade backed by the Rust E1–E8 validator."""

from __future__ import annotations

from dataclasses import dataclass
import json
from typing import Any, Callable, Mapping, Protocol, TypedDict, Literal

EVOLUTION_REPORT_SCHEMA = "evolution-report/v1"
EvolutionValidateJson = Callable[[str], str]


EvolutionBundle = Mapping[str, Any]


class _EvaluationContextBindingRequired(TypedDict):
  """SDK mirror for the context evidence binding checked by the Rust core.

  The canonical payload requires ``digest``, ``operation_id``, ``execution_input``,
  ``context_state``, ``context_policy``, ``context_plan``, ``rendered_snapshot``,
  ``prompt_measurement``, and ``provider_route``. ``cache_prefix`` is optional and, when
  present, must also be listed in ``evidence_refs``.
  """

  digest: str
  operation_id: str
  execution_input: str
  context_state: str
  context_policy: str
  context_plan: str
  rendered_snapshot: str
  prompt_measurement: str
  provider_route: str


class EvaluationContextBinding(_EvaluationContextBindingRequired, total=False):
  """Canonical required evidence identities plus an optional cache evidence reference."""

  cache_prefix: str


class ContextEntryRef(TypedDict):
  entry_id: str
  content_digest: str
  source: Literal["system", "knowledge", "history", "state", "signal"]
  ordinal: int


class ContextState(TypedDict):
  schema: Literal["context/v1"]
  generation: int
  system: list[ContextEntryRef]
  knowledge: list[ContextEntryRef]
  history: list[ContextEntryRef]
  state: list[ContextEntryRef]
  task_state: str
  signals: list[str]
  digest: str


class ContextSelection(TypedDict):
  entry_id: str
  action: Literal["include", "excerpt", "collapse", "page_out", "omit"]
  reason: str


class CachePrefixBoundary(TypedDict):
  digest: str
  entries: int


class ContextPlan(TypedDict):
  schema: Literal["context/v1"]
  plan_id: str
  operation_id: str
  step_id: str
  state_digest: str
  state_generation: int
  runtime_inputs: str
  policy_digest: str
  provider_profile_digest: str
  measurement_fingerprints: list[str]
  selections: list[ContextSelection]
  input_budget_tokens: int
  projected_tokens: int
  pressure_ppm: int
  cache_prefix: CachePrefixBoundary | None


class ContextExecutionInput(TypedDict):
  schema: Literal["context/v1"]
  input_digest: str
  operation_id: str
  step_id: str
  input_sequence: int
  state_digest: str
  policy_digest: str
  plan_digest: str
  rendered_snapshot: str
  prompt_measurement: str
  provider_route: str
  cache_prefix: CachePrefixBoundary | None


class ContextPreparationRequest(TypedDict):
  operation_id: str
  step_id: str
  input_sequence: int
  policy_digest: str
  prompt_measurement: str
  provider_route: str


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
