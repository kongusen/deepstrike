"""ReplayProvider — an LLMProvider that emits previously-recorded assistant messages
instead of calling a real LLM API.

Python port of node/src/runtime/replay-provider.ts. See that file for the full design
rationale. Distinct from `provider_replay` / `seed_provider_replay` which is the
session-repair reasoning-content cache (does NOT skip LLM calls).

Cost-accounting under replay:
- inputTokens estimated from the rendered context this call carries (NOT recorded).
- outputTokens taken from msg.token_count when present; else chars / 4.
- cacheReadInputTokens / cacheCreationInputTokens emitted as 0 — replay has no real
  cache state.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, AsyncIterator, Callable, Sequence

from deepstrike._kernel import Message, ToolSchema  # type: ignore
from deepstrike.providers.base import (
    LLMProvider,  # type: ignore[attr-defined]
    ProviderDescriptor,
    ProviderRunState,
    RenderedContext,
    StreamEvent,
)


@dataclass
class ReplayProviderOpts:
    """Options for ReplayProvider construction."""

    #: Maps a rendered-context text payload to a token count. Defaults to chars / 4.
    tokenizer: Callable[[str], int] | None = None
    #: Provider descriptor advertised via descriptor(). Defaults to a generic replay descriptor.
    descriptor: ProviderDescriptor | None = None
    #: When True, stream() and complete() wrap to the start once the fixture is exhausted.
    wrap: bool = False


_DEFAULT_DESCRIPTOR: ProviderDescriptor = {
    "provider": "replay",
    "protocol": "openai-chat",
    "model": "replay",
    "reasoning": {"supported": False, "preserveAcrossToolTurns": False},
    "toolCalls": {"supported": True, "requiresStrictPairing": False},
}


def _default_tokenizer(text: str) -> int:
    return (len(text) + 3) // 4


def _render_context_to_text(context: RenderedContext, tools: Sequence[ToolSchema]) -> str:
    parts: list[str] = []
    sys_text = getattr(context, "systemText", None) or context.get("systemText") if isinstance(context, dict) else getattr(context, "systemText", None)
    # Be tolerant of either dict-like RenderedContext or dataclass — use getattr / dict access.
    def _get(key: str) -> Any:
        if isinstance(context, dict):
            return context.get(key)
        return getattr(context, key, None)

    for key in ("systemText", "systemStable", "systemKnowledge"):
        v = _get(key)
        if v:
            parts.append(str(v))
    state_turn = _get("stateTurn")
    if state_turn:
        content = state_turn.get("content") if isinstance(state_turn, dict) else getattr(state_turn, "content", None)
        if content:
            parts.append(str(content))
    turns = _get("turns") or []
    for turn in turns:
        content = turn.get("content") if isinstance(turn, dict) else getattr(turn, "content", None)
        if content:
            parts.append(str(content))
        content_parts = turn.get("contentParts") if isinstance(turn, dict) else getattr(turn, "contentParts", None)
        for p in content_parts or []:
            for f in ("output", "text"):
                v = p.get(f) if isinstance(p, dict) else getattr(p, f, None)
                if isinstance(v, str):
                    parts.append(v)
                    break
        tool_calls = turn.get("toolCalls") if isinstance(turn, dict) else getattr(turn, "toolCalls", None)
        for tc in tool_calls or []:
            name = tc.get("name") if isinstance(tc, dict) else getattr(tc, "name", "")
            args = tc.get("arguments") if isinstance(tc, dict) else getattr(tc, "arguments", "")
            parts.append(f"{name} {args}")
    for t in tools or []:
        name = t.get("name") if isinstance(t, dict) else getattr(t, "name", "")
        desc = t.get("description") if isinstance(t, dict) else getattr(t, "description", "")
        params = t.get("parameters") if isinstance(t, dict) else getattr(t, "parameters", "")
        parts.append(f"{name} {desc} {params}")
    return "\n".join(parts)


class ReplayProvider(LLMProvider):
    """LLMProvider that dequeues recorded assistant messages instead of calling an API."""

    def __init__(self, messages: Sequence[Message], opts: ReplayProviderOpts | None = None) -> None:
        self._messages: tuple[Message, ...] = tuple(messages)
        opts = opts or ReplayProviderOpts()
        self._tokenizer = opts.tokenizer or _default_tokenizer
        self._descriptor = opts.descriptor or _DEFAULT_DESCRIPTOR
        self._wrap = bool(opts.wrap)
        self._cursor = 0

    def descriptor(self) -> ProviderDescriptor:
        return self._descriptor

    def consumed(self) -> int:
        return self._cursor

    def remaining(self) -> int:
        return max(0, len(self._messages) - self._cursor)

    def reset(self) -> None:
        self._cursor = 0

    def _pull(self) -> Message:
        if self._cursor >= len(self._messages):
            if self._wrap and self._messages:
                self._cursor = 0
            else:
                raise RuntimeError(
                    f"ReplayProvider: fixture exhausted (consumed={self._cursor}, total={len(self._messages)})"
                )
        msg = self._messages[self._cursor]
        self._cursor += 1
        return msg

    async def complete(
        self,
        context: RenderedContext,
        tools: list[ToolSchema],
        extensions: dict | None = None,
    ) -> Message:
        msg = self._pull()
        out: dict[str, Any] = {"role": "assistant", "content": getattr(msg, "content", "") or ""}
        tool_calls = getattr(msg, "toolCalls", None) or getattr(msg, "tool_calls", None)
        if tool_calls:
            out["toolCalls"] = tool_calls
        token_count = getattr(msg, "tokenCount", None) or getattr(msg, "token_count", None)
        if token_count is not None:
            out["tokenCount"] = token_count
        return out  # type: ignore[return-value]

    async def stream(  # type: ignore[override]
        self,
        context: RenderedContext,
        tools: list[ToolSchema],
        extensions: dict | None = None,
        state: ProviderRunState | None = None,
    ) -> AsyncIterator[StreamEvent]:
        msg = self._pull()
        content = getattr(msg, "content", None) or (msg.get("content") if isinstance(msg, dict) else "") or ""
        token_count = getattr(msg, "tokenCount", None) or (msg.get("tokenCount") if isinstance(msg, dict) else None)
        input_tokens = self._tokenizer(_render_context_to_text(context, tools))
        output_tokens = token_count if token_count is not None else self._tokenizer(content)

        yield {  # type: ignore[misc]
            "type": "usage",
            "totalTokens": input_tokens + output_tokens,
            "inputTokens": input_tokens,
            "outputTokens": output_tokens,
            "cacheReadInputTokens": 0,
            "cacheCreationInputTokens": 0,
        }
        if content:
            yield {"type": "text_delta", "delta": content}  # type: ignore[misc]

        tool_calls = getattr(msg, "toolCalls", None) or (msg.get("toolCalls") if isinstance(msg, dict) else None) or []
        import json

        for tc in tool_calls:
            name = tc.get("name") if isinstance(tc, dict) else getattr(tc, "name", "")
            tc_id = tc.get("id") if isinstance(tc, dict) else getattr(tc, "id", "")
            raw_args = tc.get("arguments") if isinstance(tc, dict) else getattr(tc, "arguments", "{}")
            try:
                args = json.loads(raw_args) if isinstance(raw_args, str) else (raw_args or {})
            except Exception:
                args = {}
            yield {"type": "tool_call", "id": tc_id, "name": name, "arguments": args}  # type: ignore[misc]
