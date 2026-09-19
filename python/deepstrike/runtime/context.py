"""Host preparation adapter for the Rust Context authority."""
from __future__ import annotations

import json
from typing import Any, Callable, Protocol, TypedDict
from .evolution import ContextExecutionInput, ContextPlan, ContextState, EvaluationContextBinding


class ContextProviderPreparationRequest(TypedDict):
  effect: dict[str, Any]
  request_fingerprint: str
  provider_route: dict[str, Any]
  prompt_measurement: dict[str, Any]


class ContextPrepared(TypedDict):
  state: ContextState
  provider_route: dict[str, Any]
  prompt_measurement: dict[str, Any]
  execution_input: ContextExecutionInput
  plan: ContextPlan
  binding: EvaluationContextBinding


class ContextPreparationAdapter(Protocol):
  def prepare(self, request: ContextProviderPreparationRequest) -> ContextPrepared: ...
  def verify(self, effect: dict[str, Any], preparation: ContextPrepared) -> bool: ...


def create_context_preparation_adapter(prepare_json: Callable[[str], str], verify_json: Callable[[str], str]) -> ContextPreparationAdapter:
  class Adapter:
    def verify(self, effect: dict[str, Any], preparation: ContextPrepared) -> bool:
      return json.loads(verify_json(json.dumps({"effect": effect, "preparation": preparation}, separators=(",", ":"))))

    def prepare(self, request: ContextProviderPreparationRequest) -> ContextPrepared:
      return json.loads(prepare_json(json.dumps(request, separators=(",", ":"))))
  return Adapter()


def create_native_context_preparation_adapter() -> ContextPreparationAdapter:
  from deepstrike._kernel import context_prepare_json, context_verify_json
  return create_context_preparation_adapter(context_prepare_json, context_verify_json)
