"""Per-backend provider factories — one function per backend (parity with the Node SDK), replacing the
dual ``<Backend>Provider`` / ``<Backend>AnthropicProvider`` class families in the public surface. Where a
backend speaks both the OpenAI- and Anthropic-compatible wire, the ``protocol`` argument selects it (the
two have genuinely different request/replay logic, so they stay distinct internal classes). For OpenAI
itself use the top-level ``OpenAIProvider`` / ``OpenAIResponsesProvider``.

The dual classes still exist (importable from their modules for advanced subclassing); they are simply no
longer part of the public ``deepstrike.providers`` surface.
"""
from __future__ import annotations

from typing import Any, Literal

from .deepseek import DeepSeekProvider, DeepSeekAnthropicProvider
from .kimi import KimiProvider, KimiAnthropicProvider
from .qwen import QwenProvider, QwenAnthropicProvider
from .glm import GLMProvider, GLMAnthropicProvider
from .minimax import MiniMaxOpenAIProvider, MiniMaxAnthropicProvider
from .gemini import GeminiProvider
from .ollama import OllamaProvider

Protocol = Literal["openai", "anthropic"]


def _build(cls, api_key, model, base_url, retry_config):
    kw: dict[str, Any] = {"api_key": api_key}
    if model is not None:
        kw["model"] = model
    if base_url is not None:
        kw["base_url"] = base_url
    if retry_config is not None:
        kw["retry_config"] = retry_config
    return cls(**kw)


def deepseek(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "openai"):
    """DeepSeek. Defaults to the OpenAI-compatible wire (richer reasoning-replay handling)."""
    cls = DeepSeekAnthropicProvider if protocol == "anthropic" else DeepSeekProvider
    return _build(cls, api_key, model, base_url, retry_config)


def kimi(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "openai"):
    """Moonshot Kimi. Defaults to the OpenAI-compatible wire."""
    cls = KimiAnthropicProvider if protocol == "anthropic" else KimiProvider
    return _build(cls, api_key, model, base_url, retry_config)


def qwen(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "openai"):
    """Alibaba Qwen / DashScope. Defaults to the OpenAI-compatible (DashScope) wire."""
    cls = QwenAnthropicProvider if protocol == "anthropic" else QwenProvider
    return _build(cls, api_key, model, base_url, retry_config)


def glm(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "openai"):
    """Zhipu GLM. Defaults to the OpenAI-compatible wire."""
    cls = GLMAnthropicProvider if protocol == "anthropic" else GLMProvider
    return _build(cls, api_key, model, base_url, retry_config)


def minimax(*, api_key, model=None, base_url=None, retry_config=None, protocol: Protocol = "anthropic"):
    """MiniMax. Defaults to the Anthropic-compatible wire (the primary M2.x path)."""
    cls = MiniMaxOpenAIProvider if protocol == "openai" else MiniMaxAnthropicProvider
    return _build(cls, api_key, model, base_url, retry_config)


def gemini(*, api_key, model=None, base_url=None, retry_config=None):
    """Google Gemini (single wire)."""
    return _build(GeminiProvider, api_key, model, base_url, retry_config)


def ollama(*, model=None, base_url=None, retry_config=None):
    """Local Ollama (single wire, no API key)."""
    kw: dict[str, Any] = {}
    if model is not None:
        kw["model"] = model
    if base_url is not None:
        kw["base_url"] = base_url
    if retry_config is not None:
        kw["retry_config"] = retry_config
    return OllamaProvider(**kw)
