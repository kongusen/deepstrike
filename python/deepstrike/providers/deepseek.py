from __future__ import annotations
import logging
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .base import RetryConfig, ProviderDescriptor, RenderedContext, RuntimePolicy
from .openai import OpenAIProvider, OpenAIChatTurnReasoning
from .anthropic_compatible import AnthropicCompatibleProvider
from .vendor_profiles import DEEPSEEK_POLICIES as _DEEPSEEK_POLICIES, ANTHROPIC_VENDOR_PROFILES

logger = logging.getLogger(__name__)

_DEEPSEEK_BASE_URL = "https://api.deepseek.com/v1"
_REASONER_MODELS = {"deepseek-reasoner", "deepseek-r1"}
# Models whose tool turns require a non-empty reasoning_content replay (fail fast
# rather than send a request the provider rejects with 400).
_DEEPSEEK_REASONING_MODELS = {"deepseek-reasoner", "deepseek-r1", "deepseek-v4-flash", "deepseek-v4-pro"}


class DeepSeekAnthropicProvider(AnthropicCompatibleProvider):
    """DeepSeek over its Anthropic-compatible endpoint.

    Deprecated: prefer ``deepseek(protocol="anthropic")``. Behavior is data-driven
    via ``ANTHROPIC_VENDOR_PROFILES["deepseek"]``; this thin shim is kept for
    backward compatibility and ``isinstance`` checks.
    """

    def __init__(
        self,
        api_key: str,
        model: str | None = None,
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        super().__init__(ANTHROPIC_VENDOR_PROFILES["deepseek"], api_key, model, retry_config, base_url)


class DeepSeekProvider(OpenAIProvider):
    """DeepSeek provider.

    deepseek-chat: full tool calling
    deepseek-reasoner / deepseek-r1: chain-of-thought via reasoning_content, NO tool calling

    extensions:
      expose_reasoning (bool): prepend <think>…</think> to content
    """

    def __init__(self, api_key: str, model: str = "deepseek-chat", retry_config: RetryConfig | None = None, base_url: str = _DEEPSEEK_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _DEEPSEEK_POLICIES.get(self._model, RuntimePolicy())

    def descriptor(self) -> ProviderDescriptor:
        return ProviderDescriptor(
            provider="deepseek",
            protocol="openai-chat",
            model=self._model,
            reasoning={"supported": True, "preserve_across_tool_turns": True, "requires_replay_for_tool_turns": True},
            tool_calls={"supported": True, "requires_strict_pairing": True},
        )

    def _require_non_empty_reasoning_replay_for_tool_turns(self, extensions: dict | None) -> bool:
        if (extensions or {}).get("thinking") is False:
            return False
        return self._model in _DEEPSEEK_REASONING_MODELS

    def _remember_deepseek_replay(
        self,
        content: str,
        tool_calls: list[ToolCall],
        reasoning_content: object,
        native_tool_calls: list | None = None,
    ) -> None:
        """Persist a provider-scoped replay envelope only when real reasoning was
        produced. A missing/empty reasoning_content is never synthesized."""
        if not (isinstance(reasoning_content, str) and reasoning_content.strip()):
            return
        envelope: dict = {
            "schema_version": 2,
            "provider": "deepseek",
            "protocol": "openai-chat",
            "model": self._model,
            "reasoning_content": reasoning_content,
        }
        if native_tool_calls:
            envelope["tool_calls"] = native_tool_calls
        self.remember_replay_fields(
            Message(role="assistant", content=content, tool_calls=tool_calls or None),
            envelope,
        )

    # ── Template-Method seams (complete()/stream() inherited from OpenAIProvider) ──

    def _cache_key_params(self, context: RenderedContext, tools: list[ToolSchema]) -> dict:
        # DeepSeek 400s on unknown params and auto prefix-caches, so never send prompt_cache_key
        # (mirrors the Node DeepSeekProvider.cacheKeyParams).
        return {}

    def _wire_tools(self, tools: list[ToolSchema], extensions: dict | None = None) -> list[dict] | None:
        # Reasoner models (deepseek-reasoner / deepseek-r1) do not support tool calling.
        if self._model in _REASONER_MODELS:
            return None
        return super()._wire_tools(tools, extensions)

    def _prepare_extensions(self, extensions: dict | None) -> dict:
        # DeepSeek-native thinking control (reasoning_effort + extra_body.thinking), mirroring the Node
        # DeepSeekProvider. Vendor-aware: only the reasoning models accept it, and DeepSeek 400s on
        # unknown params — so deepseek-chat (non-reasoning) gets a clean pass-through. The thinking
        # CONTROL keys are stripped from the wire (re-expressed as reasoning_effort / extra_body);
        # `thinking` stays readable on the raw extensions for _require_non_empty_… (which reads them).
        # [verify] reasoning_effort acceptance against current DeepSeek docs before a live ship.
        ext = dict(extensions or {})
        if self._model not in _DEEPSEEK_REASONING_MODELS:
            return ext
        thinking = "disabled" if ext.get("thinking") is False else "enabled"
        reasoning_effort = "max" if (ext.get("reasoning_effort") or ext.get("reasoningEffort")) == "max" else "high"
        cleaned = {k: v for k, v in ext.items() if k not in {"thinking", "reasoning_effort", "reasoningEffort", "expose_reasoning", "extra_body"}}
        cleaned["reasoning_effort"] = reasoning_effort
        cleaned["extra_body"] = {"thinking": {"type": thinking}}
        return cleaned

    def _uses_inline_thinking_tags(self) -> bool:
        # Reasoning arrives out-of-band as reasoning_content, never as inline <thinking> tags.
        return False

    def _expose_reasoning_delta(self, extensions: dict | None) -> bool:
        return bool((extensions or {}).get("expose_reasoning"))

    def _remember_complete_replay(self, content: str, tool_calls: list, reasoning: OpenAIChatTurnReasoning) -> None:
        self._remember_deepseek_replay(content, tool_calls, reasoning.reasoning_content, reasoning.native_tool_calls)

    def _remember_stream_replay(self, content: str, tool_calls: list, reasoning: OpenAIChatTurnReasoning) -> None:
        self._remember_deepseek_replay(content, tool_calls, reasoning.reasoning_content, reasoning.native_tool_calls)
