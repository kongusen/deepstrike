"""Explicit provider fallback policy for host-side route selection."""
from __future__ import annotations

from collections.abc import AsyncIterator
from typing import Any, Sequence

from .stream import ErrorEvent, StreamEvent


class FallbackProvider:
    """Try providers in order, switching only before the first output event."""

    def __init__(self, providers: Sequence[Any]) -> None:
        if not providers:
            raise ValueError("FallbackProvider requires at least one provider")
        self.providers = tuple(providers)
        self._active = 0

    def descriptor(self) -> Any:
        provider = self.providers[self._active]
        return provider.descriptor() if callable(getattr(provider, "descriptor", None)) else None

    async def complete(self, context, tools, extensions=None):
        last_error: Exception | None = None
        for index, provider in enumerate(self.providers):
            try:
                result = await provider.complete(context, tools, extensions)
                self._active = index
                return result
            except Exception as exc:
                last_error = exc
        raise RuntimeError("all fallback providers failed") from last_error

    async def stream(self, context, tools, extensions=None, state=None) -> AsyncIterator[StreamEvent]:
        last_error: Exception | None = None
        for index, provider in enumerate(self.providers):
            emitted = False
            try:
                async for event in provider.stream(context, tools, extensions, state):
                    event_type = event.get("type") if isinstance(event, dict) else getattr(event, "type", None)
                    if event_type in {"text_delta", "thinking_delta", "tool_call", "tool_delta", "tool_result"}:
                        emitted = True
                    if isinstance(event, ErrorEvent) or event_type == "error":
                        raise RuntimeError(getattr(event, "message", "provider stream error"))
                    yield event
                self._active = index
                return
            except Exception as exc:
                last_error = exc
                if emitted:
                    raise
        raise RuntimeError("all fallback providers failed") from last_error
