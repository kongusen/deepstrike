"""Per-backend provider factories — one function per backend (parity with the Node SDK), replacing the
dual ``<Backend>Provider`` / ``<Backend>AnthropicProvider`` class families in the public surface. Where a
backend speaks both the OpenAI- and Anthropic-compatible wire, the ``protocol`` argument selects it (the
two have genuinely different request/replay logic, so they stay distinct internal classes). For OpenAI
itself use the top-level ``OpenAIProvider`` / ``OpenAIResponsesProvider``.

Vendor-specific implementation classes are private; factories are the only public backend construction
surface.
"""
from __future__ import annotations

from typing import Literal

from .runtime_registry import create_provider

Protocol = Literal["openai", "anthropic"]
Region = Literal["cn", "global"]


def deepseek(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "openai"):
    """DeepSeek. Defaults to the OpenAI-compatible wire (richer reasoning-replay handling)."""
    return create_provider("deepseek", api_key=api_key, model=model, base_url=base_url, retry_config=retry_config, protocol=protocol)


def kimi(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "openai", region: Region | None = None):
    """Moonshot Kimi. Defaults to the OpenAI-compatible wire. ``region`` ("cn"|"global") selects the
    mainland vs international endpoint for the chosen protocol (supply that region's API key); an
    explicit ``base_url`` overrides it. Both protocols exist in both regions."""
    return create_provider("kimi", api_key=api_key, model=model, base_url=base_url, retry_config=retry_config, protocol=protocol, region=region)


def qwen(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "openai"):
    """Alibaba Qwen / DashScope. Defaults to the OpenAI-compatible (DashScope) wire."""
    return create_provider("qwen", api_key=api_key, model=model, base_url=base_url, retry_config=retry_config, protocol=protocol)


def glm(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "openai", region: Region | None = None):
    """Zhipu GLM. Defaults to the OpenAI-compatible wire. ``region`` ("cn"|"global") selects the
    mainland (bigmodel.cn) vs international (z.ai) endpoint for the chosen protocol (supply that
    region's API key); an explicit ``base_url`` overrides it. Both protocols exist in both regions."""
    return create_provider("glm", api_key=api_key, model=model, base_url=base_url, retry_config=retry_config, protocol=protocol, region=region)


def minimax(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "anthropic"):
    """MiniMax. Defaults to the Anthropic-compatible wire (the primary M2.x path)."""
    return create_provider("minimax", api_key=api_key, model=model, base_url=base_url, retry_config=retry_config, protocol=protocol)


def gemini(*, api_key, model=None, base_url=None, retry_config=None):
    """Google Gemini (single wire)."""
    return create_provider("gemini", api_key=api_key, model=model, base_url=base_url, retry_config=retry_config, protocol="gemini")


def ollama(*, model=None, base_url=None, retry_config=None):
    """Local Ollama (single wire, no API key)."""
    return create_provider("ollama", api_key=None, model=model, base_url=base_url, retry_config=retry_config, protocol="ollama")
