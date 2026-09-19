"""Framework Evolution Runtime façade backed by the Rust E1–E8 validator."""

from __future__ import annotations

from dataclasses import dataclass
import json
from typing import Any, Callable, Mapping, Protocol

EVOLUTION_REPORT_SCHEMA = "evolution-report/v1"
EvolutionValidateJson = Callable[[str], str]


EvolutionBundle = Mapping[str, Any]


@dataclass(frozen=True, slots=True)
class EvolutionReport:
  schema: str
  verdict: str
  violations: tuple[Mapping[str, Any], ...]


class EvolutionRuntimeAdapter(Protocol):
  def validate(self, bundle: Mapping[str, Any]) -> EvolutionReport: ...


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
