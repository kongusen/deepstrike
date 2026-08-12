"""Model Registry — single query entry for Python provider runtime parity.

Mirrors the Node ``model-registry.ts`` contract: model intrinsic facts, protocol capabilities,
and endpoint runtime capabilities are resolved into tri-state effective capabilities.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal

from .base import RuntimePolicy

ModelKind = Literal["generation", "embedding"]
CapabilityState = Literal["supported", "unsupported", "unknown"]
InputModality = Literal["text", "image", "audio", "video", "file"]
OutputModality = Literal["text", "image", "audio", "embedding"]
GenerationProtocol = Literal[
    "anthropic-messages",
    "openai-chat",
    "openai-responses",
    "gemini",
    "ollama-chat",
]
EndpointProtocol = Literal[
    "anthropic-messages",
    "openai-chat",
    "openai-responses",
    "gemini",
    "ollama-chat",
]
CapabilityEvidenceLayer = Literal["model", "protocol", "endpoint"]


@dataclass(frozen=True)
class ModelDescriptor:
    id: str
    provider_id: str
    kind: ModelKind
    context_window: int | None = None
    max_output_tokens: int | None = None
    intrinsic_input_modalities: tuple[InputModality, ...] = ()
    intrinsic_output_modalities: tuple[OutputModality, ...] = ()
    intrinsic_tools: bool | None = None
    intrinsic_reasoning: bool | None = None


@dataclass(frozen=True)
class ModelRegistration:
    descriptor: ModelDescriptor
    default_endpoint_id: str
    recommended_runtime_policy: RuntimePolicy | None = None


@dataclass(frozen=True)
class ProtocolRuntimeCapabilities:
    accepted_input_modalities: tuple[InputModality, ...]
    emitted_output_modalities: tuple[OutputModality, ...]
    tools: bool
    parallel_tool_calls: bool | None = None
    structured_output: bool | None = None
    reasoning_replay: Literal["none", "optional", "required"] = "none"
    prompt_caching: bool | None = None
    image_url: bool | None = None
    image_base64: bool | None = None
    file_id: bool | None = None
    audio_url: bool | None = None
    audio_base64: bool | None = None


@dataclass(frozen=True)
class EndpointRuntimeCapabilities:
    native_token_counting: bool | None = None
    protocol_overrides: "ProtocolRuntimeCapabilities | None" = None


@dataclass(frozen=True)
class EffectiveCapability:
    state: CapabilityState
    value: bool | None = None
    evidence: tuple[CapabilityEvidenceLayer, ...] = ()


@dataclass(frozen=True)
class EffectiveModelCapabilities:
    input_modalities: dict[InputModality, EffectiveCapability]
    output_modalities: dict[OutputModality, EffectiveCapability]
    tools: EffectiveCapability
    reasoning: EffectiveCapability
    parallel_tool_calls: EffectiveCapability
    structured_output: EffectiveCapability
    prompt_caching: EffectiveCapability
    native_token_counting: EffectiveCapability
    image_url: EffectiveCapability
    image_base64: EffectiveCapability
    file_id: EffectiveCapability
    audio_url: EffectiveCapability
    audio_base64: EffectiveCapability


@dataclass(frozen=True)
class ResolvedProviderRuntime:
    provider_id: str
    model_id: str
    endpoint_id: str
    protocol: GenerationProtocol
    model: ModelDescriptor | None
    effective_capabilities: EffectiveModelCapabilities
    runtime_policy: RuntimePolicy | None = None


_PROTOCOL_CAPS: dict[GenerationProtocol, ProtocolRuntimeCapabilities] = {
    "anthropic-messages": ProtocolRuntimeCapabilities(
        accepted_input_modalities=("text", "image", "audio", "file"),
        emitted_output_modalities=("text", "image", "audio", "file"),
        tools=True,
        parallel_tool_calls=True,
        structured_output=True,
        reasoning_replay="optional",
        prompt_caching=True,
        image_url=True,
        image_base64=True,
        file_id=True,
        audio_url=False,
        audio_base64=True,
    ),
    "openai-chat": ProtocolRuntimeCapabilities(
        accepted_input_modalities=("text", "image", "audio", "video", "file"),
        emitted_output_modalities=("text", "audio"),
        tools=True,
        parallel_tool_calls=True,
        structured_output=True,
        reasoning_replay="optional",
        prompt_caching=True,
        image_url=True,
        image_base64=True,
        file_id=True,
        audio_url=False,
        audio_base64=True,
    ),
    "openai-responses": ProtocolRuntimeCapabilities(
        accepted_input_modalities=("text", "image", "audio", "video", "file"),
        emitted_output_modalities=("text", "audio"),
        tools=True,
        parallel_tool_calls=True,
        structured_output=True,
        reasoning_replay="optional",
        prompt_caching=True,
        image_url=True,
        image_base64=True,
        file_id=True,
        audio_url=False,
        audio_base64=True,
    ),
    "gemini": ProtocolRuntimeCapabilities(
        accepted_input_modalities=("text", "image", "audio", "video", "file"),
        emitted_output_modalities=("text", "image", "audio", "file"),
        tools=True,
        parallel_tool_calls=True,
        structured_output=True,
        reasoning_replay="none",
        prompt_caching=True,
        image_url=True,
        image_base64=True,
        file_id=True,
        audio_url=False,
        audio_base64=True,
    ),
    "ollama-chat": ProtocolRuntimeCapabilities(
        accepted_input_modalities=("text", "image", "audio", "video", "file"),
        emitted_output_modalities=("text", "image", "audio", "file"),
        tools=True,
        parallel_tool_calls=False,
        structured_output=False,
        reasoning_replay="none",
        prompt_caching=False,
        image_url=True,
        image_base64=True,
        file_id=False,
        audio_url=False,
        audio_base64=True,
    ),
}

_ENDPOINT_PROTOCOL: dict[str, GenerationProtocol] = {
    "anthropic.messages": "anthropic-messages",
    "openai.chat": "openai-chat",
    "openai.responses": "openai-responses",
    "openai.embeddings": "openai-chat",
    "deepseek.openai": "openai-chat",
    "deepseek.anthropic": "anthropic-messages",
    "kimi.global.openai": "openai-chat",
    "kimi.global.anthropic": "anthropic-messages",
    "kimi.cn.openai": "openai-chat",
    "kimi.cn.anthropic": "anthropic-messages",
    "qwen.global.openai": "openai-chat",
    "qwen.global.anthropic": "anthropic-messages",
    "qwen.cn.openai": "openai-chat",
    "glm.global.openai": "openai-chat",
    "glm.global.anthropic": "anthropic-messages",
    "glm.cn.openai": "openai-chat",
    "glm.cn.anthropic": "anthropic-messages",
    "minimax.openai": "openai-chat",
    "minimax.anthropic": "anthropic-messages",
    "gemini.google": "gemini",
    "gemini.google.embeddings": "gemini",
    "glm.openai.embeddings": "openai-chat",
    "qwen.dashscope.embeddings": "openai-chat",
    "qwen.dashscope.multimodal-embeddings": "openai-chat",
    "ollama.local": "ollama-chat",
    "baai.self-hosted.embeddings": "openai-chat",
}

_ENDPOINT_NATIVE_COUNT = frozenset({
    "anthropic.messages",
    "gemini.google",
})

_DEFAULT_ENDPOINT: dict[str, str] = {
    "anthropic": "anthropic.messages",
    "openai": "openai.chat",
    "deepseek": "deepseek.anthropic",
    "kimi": "kimi.global.anthropic",
    "qwen": "qwen.global.anthropic",
    "glm": "glm.global.anthropic",
    "minimax": "minimax.anthropic",
    "gemini": "gemini.google",
    "ollama": "ollama.local",
    "baai": "baai.self-hosted.embeddings",
}

_KNOWN_PROVIDERS = frozenset(_DEFAULT_ENDPOINT.keys())

_POLICIES: dict[str, RuntimePolicy] = {
    # Anthropic
    "anthropic/claude-opus-4-1": RuntimePolicy(max_turns=50),
    "anthropic/claude-opus-4-7": RuntimePolicy(max_turns=50),
    "anthropic/claude-opus-4-6": RuntimePolicy(max_turns=50),
    "anthropic/claude-opus-4-0": RuntimePolicy(max_turns=50),
    "anthropic/claude-sonnet-4-6": RuntimePolicy(max_turns=25),
    "anthropic/claude-sonnet-4-0": RuntimePolicy(max_turns=25),
    "anthropic/claude-haiku-4-5": RuntimePolicy(max_turns=15),
    "anthropic/claude-3-5-haiku-latest": RuntimePolicy(max_turns=15),
    # OpenAI
    "openai/gpt-5.5": RuntimePolicy(max_turns=60),
    "openai/gpt-5.4": RuntimePolicy(max_turns=50),
    "openai/gpt-4o": RuntimePolicy(max_turns=25),
    "openai/gpt-4o-mini": RuntimePolicy(max_turns=15),
    "openai/gpt-4.1": RuntimePolicy(max_turns=35),
    "openai/gpt-5": RuntimePolicy(max_turns=50),
    "openai/o1": RuntimePolicy(max_turns=50),
    "openai/o3": RuntimePolicy(max_turns=50),
    # DeepSeek
    "deepseek/deepseek-chat": RuntimePolicy(max_turns=25),
    "deepseek/deepseek-reasoner": RuntimePolicy(max_turns=50),
    "deepseek/deepseek-v4-flash": RuntimePolicy(max_turns=20),
    "deepseek/deepseek-v4-pro": RuntimePolicy(max_turns=35),
    # Kimi
    "kimi/moonshot-v1-8k": RuntimePolicy(max_turns=15),
    "kimi/moonshot-v1-32k": RuntimePolicy(max_turns=20),
    "kimi/moonshot-v1-128k": RuntimePolicy(max_turns=30),
    "kimi/kimi-k2.5": RuntimePolicy(max_turns=30),
    "kimi/kimi-k2.6": RuntimePolicy(max_turns=35),
    "kimi/kimi-k2-thinking": RuntimePolicy(max_turns=50),
    # Qwen
    "qwen/qwen3.6-plus": RuntimePolicy(max_turns=35),
    "qwen/qwen3.6-flash": RuntimePolicy(max_turns=20),
    "qwen/qwen3.5-plus": RuntimePolicy(max_turns=35),
    # GLM
    "glm/glm-5.2": RuntimePolicy(max_turns=50),
    "glm/glm-4-plus": RuntimePolicy(max_turns=35),
    "glm/glm-4-flash": RuntimePolicy(max_turns=15),
    # MiniMax
    "minimax/MiniMax-M3": RuntimePolicy(max_turns=35),
    "minimax/MiniMax-M2.5": RuntimePolicy(max_turns=25),
    "minimax/MiniMax-M2": RuntimePolicy(max_turns=20),
    # Gemini
    "gemini/gemini-3-pro-preview": RuntimePolicy(max_turns=50),
    "gemini/gemini-2.5-pro": RuntimePolicy(max_turns=35),
    "gemini/gemini-2.0-flash": RuntimePolicy(max_turns=15),
    "gemini/gemini-2.0-flash-lite": RuntimePolicy(max_turns=10),
    "gemini/gemini-1.5-pro": RuntimePolicy(max_turns=30),
    # Ollama
    "ollama/llama3": RuntimePolicy(max_turns=20),
    "ollama/deepseek-r1": RuntimePolicy(max_turns=40),
}


def _endpoint_for(provider_id: str, model_id: str) -> str:
    """Runtime endpoint selection based on provider/model naming conventions."""
    if provider_id == "openai":
        if model_id.startswith("text-embedding-"):
            return "openai.embeddings"
        if any(model_id.startswith(p) for p in ("gpt-5", "gpt-4.1", "o3", "o4-mini")):
            return "openai.responses"
    if provider_id == "qwen":
        if model_id.startswith("text-embedding-"):
            return "qwen.dashscope.embeddings"
        if model_id in ("qwen2.5-vl-embedding", "qwen3-vl-embedding"):
            return "qwen.dashscope.multimodal-embeddings"
    if provider_id == "gemini" and model_id.startswith("gemini-embedding-"):
        return "gemini.google.embeddings"
    if provider_id == "glm" and model_id.startswith("embedding-"):
        return "glm.openai.embeddings"
    return _DEFAULT_ENDPOINT[provider_id]


def _model_kind(endpoint_id: str) -> ModelKind:
    return "embedding" if "embeddings" in endpoint_id else "generation"


def _resolve_effective_capability(
    layers: list[tuple[CapabilityEvidenceLayer, CapabilityState, bool | None]],
) -> EffectiveCapability:
    evidence: list[CapabilityEvidenceLayer] = []
    decided_value: bool | None = None
    for layer, state, value in layers:
        if state != "unknown":
            evidence.append(layer)
        if state == "unsupported":
            return EffectiveCapability(state="unsupported", evidence=tuple(evidence))
        if state == "supported" and decided_value is None and value is not None:
            decided_value = value
    if all(state == "supported" for _, state, _ in layers) and layers:
        return EffectiveCapability(state="supported", value=decided_value, evidence=tuple(evidence))
    return EffectiveCapability(state="unknown", evidence=tuple(evidence))


def _boolean_state(value: bool | None) -> CapabilityState:
    return "unknown" if value is None else ("supported" if value else "unsupported")


def _membership_state(values: tuple[str, ...] | None, value: str) -> CapabilityState:
    if values is None:
        return "unknown"
    return "supported" if value in values else "unsupported"


def resolve_effective_capabilities(
    model: ModelDescriptor,
    endpoint_id: str,
    endpoint_overrides: EndpointRuntimeCapabilities | None = None,
) -> EffectiveModelCapabilities:
    """Tri-state effective capability resolution (model ∩ protocol ∩ endpoint)."""
    protocol = _ENDPOINT_PROTOCOL.get(endpoint_id)
    if protocol is None:
        raise ValueError(f"Unknown endpoint {endpoint_id!r}")
    proto = _PROTOCOL_CAPS[protocol]
    overrides = endpoint_overrides.protocol_overrides if endpoint_overrides else None

    def media_cap(key: str) -> EffectiveCapability:
        proto_value = getattr(proto, key, None)
        over_value = getattr(overrides, key, None) if overrides else None
        layers = [("protocol", _boolean_state(proto_value), proto_value)]
        if overrides and over_value is not None:
            layers.append(("endpoint", _boolean_state(over_value), over_value))
        return _resolve_effective_capability(layers)

    def modality_layers(
        model_values: tuple[str, ...] | None,
        protocol_values: tuple[str, ...],
        override_values: tuple[str, ...] | None,
        modality: str,
    ) -> list[tuple[CapabilityEvidenceLayer, CapabilityState, bool | None]]:
        layers: list[tuple[CapabilityEvidenceLayer, CapabilityState, bool | None]] = [
            ("model", _membership_state(model_values, modality), None),
            ("protocol", _membership_state(protocol_values, modality), None),
        ]
        if overrides and override_values is not None:
            layers.append(("endpoint", _membership_state(override_values, modality), None))
        return layers

    def boolean_layers(
        model_state: CapabilityState,
        model_value: bool | None,
        protocol_state: CapabilityState,
        protocol_value: bool | None,
        override_value: bool | None,
    ) -> list[tuple[CapabilityEvidenceLayer, CapabilityState, bool | None]]:
        layers: list[tuple[CapabilityEvidenceLayer, CapabilityState, bool | None]] = [
            ("model", model_state, model_value),
            ("protocol", protocol_state, protocol_value),
        ]
        if overrides and override_value is not None:
            layers.append(("endpoint", _boolean_state(override_value), override_value))
        return layers

    return EffectiveModelCapabilities(
        input_modalities={
            m: _resolve_effective_capability(modality_layers(
                model.intrinsic_input_modalities or None,
                proto.accepted_input_modalities,
                overrides.accepted_input_modalities if overrides else None,
                m,
            ))
            for m in ("text", "image", "audio", "video", "file")
        },
        output_modalities={
            m: _resolve_effective_capability(modality_layers(
                model.intrinsic_output_modalities or None,
                proto.emitted_output_modalities,
                overrides.emitted_output_modalities if overrides else None,
                m,
            ))
            for m in ("text", "image", "audio", "embedding")
        },
        tools=_resolve_effective_capability(boolean_layers(
            _boolean_state(model.intrinsic_tools),
            model.intrinsic_tools,
            _boolean_state(proto.tools),
            proto.tools,
            overrides.tools if overrides else None,
        )),
        reasoning=_resolve_effective_capability(boolean_layers(
            _boolean_state(model.intrinsic_reasoning),
            model.intrinsic_reasoning,
            _boolean_state(proto.reasoning_replay != "none"),
            None,
            overrides.reasoning_replay != "none" if overrides and overrides.reasoning_replay is not None else None,
        )),
        parallel_tool_calls=_resolve_effective_capability(boolean_layers(
            "unknown",
            None,
            _boolean_state(proto.parallel_tool_calls),
            proto.parallel_tool_calls,
            overrides.parallel_tool_calls if overrides else None,
        )),
        structured_output=_resolve_effective_capability(boolean_layers(
            "unknown",
            None,
            _boolean_state(proto.structured_output),
            proto.structured_output,
            overrides.structured_output if overrides else None,
        )),
        prompt_caching=_resolve_effective_capability(boolean_layers(
            "unknown",
            None,
            _boolean_state(proto.prompt_caching),
            proto.prompt_caching,
            overrides.prompt_caching if overrides else None,
        )),
        native_token_counting=_resolve_effective_capability(
            [("endpoint", "supported", True)]
            if endpoint_id in _ENDPOINT_NATIVE_COUNT
            else [("endpoint", "unknown", None)]
        ),
        image_url=media_cap("image_url"),
        image_base64=media_cap("image_base64"),
        file_id=media_cap("file_id"),
        audio_url=media_cap("audio_url"),
        audio_base64=media_cap("audio_base64"),
    )

def normalize_model_id(provider_id: str, model_id: str) -> str:
    prefix = f"{provider_id}/"
    return model_id[len(prefix):] if model_id.startswith(prefix) else model_id


def provider_prefix(model_id: str) -> str | None:
    if "/" not in model_id:
        return None
    prefix = model_id.split("/", 1)[0]
    return prefix if prefix in _KNOWN_PROVIDERS else None


class ModelRegistry:
    """Single query entry for model facts and default endpoint resolution."""

    def resolve(
        self,
        model_id: str,
        provider_id: str | None = None,
    ) -> ModelRegistration | None:
        if provider_id is None:
            provider_id = provider_prefix(model_id)
        if provider_id is None or provider_id not in _KNOWN_PROVIDERS:
            return None
        raw_model = normalize_model_id(provider_id, model_id)
        endpoint_id = _endpoint_for(provider_id, raw_model)
        kind = _model_kind(endpoint_id)
        descriptor = ModelDescriptor(
            id=f"{provider_id}/{raw_model}",
            provider_id=provider_id,
            kind=kind,
        )
        policy = _POLICIES.get(descriptor.id)
        return ModelRegistration(
            descriptor=descriptor,
            default_endpoint_id=endpoint_id,
            recommended_runtime_policy=policy,
        )

    def resolve_provider_runtime(
        self,
        provider_id: str,
        model_id: str,
        endpoint_id: str | None = None,
        endpoint_overrides: EndpointRuntimeCapabilities | None = None,
    ) -> ResolvedProviderRuntime:
        """One-shot resolution of model + endpoint + effective capabilities."""
        registration = self.resolve(model_id, provider_id)
        descriptor = registration.descriptor if registration else None
        if descriptor is None:
            raw_model = normalize_model_id(provider_id, model_id)
            descriptor = ModelDescriptor(
                id=f"{provider_id}/{raw_model}",
                provider_id=provider_id,
                kind="generation",
            )
        endpoint_id = endpoint_id or registration.default_endpoint_id if registration else _DEFAULT_ENDPOINT.get(provider_id, "openai.chat")
        protocol = _ENDPOINT_PROTOCOL.get(endpoint_id)
        if protocol is None:
            protocol = "openai-chat"
        return ResolvedProviderRuntime(
            provider_id=provider_id,
            model_id=descriptor.id.split("/", 1)[1],
            endpoint_id=endpoint_id,
            protocol=protocol,
            model=descriptor,
            effective_capabilities=resolve_effective_capabilities(descriptor, endpoint_id, endpoint_overrides),
            runtime_policy=registration.recommended_runtime_policy if registration else None,
        )


model_registry = ModelRegistry()


def get_runtime_policy(provider_id: str, model_id: str) -> RuntimePolicy:
    registration = model_registry.resolve(model_id, provider_id)
    return registration.recommended_runtime_policy if registration else RuntimePolicy()
