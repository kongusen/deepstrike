from __future__ import annotations

from deepstrike._kernel import Message, ToolCall, ToolSchema
from .base import ProviderDescriptor, RenderedContext, RetryConfig, RuntimePolicy
from .openai import OpenAIProvider, OpenAIChatTurnReasoning
from .anthropic_compatible import AnthropicCompatibleProvider
from .vendor_profiles import MINIMAX_POLICIES as _MINIMAX_POLICIES, ANTHROPIC_VENDOR_PROFILES

_MINIMAX_OPENAI_BASE = "https://api.minimaxi.com/v1"


class MiniMaxAnthropicProvider(AnthropicCompatibleProvider):
    """MiniMax over its Anthropic-compatible endpoint. Replay is carried as
    Anthropic ``native_blocks`` (thinking / text / tool_use).

    Deprecated: prefer ``minimax(protocol="anthropic")``. Data-driven via
    ``ANTHROPIC_VENDOR_PROFILES["minimax"]``; thin shim for backward compat / isinstance.
    """

    def __init__(
        self,
        api_key: str,
        model: str | None = None,
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        super().__init__(ANTHROPIC_VENDOR_PROFILES["minimax"], api_key, model, retry_config, base_url)


class MiniMaxOpenAIProvider(OpenAIProvider):
    """MiniMax over its OpenAI-compatible endpoint. Replay is carried as
    ``reasoning_content`` / ``reasoning_details`` (split reasoning); requests
    default to ``reasoning_split: true``."""

    def __init__(
        self,
        api_key: str,
        model: str = "MiniMax-M3",
        retry_config: RetryConfig | None = None,
        base_url: str = _MINIMAX_OPENAI_BASE,
    ):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _MINIMAX_POLICIES.get(self._model, RuntimePolicy())

    def descriptor(self) -> ProviderDescriptor:
        return ProviderDescriptor(
            provider="minimax",
            protocol="openai-chat",
            model=self._model,
            reasoning={"supported": True, "preserve_across_tool_turns": True, "requires_replay_for_tool_turns": True},
            tool_calls={"supported": True, "requires_strict_pairing": True},
        )

    def _reasoning_split_enabled(self, extensions: dict | None) -> bool:
        return (extensions or {}).get("reasoning_split") is not False

    def _require_non_empty_reasoning_replay_for_tool_turns(self, extensions: dict | None) -> bool:
        return self._reasoning_split_enabled(extensions)

    # ── Template-Method seams (complete()/stream() inherited from OpenAIProvider) ──

    def _cache_key_params(self, context: RenderedContext, tools: list[ToolSchema]) -> dict:
        # MiniMax auto prefix-caches and does not accept OpenAI's prompt_cache_key; omit it.
        return {}

    def _prepare_extensions(self, extensions: dict | None) -> dict:
        # Force reasoning_split onto the wire so reasoning is returned out-of-band.
        return {**(extensions or {}), "reasoning_split": self._reasoning_split_enabled(extensions)}

    def _uses_inline_thinking_tags(self) -> bool:
        # Reasoning arrives out-of-band (reasoning_content / reasoning_details), never inline tags.
        return False

    def _expose_reasoning_delta(self, extensions: dict | None) -> bool:
        return bool((extensions or {}).get("expose_reasoning"))

    def _capture_reasoning_details(self) -> bool:
        return True

    def _remember_minimax_replay(
        self,
        content: str,
        tool_calls: list[ToolCall],
        reasoning_content: object,
        reasoning_details: object,
        native_tool_calls: list | None,
    ) -> None:
        has_reasoning = isinstance(reasoning_content, str) and reasoning_content.strip()
        has_details = reasoning_details is not None
        if not has_reasoning and not has_details:
            return
        envelope: dict = {
            "schema_version": 2,
            "provider": "minimax",
            "protocol": "openai-chat",
            "model": self._model,
        }
        if has_reasoning:
            envelope["reasoning_content"] = reasoning_content
        if has_details:
            envelope["reasoning_details"] = reasoning_details
        if native_tool_calls:
            envelope["tool_calls"] = native_tool_calls
        self.remember_replay_fields(
            Message(role="assistant", content=content, tool_calls=tool_calls or None),
            envelope,
        )

    def _remember_complete_replay(self, content: str, tool_calls: list, reasoning: OpenAIChatTurnReasoning) -> None:
        self._remember_minimax_replay(content, tool_calls, reasoning.reasoning_content, reasoning.reasoning_details, reasoning.native_tool_calls)

    def _remember_stream_replay(self, content: str, tool_calls: list, reasoning: OpenAIChatTurnReasoning) -> None:
        self._remember_minimax_replay(content, tool_calls, reasoning.reasoning_content, reasoning.reasoning_details, reasoning.native_tool_calls)
