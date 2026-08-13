"""Policy-free capability filtering over already resolved provider runtimes."""
from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from .runtime_registry import create_provider_async

if TYPE_CHECKING:
    from .credentials import CredentialResolver, OAuthCredentialResolver
    from .model_catalog import ModelCatalog
    from .model_registry import InputModality, ResolvedProviderRuntime


@dataclass(frozen=True)
class CapabilityRequirement:
    required_input_modalities: tuple["InputModality", ...] = ()
    tools: bool | None = None
    reasoning: bool | None = None
    minimum_context_window: int | None = None

    def as_dict(self) -> dict[str, object]:
        value: dict[str, object] = {}
        if self.required_input_modalities:
            value["required_input_modalities"] = self.required_input_modalities
        if self.tools is not None:
            value["tools"] = self.tools
        if self.reasoning is not None:
            value["reasoning"] = self.reasoning
        if self.minimum_context_window is not None:
            value["minimum_context_window"] = self.minimum_context_window
        return value


@dataclass(frozen=True)
class ProviderCandidate:
    provider_id: str
    model: str
    api_key: str | None = None
    bearer_token: str | None = None
    credential_resolver: "CredentialResolver | OAuthCredentialResolver | None" = None
    model_catalog: "ModelCatalog | None" = None
    protocol: str = "openai"
    region: str | None = None
    base_url: str | None = None


@dataclass(frozen=True)
class CapabilityRouteResult:
    ok: bool
    runtime: "ResolvedProviderRuntime | None" = None
    provider: object | None = None
    error: dict[str, object] | None = None


class CapabilityRouter:
    """First-match capability router. Unknown evidence stays eligible by contract."""

    async def route(
        self,
        requirement: CapabilityRequirement,
        candidates: tuple[ProviderCandidate, ...] | list[ProviderCandidate],
    ) -> CapabilityRouteResult:
        rejected: list[dict[str, object]] = []
        for candidate in candidates:
            try:
                provider = await create_provider_async(
                    candidate.provider_id,
                    api_key=candidate.api_key,
                    bearer_token=candidate.bearer_token,
                    credential_resolver=candidate.credential_resolver,
                    model=candidate.model,
                    protocol=candidate.protocol,
                    region=candidate.region,
                    base_url=candidate.base_url,
                    model_catalog=candidate.model_catalog,
                )
                runtime = provider._resolved_runtime
            except Exception:
                rejected.append({"model": candidate.model, "rejected_by": ["unavailable"]})
                continue
            rejected_by = _rejects(requirement, runtime)
            if not rejected_by:
                return CapabilityRouteResult(ok=True, runtime=runtime, provider=provider)
            rejected.append({"model": candidate.model, "rejected_by": rejected_by})
        return CapabilityRouteResult(
            ok=False,
            error={
                "code": "no_capable_model",
                "requirement": requirement.as_dict(),
                "candidates": rejected,
            },
        )


def _rejects(requirement: CapabilityRequirement, runtime: "ResolvedProviderRuntime") -> list[str]:
    rejected: list[str] = []
    for modality in requirement.required_input_modalities:
        if runtime.effective_capabilities.input_modalities[modality].state == "unsupported":
            rejected.append(f"input:{modality}")
    if requirement.tools is True and runtime.effective_capabilities.tools.state == "unsupported":
        rejected.append("tools")
    if requirement.reasoning is True and runtime.effective_capabilities.reasoning.state == "unsupported":
        rejected.append("reasoning")
    if (
        requirement.minimum_context_window is not None
        and runtime.model is not None
        and runtime.model.context_window is not None
        and runtime.model.context_window < requirement.minimum_context_window
    ):
        rejected.append("context")
    return rejected
