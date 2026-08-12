"""Data-driven runtime registry for Python providers.

P-07: centralizes endpoint/protocol resolution, dialect selection, and provider class
instantiation so factories no longer encode per-vendor branches.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable

from .base import LLMProvider, RetryConfig
from .model_registry import (
    GenerationProtocol,
    ModelRegistry,
    ResolvedProviderRuntime,
)


@dataclass(frozen=True)
class WireDialect:
    """Per-(provider, protocol) wire differences for OpenAI-chat compatible vendors."""
    provider_id: str
    protocol: str
    default_model: str
    reasoning_preserve_across_tool_turns: bool = False
    reasoning_requires_replay_for_tool_turns: bool = False
    reasoning_model_ids: frozenset[str] = frozenset()
    tool_unsupported_model_ids: frozenset[str] = frozenset()
    prepare_extensions: "Callable[[dict[str, Any]], dict[str, Any]] | None" = None
    server_tools: "Callable[[dict[str, Any]], list[dict[str, Any]]] | None" = None
    cache_key_mode: str = "openai"  # "openai" | "none"
    inline_thinking_tags: bool = False
    expose_reasoning: "Callable[[dict[str, Any]], bool] | None" = None
    require_reasoning_replay: "Callable[[dict[str, Any]], bool] | None" = None
    replay_strategy: str = "generic_stream"
    capture_reasoning_details: bool = False
    supports_context_cache: bool = False


# OpenAI-chat dialects for official OpenAI and the five CN-compatible vendors.
# These hooks mirror the Node ``openai-chat-dialects.ts`` table. The generic
# OpenAI transport consumes them directly; Qwen keeps its DashScope transport
# while sharing the same dialect metadata.
def _passthrough_extensions(extensions: dict[str, Any]) -> dict[str, Any]:
    return dict(extensions or {})


OPENAI_CHAT_DIALECTS: dict[str, WireDialect] = {
    "openai": WireDialect(
        provider_id="openai",
        protocol="openai-chat",
        default_model="gpt-4o",
        reasoning_preserve_across_tool_turns=False,
        inline_thinking_tags=True,
        expose_reasoning=lambda _ext: True,
        require_reasoning_replay=lambda _ext: False,
    ),
    "deepseek": WireDialect(
        provider_id="deepseek",
        protocol="openai-chat",
        default_model="deepseek-chat",
        reasoning_preserve_across_tool_turns=True,
        reasoning_requires_replay_for_tool_turns=True,
        reasoning_model_ids=frozenset({"deepseek-reasoner", "deepseek-r1", "deepseek-v4-flash", "deepseek-v4-pro"}),
        tool_unsupported_model_ids=frozenset({"deepseek-reasoner", "deepseek-r1"}),
        prepare_extensions=lambda ext: {
            **{k: v for k, v in (ext or {}).items() if k not in {"thinking", "reasoning_effort", "reasoningEffort", "expose_reasoning", "extra_body"}},
            "reasoning_effort": "max" if (ext or {}).get("reasoningEffort") == "max" or (ext or {}).get("reasoning_effort") == "max" else "high",
            "extra_body": {"thinking": {"type": "disabled" if (ext or {}).get("thinking") is False else "enabled"}},
        },
        cache_key_mode="none",
        expose_reasoning=lambda ext: (ext or {}).get("exposeReasoning") is True or (ext or {}).get("expose_reasoning") is True,
        require_reasoning_replay=lambda ext: (ext or {}).get("thinking") is not False,
        replay_strategy="deepseek",
    ),
    "kimi": WireDialect(
        provider_id="kimi",
        protocol="openai-chat",
        default_model="kimi-k2.6",
        cache_key_mode="none",
        inline_thinking_tags=True,
        expose_reasoning=lambda _ext: True,
        require_reasoning_replay=lambda _ext: False,
        supports_context_cache=True,
    ),
    "qwen": WireDialect(
        provider_id="qwen",
        protocol="openai-chat",
        default_model="qwen3.6-plus",
        reasoning_preserve_across_tool_turns=True,
        prepare_extensions=lambda ext: {
            **_passthrough_extensions(ext),
            **({"enable_thinking": True} if (ext or {}).get("enableThinking") else {}),
        },
        cache_key_mode="none",
        inline_thinking_tags=False,
        expose_reasoning=lambda _ext: True,
        require_reasoning_replay=lambda _ext: False,
    ),
    "glm": WireDialect(
        provider_id="glm",
        protocol="openai-chat",
        default_model="glm-5.2",
        prepare_extensions=lambda ext: {
            k: v for k, v in (ext or {}).items() if k != "web_search"
        },
        cache_key_mode="openai",
        inline_thinking_tags=True,
        expose_reasoning=lambda _ext: True,
        require_reasoning_replay=lambda _ext: False,
        server_tools=lambda ext: ([{"type": "web_search", "web_search": ext["web_search"] if isinstance(ext.get("web_search"), dict) else {}}]
                                  if (ext or {}).get("web_search") else []),
    ),
    "minimax": WireDialect(
        provider_id="minimax",
        protocol="openai-chat",
        default_model="MiniMax-M3",
        reasoning_preserve_across_tool_turns=True,
        reasoning_requires_replay_for_tool_turns=True,
        prepare_extensions=lambda ext: {
            **_passthrough_extensions(ext),
            "reasoning_split": (ext or {}).get("reasoning_split", True),
        },
        cache_key_mode="none",
        expose_reasoning=lambda ext: (ext or {}).get("exposeReasoning") is True or (ext or {}).get("expose_reasoning") is True,
        require_reasoning_replay=lambda ext: (ext or {}).get("reasoning_split") is not False,
        replay_strategy="minimax",
        capture_reasoning_details=True,
    ),
}


@dataclass(frozen=True)
class EndpointProfile:
    id: str
    provider_id: str
    protocol: GenerationProtocol
    base_url: str


# Endpoint identity + base URL for every runtime endpoint. Region is encoded in the
# endpoint id for the CN vendors (kimi/glm/qwen) so that endpoint-aware effective
# capabilities and base URL selection are data-driven.
ENDPOINT_PROFILES: dict[str, EndpointProfile] = {
    "anthropic.messages": EndpointProfile("anthropic.messages", "anthropic", "anthropic-messages", "https://api.anthropic.com"),
    "openai.chat": EndpointProfile("openai.chat", "openai", "openai-chat", "https://api.openai.com/v1"),
    "openai.responses": EndpointProfile("openai.responses", "openai", "openai-responses", "https://api.openai.com/v1"),
    "openai.embeddings": EndpointProfile("openai.embeddings", "openai", "openai-chat", "https://api.openai.com/v1"),
    "deepseek.openai": EndpointProfile("deepseek.openai", "deepseek", "openai-chat", "https://api.deepseek.com"),
    "deepseek.anthropic": EndpointProfile("deepseek.anthropic", "deepseek", "anthropic-messages", "https://api.deepseek.com/anthropic"),
    "kimi.global.openai": EndpointProfile("kimi.global.openai", "kimi", "openai-chat", "https://api.moonshot.ai/v1"),
    "kimi.global.anthropic": EndpointProfile("kimi.global.anthropic", "kimi", "anthropic-messages", "https://api.moonshot.ai/anthropic"),
    "kimi.cn.openai": EndpointProfile("kimi.cn.openai", "kimi", "openai-chat", "https://api.moonshot.cn/v1"),
    "kimi.cn.anthropic": EndpointProfile("kimi.cn.anthropic", "kimi", "anthropic-messages", "https://api.moonshot.cn/anthropic"),
    "qwen.global.openai": EndpointProfile("qwen.global.openai", "qwen", "openai-chat", "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
    "qwen.global.anthropic": EndpointProfile("qwen.global.anthropic", "qwen", "anthropic-messages", "https://dashscope-intl.aliyuncs.com/apps/anthropic"),
    "qwen.cn.openai": EndpointProfile("qwen.cn.openai", "qwen", "openai-chat", "https://dashscope.aliyuncs.com/compatible-mode/v1"),
    "glm.global.openai": EndpointProfile("glm.global.openai", "glm", "openai-chat", "https://api.z.ai/api/paas/v4"),
    "glm.global.anthropic": EndpointProfile("glm.global.anthropic", "glm", "anthropic-messages", "https://api.z.ai/api/anthropic"),
    "glm.cn.openai": EndpointProfile("glm.cn.openai", "glm", "openai-chat", "https://open.bigmodel.cn/api/paas/v4"),
    "glm.cn.anthropic": EndpointProfile("glm.cn.anthropic", "glm", "anthropic-messages", "https://open.bigmodel.cn/api/anthropic"),
    "minimax.openai": EndpointProfile("minimax.openai", "minimax", "openai-chat", "https://api.minimaxi.com/v1"),
    "minimax.anthropic": EndpointProfile("minimax.anthropic", "minimax", "anthropic-messages", "https://api.minimaxi.com/anthropic"),
    "gemini.google": EndpointProfile("gemini.google", "gemini", "gemini", "https://generativelanguage.googleapis.com"),
    "ollama.local": EndpointProfile("ollama.local", "ollama", "ollama-chat", "http://localhost:11434"),
}

_REGION_PROVIDERS: frozenset = {"kimi", "glm", "qwen"}


# Provider class map: data-driven instantiation. New compatible vendors can be
# added by extending this table and the endpoint/dialect tables without adding a
# new branch in factory functions.
from .anthropic import AnthropicProvider
from .openai import OpenAIProvider
from .openai_responses import OpenAIResponsesProvider
from .gemini import GeminiProvider
from .ollama import OllamaProvider
from .deepseek import DeepSeekAnthropicProvider
from .kimi import KimiAnthropicProvider
from .qwen import QwenProvider, QwenAnthropicProvider
from .glm import GLMAnthropicProvider
from .minimax import MiniMaxAnthropicProvider

_PROVIDER_CLASSES: dict[tuple[str, str], type[LLMProvider]] = {
    ("anthropic", "anthropic-messages"): AnthropicProvider,
    ("openai", "openai-chat"): OpenAIProvider,
    ("openai", "openai-responses"): OpenAIResponsesProvider,
    ("deepseek", "openai-chat"): OpenAIProvider,
    ("deepseek", "anthropic-messages"): DeepSeekAnthropicProvider,
    ("kimi", "openai-chat"): OpenAIProvider,
    ("kimi", "anthropic-messages"): KimiAnthropicProvider,
    ("qwen", "openai-chat"): QwenProvider,
    ("qwen", "anthropic-messages"): QwenAnthropicProvider,
    ("glm", "openai-chat"): OpenAIProvider,
    ("glm", "anthropic-messages"): GLMAnthropicProvider,
    ("minimax", "openai-chat"): OpenAIProvider,
    ("minimax", "anthropic-messages"): MiniMaxAnthropicProvider,
    ("gemini", "gemini"): GeminiProvider,
    ("ollama", "ollama-chat"): OllamaProvider,
}


def resolve_endpoint(
    provider_id: str,
    model_id: str | None,
    protocol: str,
    region: str | None,
    base_url: str | None,
) -> tuple[str, str]:
    """Resolve endpoint id and base URL.

    Region-aware providers get a region-qualified endpoint id. Custom base_url overrides
    the table but keeps the endpoint identity for dialect/capability selection.
    """
    if region and provider_id in _REGION_PROVIDERS:
        endpoint_id = f"{provider_id}.{region}.{protocol}"
    else:
        # Preserve existing defaults: CN vendors without region use the mainland endpoint.
        # The model registry is consulted for capabilities/runtime policy; endpoint identity
        # is determined here by (provider, protocol, region).
        if provider_id in _REGION_PROVIDERS:
            endpoint_id = f"{provider_id}.cn.{protocol}"
        elif provider_id == "openai":
            endpoint_id = "openai.responses" if protocol == "responses" else "openai.chat"
        elif provider_id == "gemini":
            endpoint_id = "gemini.google"
        elif provider_id == "ollama":
            endpoint_id = "ollama.local"
        else:
            endpoint_id = f"{provider_id}.{protocol}"

    profile = ENDPOINT_PROFILES.get(endpoint_id)
    resolved_base = base_url or (profile.base_url if profile else None)
    if resolved_base is None:
        raise ValueError(f"No base URL known for {endpoint_id!r}")
    return endpoint_id, resolved_base


def resolve_runtime_profile(
    provider_id: str,
    *,
    model: str | None = None,
    protocol: str = "openai",
    region: str | None = None,
    base_url: str | None = None,
) -> tuple[ResolvedProviderRuntime, EndpointProfile | None, WireDialect | None]:
    """Resolve the full runtime profile for a provider construction call."""
    endpoint_id, resolved_base = resolve_endpoint(provider_id, model, protocol, region, base_url)
    profile = ENDPOINT_PROFILES.get(endpoint_id)
    if profile is None:
        raise ValueError(f"Unknown endpoint {endpoint_id!r}")
    dialect = OPENAI_CHAT_DIALECTS.get(provider_id) if profile.protocol == "openai-chat" else None
    model_id = model or (dialect.default_model if dialect is not None else f"{provider_id}/default")
    runtime = model_registry.resolve_provider_runtime(provider_id, model_id, endpoint_id=endpoint_id)
    # Patch base URL into the resolved runtime if caller overrode it.
    return runtime, profile, dialect


def _build_provider(
    cls: type[LLMProvider],
    *,
    api_key: str | None,
    model: str | None,
    base_url: str,
    retry_config: RetryConfig | None,
    dialect: WireDialect | None = None,
) -> LLMProvider:
    kwargs: dict[str, Any] = {"base_url": base_url}
    if api_key is not None:
        kwargs["api_key"] = api_key
    if model is not None:
        kwargs["model"] = model
    if retry_config is not None:
        kwargs["retry_config"] = retry_config
    if dialect is not None:
        kwargs["dialect"] = dialect
    return cls(**kwargs)


def create_provider(
    provider_id: str,
    *,
    api_key: str | None = None,
    model: str | None = None,
    protocol: str = "openai",
    region: str | None = None,
    base_url: str | None = None,
    retry_config: RetryConfig | None = None,
) -> LLMProvider:
    """Data-driven factory: resolve endpoint/protocol and instantiate the right provider class.

    This is the single construction seam used by the public per-vendor factories.
    """
    endpoint_id, resolved_base = resolve_endpoint(provider_id, model, protocol, region, base_url)
    profile = ENDPOINT_PROFILES.get(endpoint_id)
    if profile is None:
        raise ValueError(f"Unknown endpoint {endpoint_id!r}")
    key = (provider_id, profile.protocol)
    cls = _PROVIDER_CLASSES.get(key)
    if cls is None:
        raise ValueError(f"No provider class registered for {key!r}")
    dialect = OPENAI_CHAT_DIALECTS.get(provider_id) if profile.protocol == "openai-chat" else None
    resolved_model = model or (dialect.default_model if dialect is not None else None)
    return _build_provider(cls, api_key=api_key, model=resolved_model, base_url=resolved_base, retry_config=retry_config, dialect=dialect)


model_registry = ModelRegistry()
