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
from dataclasses import dataclass, field
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
    UnsupportedModalityError,
    normalize_tool_call,
    wire_request_extensions,
)
from .stop_reason import canonicalize_stop_reason
from .usage import normalize_usage
from .protocol_adapter import AdapterOutput, ProtocolResponseError
from deepstrike.types.content import CanonicalAdapterInput, normalize_canonical_adapter_input

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


@dataclass(frozen=True)
class OpenAIResponsesRequestPlan:
    params: dict[str, Any]


@dataclass
class OpenAIResponsesStreamState:
    input: Any
    function_calls: dict[int, dict] = field(default_factory=dict)


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
            # Default the MIME type (like every other serializer) so a data-only image is
            # not silently dropped; only a part with neither url nor data yields None.
            if part.data:
                image_url = f"data:{part.media_type or 'image/png'};base64,{part.data}"
            else:
                image_url = part.url
            if image_url:
                content.append({
                    "type": "input_image",
                    "detail": part.detail or "auto",
                    "image_url": image_url,
                })
        elif part.type == "file":
            content.append({"type": "input_file", "file_id": part.file_id})
        elif part.type == "audio":
            raise UnsupportedModalityError("audio", "openai-responses")
    return content


class OpenAIResponsesAdapter:
    protocol = "openai-responses"

    def __init__(self, model: str = "gpt-4.1") -> None:
        self._model = model

    @staticmethod
    def _get(raw: Any, name: str) -> Any:
        return raw.get(name) if isinstance(raw, dict) else getattr(raw, name, None)

    @classmethod
    def _number(cls, raw: Any, field: str) -> int | None:
        value = cls._get(raw, field)
        if value is None:
            return None
        if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
            raise ProtocolResponseError("openai-responses", f"usage.{field} must be a non-negative number")
        return int(value)

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

    def build_input(
        self,
        context: RenderedContext,
        state: ProviderRunState | None = None,
        resolved=None,
    ) -> list[dict]:
        """The Responses ``input`` array. When continuing from a previous response, only the
        uncovered tail (turns past ``covered_message_count``) is serialized — the covered prefix
        already lives server-side under ``previous_response_id``. The volatile State turn is always
        appended (it changes every call and is never "covered")."""
        normalize_canonical_adapter_input(context, [], resolved=resolved)
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

    def _builtin_tools(self, extensions: dict[str, Any]) -> list[dict]:
        tools: list[dict] = []
        web_search = extensions.get("web_search")
        if web_search:
            tools.append({"type": "web_search", **web_search} if isinstance(web_search, dict) else {"type": "web_search"})
        builtin = extensions.get("builtin_tools")
        if isinstance(builtin, list):
            tools.extend(builtin)
        return tools

    def build_request(
        self,
        input: CanonicalAdapterInput,
        state: ProviderRunState | None = None,
    ) -> OpenAIResponsesRequestPlan:
        extensions = input.extensions
        params = {
            **wire_request_extensions(
                extensions,
                extra_omit=("input", "instructions", "previous_response_id", "web_search", "builtin_tools"),
            ),
            "model": input.resolved.model_id if input.resolved is not None else self._model,
            "input": self.build_input(input.context, state, input.resolved),
        }
        instructions = self.build_instructions(input.context)
        if instructions:
            params["instructions"] = instructions
        if state and state.get("previous_response_id"):
            params["previous_response_id"] = state["previous_response_id"]
        tools = [*self.build_tools(list(input.tools)), *self._builtin_tools(extensions)]
        if tools:
            params["tools"] = tools
        return OpenAIResponsesRequestPlan(params=params)

    def normalize_usage(self, raw: Any):
        if raw is None:
            return None
        if not isinstance(raw, dict) and not hasattr(raw, "input_tokens"):
            raise ProtocolResponseError("openai-responses", "usage must be an object")
        self._number(raw, "input_tokens")
        self._number(raw, "output_tokens")
        self._number(raw, "total_tokens")
        input_details = self._get(raw, "input_tokens_details")
        if input_details is not None:
            if not isinstance(input_details, dict) and not hasattr(input_details, "cached_tokens"):
                raise ProtocolResponseError("openai-responses", "usage.input_tokens_details must be an object")
            self._number(input_details, "cached_tokens")
        output_details = self._get(raw, "output_tokens_details")
        if output_details is not None:
            if not isinstance(output_details, dict) and not hasattr(output_details, "reasoning_tokens"):
                raise ProtocolResponseError("openai-responses", "usage.output_tokens_details must be an object")
            self._number(output_details, "reasoning_tokens")
        return normalize_usage(raw)

    def decode_complete(self, raw: Any, input: CanonicalAdapterInput) -> Message:
        output = self._get(raw, "output") or []
        decoded = self.decode_output([
            item if isinstance(item, dict) else item.model_dump() if hasattr(item, "model_dump") else item
            for item in output
        ])
        usage = self._get(raw, "usage")
        self.normalize_usage(usage)
        token_count = self._number(usage, "output_tokens") if usage is not None else None
        if token_count is None and usage is not None:
            token_count = self._number(usage, "total_tokens")
        return Message(role="assistant", content=decoded["content"], tool_calls=decoded["tool_calls"] or None, token_count=token_count)

    def create_stream_state(
        self,
        input: CanonicalAdapterInput,
        state: ProviderRunState | None = None,
    ) -> OpenAIResponsesStreamState:
        return OpenAIResponsesStreamState(input=input)

    def push_stream_chunk(self, chunk: Any, state: OpenAIResponsesStreamState) -> AdapterOutput:
        kind = self._get(chunk, "type")
        events: list[StreamEvent] = []
        if kind == "response.output_text.delta":
            events.append(TextDelta(delta=self._get(chunk, "delta") or ""))
        elif kind == "response.output_item.added":
            item = self._get(chunk, "item")
            if self._get(item, "type") == "function_call":
                state.function_calls[int(self._get(chunk, "output_index"))] = {
                    "id": self._get(item, "call_id"),
                    "name": self._get(item, "name"),
                    "args_buf": self._get(item, "arguments") or "",
                }
        elif kind == "response.function_call_arguments.delta":
            call = state.function_calls.get(int(self._get(chunk, "output_index")))
            if call:
                call["args_buf"] += self._get(chunk, "delta") or ""
        elif kind == "response.function_call_arguments.done":
            call = state.function_calls.get(int(self._get(chunk, "output_index")))
            if call:
                call["args_buf"] = self._get(chunk, "arguments") or call["args_buf"]
        elif kind == "response.output_item.done":
            item = self._get(chunk, "item")
            if self._get(item, "type") == "function_call":
                call = state.function_calls.get(int(self._get(chunk, "output_index"))) or {
                    "id": self._get(item, "call_id"),
                    "name": self._get(item, "name"),
                    "args_buf": self._get(item, "arguments") or "{}",
                }
                try:
                    args = json.loads(call["args_buf"] or "{}")
                except json.JSONDecodeError:
                    args = {}
                events.append(ToolCallEvent(id=call["id"], name=call["name"], arguments=args))
        elif kind in {"response.completed", "response.incomplete"}:
            response = self._get(chunk, "response")
            response_id = self._get(response, "id")
            patch = {"covered_message_count": len(state.input.context.turns) + 1}
            if response_id:
                patch["previous_response_id"] = response_id
            usage = self._get(response, "usage")
            provider_usage = self.normalize_usage(usage)
            total = self._number(usage, "total_tokens") if usage is not None else None
            if total:
                details = self._get(usage, "input_tokens_details")
                incomplete_details = self._get(response, "incomplete_details")
                raw_stop_reason = self._get(incomplete_details, "reason") if incomplete_details is not None else None
                events.append(UsageEvent(
                    total_tokens=total,
                    input_tokens=self._number(usage, "input_tokens") or 0,
                    output_tokens=self._number(usage, "output_tokens") or 0,
                    cache_read_input_tokens=self._number(details, "cached_tokens") if details is not None else 0,
                    stop_reason=canonicalize_stop_reason(raw_stop_reason),
                    raw_stop_reason=raw_stop_reason,
                    provider_usage=provider_usage,
                ))
            return AdapterOutput(events=events, run_state_patch=patch)
        return AdapterOutput(events=events)

    def finish_stream(self, state: OpenAIResponsesStreamState, final: Any = None) -> AdapterOutput:
        return self.push_stream_chunk(final, state) if final is not None else AdapterOutput()


class OpenAIResponsesProvider:
    """OpenAI Responses API provider. Opt-in stateful continuation via ``previous_response_id``."""

    def __init__(
        self,
        api_key: str,
        model: str = "gpt-4.1",
        retry_config: RetryConfig | None = None,
        base_url: str = "https://api.openai.com/v1",
        auth_mode: str = "api_key",
    ):
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        self._base_url = base_url.rstrip("/")
        client_kwargs: dict[str, Any] = {"api_key": api_key, "base_url": base_url}
        if auth_mode == "bearer":
            client_kwargs["default_headers"] = {"Authorization": f"Bearer {api_key}"}
        self._client = AsyncOpenAI(**client_kwargs)
        self._responses = OpenAIResponsesAdapter(model)

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
            extra_omit=("input", "instructions", "previous_response_id", "web_search", "builtin_tools"),
        )

    def _builtin_tools(self, extensions: dict | None) -> list[dict]:
        return self._responses._builtin_tools(extensions or {})

    def _all_tools(self, tools: list[ToolSchema], extensions: dict | None) -> list[dict]:
        defs = list(self._responses.build_tools(tools)) if tools else []
        defs.extend(self._builtin_tools(extensions))
        return defs

    def _canonical_input(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None):
        return normalize_canonical_adapter_input(
            context,
            tools,
            extensions=extensions,
            resolved=getattr(self, "_resolved_runtime", None),
        )

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        adapter_input = self._canonical_input(context, tools, extensions)
        plan = self._responses.build_request(adapter_input)
        last_exc: Exception | None = None
        for attempt in range(self._retry.max_retries):
            try:
                resp = await self._client.responses.create(**plan.params)
                self._circuit.record_success()
                return self._responses.decode_complete(resp, adapter_input)
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
        adapter_input = self._canonical_input(context, tools, extensions)
        plan = self._responses.build_request(adapter_input, run_state)
        stream = await self._client.responses.create(**{**plan.params, "stream": True})
        stream_state = self._responses.create_stream_state(adapter_input, run_state)

        async for evt in stream:
            output = self._responses.push_stream_chunk(evt, stream_state)
            if output.run_state_patch:
                run_state.update(output.run_state_patch)
            for event in output.events:
                yield event
        output = self._responses.finish_stream(stream_state)
        if output.run_state_patch:
            run_state.update(output.run_state_patch)
        for event in output.events:
            yield event
