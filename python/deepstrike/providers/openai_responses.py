"""OpenAI Responses API provider with ``previous_response_id`` continuation (prefix-cache G1).

Mirror of the Node ``OpenAIResponsesProvider``. The Responses API is *stateful*: the first turn
returns a ``response.id``; passing it back as ``previous_response_id`` keeps the whole prefix on
OpenAI's servers, so each later turn sends only the **new** tail (uncovered turns + the volatile
State turn) instead of replaying the full history. This is opt-in — instantiate this provider
explicitly for the ``openai-responses`` protocol. On a missing/expired chain it degrades to a full
resend (the stateless default), so ``snapshot``/``resume`` stay correct.

The continuation state lives in the provider-owned, opaque :data:`ProviderRunState` the runner
threads across turns — keys ``previous_response_id`` and ``covered_message_count``.
"""
from __future__ import annotations
import json
import logging
from typing import Any, AsyncIterator
from openai import AsyncOpenAI
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent, UsageEvent
from .base import (
    RetryConfig,
    CircuitBreaker,
    ProviderDescriptor,
    ProviderRunState,
    RenderedContext,
    RuntimePolicy,
    normalize_tool_call,
    wire_request_extensions,
)

logger = logging.getLogger(__name__)

_OPENAI_RESPONSES_POLICIES: dict[str, RuntimePolicy] = {
    "gpt-5.5":      RuntimePolicy(max_turns=60),
    "gpt-5.4":      RuntimePolicy(max_turns=50),
    "gpt-5.4-mini": RuntimePolicy(max_turns=25),
    "gpt-5.4-nano": RuntimePolicy(max_turns=15),
    "gpt-5.2":      RuntimePolicy(max_turns=50),
    "gpt-5.2-pro":  RuntimePolicy(max_turns=60),
    "gpt-5.1":      RuntimePolicy(max_turns=50),
    "gpt-4.1":      RuntimePolicy(max_turns=35),
    "gpt-4.1-mini": RuntimePolicy(max_turns=20),
    "gpt-4.1-nano": RuntimePolicy(max_turns=15),
    "gpt-5":        RuntimePolicy(max_turns=50),
    "gpt-5-pro":    RuntimePolicy(max_turns=60),
    "gpt-5-mini":   RuntimePolicy(max_turns=25),
    "gpt-5-nano":   RuntimePolicy(max_turns=15),
    "o3":           RuntimePolicy(max_turns=50),
    "o3-mini":      RuntimePolicy(max_turns=25),
    "o4-mini":      RuntimePolicy(max_turns=25),
}


def _message_content(message: Message) -> Any:
    """Responses-native content for a message: a plain string when it has no parts, else a list of
    ``input_text``/``input_image`` blocks (mirrors the Node adapter)."""
    parts = getattr(message, "content_parts", None)
    if not parts:
        return message.content
    content: list[dict] = []
    for part in parts:
        if part.type == "text":
            content.append({"type": "input_text", "text": part.text})
        elif part.type == "image":
            if part.data and part.media_type:
                image_url = f"data:{part.media_type};base64,{part.data}"
            else:
                image_url = part.url
            if image_url:
                content.append({
                    "type": "input_image",
                    "detail": part.detail or "auto",
                    "image_url": image_url,
                })
    return content


class OpenAIResponsesAdapter:
    def build_tools(self, tools: list[ToolSchema]) -> list[dict]:
        return [
            {
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": json.loads(t.parameters),
            }
            for t in tools
        ]

    def build_instructions(self, context: RenderedContext) -> str | None:
        return context.system_text or None

    def build_input(self, context: RenderedContext, state: ProviderRunState | None = None) -> list[dict]:
        """The Responses ``input`` array. When continuing from a previous response, only the
        uncovered tail (turns past ``covered_message_count``) is serialized — the covered prefix
        already lives server-side under ``previous_response_id``. The volatile State turn is always
        appended (it changes every call and is never "covered")."""
        input_items: list[dict] = []
        turns = context.turns
        if state and state.get("previous_response_id"):
            uncovered = turns[int(state.get("covered_message_count", 0)):]
        else:
            uncovered = turns

        for message in uncovered:
            self._append_message(input_items, message)

        state_turn = getattr(context, "state_turn", None)
        if state_turn is not None:
            self._append_message(input_items, state_turn)

        return input_items

    def _append_message(self, input_items: list[dict], message: Message) -> None:
        if message.role == "assistant" and getattr(message, "tool_calls", None):
            if message.content or getattr(message, "content_parts", None):
                input_items.append({"role": "assistant", "content": _message_content(message)})
            for tc in message.tool_calls:
                input_items.append({
                    "type": "function_call",
                    "call_id": tc.id,
                    "name": tc.name,
                    "arguments": tc.arguments,
                })
            return

        if message.role == "tool":
            for part in (getattr(message, "content_parts", None) or []):
                if part.type != "tool_result":
                    continue
                input_items.append({
                    "type": "function_call_output",
                    "call_id": part.call_id,
                    "output": part.output,
                })
            return

        input_items.append({"role": message.role, "content": _message_content(message)})

    def decode_output(self, output: list[dict]) -> dict:
        content = ""
        tool_calls: list[ToolCall] = []
        for item in output:
            itype = item.get("type") if isinstance(item, dict) else getattr(item, "type", None)
            if itype == "message":
                parts = (item.get("content") if isinstance(item, dict) else getattr(item, "content", None)) or []
                for part in parts:
                    ptype = part.get("type") if isinstance(part, dict) else getattr(part, "type", None)
                    if ptype == "output_text":
                        content += str((part.get("text") if isinstance(part, dict) else getattr(part, "text", "")) or "")
            elif itype == "function_call":
                call_id = item.get("call_id") if isinstance(item, dict) else getattr(item, "call_id", None)
                name = item.get("name") if isinstance(item, dict) else getattr(item, "name", None)
                arguments = item.get("arguments") if isinstance(item, dict) else getattr(item, "arguments", None)
                tc = normalize_tool_call(call_id or "", name or "", arguments or "{}")
                if tc:
                    tool_calls.append(tc)
        return {"content": content, "tool_calls": tool_calls}


class OpenAIResponsesProvider:
    """OpenAI Responses API provider. Opt-in stateful continuation via ``previous_response_id``."""

    def __init__(
        self,
        api_key: str,
        model: str = "gpt-4.1",
        retry_config: RetryConfig | None = None,
        base_url: str = "https://api.openai.com/v1",
    ):
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        self._base_url = base_url.rstrip("/")
        self._client = AsyncOpenAI(api_key=api_key, base_url=base_url)
        self._responses = OpenAIResponsesAdapter()

    def runtime_policy(self) -> RuntimePolicy:
        return _OPENAI_RESPONSES_POLICIES.get(self._model, RuntimePolicy())

    def descriptor(self) -> ProviderDescriptor:
        return ProviderDescriptor(
            provider="openai",
            protocol="openai-responses",
            model=self._model,
            reasoning={"supported": True, "preserve_across_tool_turns": False},
            tool_calls={"supported": True, "requires_strict_pairing": True},
        )

    def create_run_state(self) -> ProviderRunState:
        return {"covered_message_count": 0}

    def _as_run_state(self, state: ProviderRunState | None) -> ProviderRunState:
        if state is None:
            return self.create_run_state()
        if not isinstance(state.get("covered_message_count"), int):
            state["covered_message_count"] = 0
        return state

    def _request_extensions(self, extensions: dict | None) -> dict:
        return wire_request_extensions(
            extensions,
            extra_omit=("input", "instructions", "previous_response_id"),
        )

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        last_exc: Exception | None = None
        for attempt in range(self._retry.max_retries):
            try:
                instructions = self._responses.build_instructions(context)
                req: dict[str, Any] = {
                    **self._request_extensions(extensions),
                    "model": self._model,
                    "input": self._responses.build_input(context),
                }
                if instructions:
                    req["instructions"] = instructions
                if tools:
                    req["tools"] = self._responses.build_tools(tools)
                resp = await self._client.responses.create(**req)
                self._circuit.record_success()
                output = getattr(resp, "output", None) or []
                decoded = self._responses.decode_output(
                    [o if isinstance(o, dict) else o.model_dump() for o in output]
                )
                usage = getattr(resp, "usage", None)
                token_count = None
                if usage is not None:
                    token_count = getattr(usage, "output_tokens", None) or getattr(usage, "total_tokens", None)
                return Message(
                    role="assistant",
                    content=decoded["content"],
                    tool_calls=decoded["tool_calls"] or None,
                    token_count=token_count,
                )
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    import asyncio
                    await asyncio.sleep(self._retry.base_delay * (2 ** attempt))

        raise last_exc or RuntimeError("Complete failed")

    async def stream(
        self,
        context: RenderedContext,
        tools: list[ToolSchema],
        extensions: dict | None = None,
        state: ProviderRunState | None = None,
    ) -> AsyncIterator[StreamEvent]:
        run_state = self._as_run_state(state)
        instructions = self._responses.build_instructions(context)
        function_calls: dict[int, dict] = {}

        req: dict[str, Any] = {
            **self._request_extensions(extensions),
            "model": self._model,
            "input": self._responses.build_input(context, run_state),
            "stream": True,
        }
        if instructions:
            req["instructions"] = instructions
        if run_state.get("previous_response_id"):
            req["previous_response_id"] = run_state["previous_response_id"]
        if tools:
            req["tools"] = self._responses.build_tools(tools)

        stream = await self._client.responses.create(**req)

        async for evt in stream:
            etype = getattr(evt, "type", None)
            if etype == "response.output_text.delta":
                yield TextDelta(delta=getattr(evt, "delta", "") or "")
            elif etype == "response.output_item.added" and getattr(getattr(evt, "item", None), "type", None) == "function_call":
                item = evt.item
                function_calls[evt.output_index] = {
                    "id": item.call_id,
                    "name": item.name,
                    "args_buf": getattr(item, "arguments", "") or "",
                }
            elif etype == "response.function_call_arguments.delta":
                call = function_calls.get(evt.output_index)
                if call:
                    call["args_buf"] += getattr(evt, "delta", "") or ""
            elif etype == "response.function_call_arguments.done":
                call = function_calls.get(evt.output_index)
                if call:
                    call["args_buf"] = getattr(evt, "arguments", "") or call["args_buf"]
            elif etype == "response.output_item.done" and getattr(getattr(evt, "item", None), "type", None) == "function_call":
                item = evt.item
                call = function_calls.get(evt.output_index) or {
                    "id": item.call_id,
                    "name": item.name,
                    "args_buf": getattr(item, "arguments", "") or "{}",
                }
                try:
                    args = json.loads(call["args_buf"] or "{}")
                except json.JSONDecodeError:
                    args = {}
                yield ToolCallEvent(id=call["id"], name=call["name"], arguments=args)
            elif etype == "response.completed":
                response = evt.response
                run_state["previous_response_id"] = response.id
                run_state["covered_message_count"] = len(context.turns) + 1
                usage = getattr(response, "usage", None)
                total = getattr(usage, "total_tokens", 0) if usage else 0
                if total:
                    details = getattr(usage, "input_tokens_details", None)
                    cached = getattr(details, "cached_tokens", 0) if details else 0
                    yield UsageEvent(
                        total_tokens=total,
                        input_tokens=getattr(usage, "input_tokens", 0) or 0,
                        output_tokens=getattr(usage, "output_tokens", 0) or 0,
                        cache_read_input_tokens=int(cached or 0),
                    )
