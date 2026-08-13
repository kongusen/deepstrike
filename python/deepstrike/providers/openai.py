from __future__ import annotations
import json
import logging
from dataclasses import dataclass, field
from typing import Any, AsyncIterator
import httpx
from openai import AsyncOpenAI
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent, ThinkingDelta, UsageEvent
from .base import RetryConfig, CircuitBreaker, ProviderDescriptor, RenderedContext, RuntimePolicy, normalize_tool_call, openai_cached_prompt_tokens, stable_prompt_cache_key, to_openai_message_params, ThinkingTagStreamExtractor, wire_request_extensions
from .replay import ReasoningReplayMixin, assistant_replay_key
from .replay_validator import validate_openai_chat_replay
from .stop_reason import canonicalize_stop_reason
from .usage import normalize_usage
from .openai_chat_adapter import OpenAIChatAdapter
from deepstrike.types.content import normalize_canonical_adapter_input

logger = logging.getLogger(__name__)

_OPENAI_POLICIES: dict[str, RuntimePolicy] = {
    "gpt-5.5":      RuntimePolicy(max_turns=60),
    "gpt-5.4":      RuntimePolicy(max_turns=50),
    "gpt-5.4-mini": RuntimePolicy(max_turns=25),
    "gpt-5.4-nano": RuntimePolicy(max_turns=15),
    "gpt-5.2":      RuntimePolicy(max_turns=50),
    "gpt-5.2-pro":  RuntimePolicy(max_turns=60),
    "gpt-5.1":      RuntimePolicy(max_turns=50),
    "gpt-4o":       RuntimePolicy(max_turns=25),
    "gpt-4o-mini":  RuntimePolicy(max_turns=15),
    "gpt-4.1":      RuntimePolicy(max_turns=35),
    "gpt-4.1-mini": RuntimePolicy(max_turns=20),
    "gpt-4.1-nano": RuntimePolicy(max_turns=15),
    "gpt-5":        RuntimePolicy(max_turns=50),
    "gpt-5-pro":    RuntimePolicy(max_turns=60),
    "gpt-5-mini":   RuntimePolicy(max_turns=25),
    "gpt-5-nano":   RuntimePolicy(max_turns=15),
    "o1":           RuntimePolicy(max_turns=50),
    "o1-mini":      RuntimePolicy(max_turns=25),
    "o3":           RuntimePolicy(max_turns=50),
    "o3-mini":      RuntimePolicy(max_turns=25),
    "o4-mini":      RuntimePolicy(max_turns=25),
}


@dataclass
class OpenAIChatTurnReasoning:
    """Reasoning captured from one model turn, handed to the replay-remember hooks so a reasoning
    vendor (DeepSeek/MiniMax) can persist whatever replay envelope its wire requires."""
    reasoning_content: str = ""
    reasoning_details: Any = None
    native_tool_calls: list = field(default_factory=list)


def _extra_field(obj: object, name: str):
    """Read a (possibly non-standard) field off an openai SDK object — direct attr first, then the
    ``model_extra`` bag where vendor extensions like reasoning_content / reasoning_details land."""
    value = getattr(obj, name, None)
    if value is not None:
        return value
    extra = getattr(obj, "model_extra", None)
    if isinstance(extra, dict):
        return extra.get(name)
    return None


def _native_tool_calls_from_bufs(bufs: dict[int, dict]) -> list[dict]:
    return [
        {"id": tb["id"], "type": "function", "function": {"name": tb["name"], "arguments": tb["args_buf"] or "{}"}}
        for tb in bufs.values()
    ]


class OpenAIProvider(ReasoningReplayMixin):
    def __init__(
        self,
        api_key: str,
        model: str = "gpt-4o",
        retry_config: RetryConfig | None = None,
        base_url: str = "https://api.openai.com/v1",
        dialect: Any | None = None,
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
        self._wire_dialect = dialect
        self._adapter = OpenAIChatAdapter(model)
        self._init_replay_store()

    def runtime_policy(self) -> RuntimePolicy:
        if self._wire_dialect is not None:
            from .model_registry import get_runtime_policy
            return get_runtime_policy(self._wire_dialect.provider_id, self._model)
        return _OPENAI_POLICIES.get(self._model, RuntimePolicy())

    def descriptor(self) -> ProviderDescriptor:
        if self._wire_dialect is not None:
            reasoning = {
                "supported": True,
                "preserve_across_tool_turns": self._wire_dialect.reasoning_preserve_across_tool_turns,
            }
            if self._wire_dialect.reasoning_requires_replay_for_tool_turns:
                reasoning["requires_replay_for_tool_turns"] = True
            return ProviderDescriptor(
                provider=self._wire_dialect.provider_id,
                protocol="openai-chat",
                model=self._model,
                reasoning=reasoning,
                tool_calls={"supported": True, "requires_strict_pairing": True},
            )
        return ProviderDescriptor(
            provider="openai",
            protocol="openai-chat",
            model=self._model,
            reasoning={"supported": True, "preserve_across_tool_turns": False},
            tool_calls={"supported": True, "requires_strict_pairing": True},
        )

    def _require_non_empty_reasoning_replay_for_tool_turns(self, extensions: dict | None) -> bool:
        if self._wire_dialect is not None:
            return (
                self._model in self._wire_dialect.reasoning_model_ids
                and bool(self._wire_dialect.require_reasoning_replay((extensions or {})))
            )
        return False

    def _degrade_missing_reasoning_replay(self, extensions: dict | None) -> bool:
        return (extensions or {}).get("degrade_missing_reasoning_replay") is True

    # ── Template-Method hooks ───────────────────────────────────────────────
    # Defaults reproduce the plain OpenAI-chat behavior; reasoning vendors
    # (DeepSeek/MiniMax) override these instead of duplicating complete()/stream().

    def _prepare_extensions(self, extensions: dict | None) -> dict | None:
        """Pre-process caller extensions before they reach the wire request body (e.g. force
        ``reasoning_split``). Default: pass through unchanged."""
        if self._wire_dialect is not None and self._wire_dialect.prepare_extensions is not None:
            return self._wire_dialect.prepare_extensions(extensions or {})
        return extensions

    def _cache_key_params(self, context: RenderedContext, tools: list[ToolSchema]) -> dict:
        """Request-body params controlling prompt caching, merged via setdefault. Default sends
        OpenAI's ``prompt_cache_key``; vendors whose endpoints reject unknown params (DeepSeek 400s)
        override to ``{}``."""
        if self._wire_dialect is not None and self._wire_dialect.cache_key_mode == "none":
            return {}
        return {"prompt_cache_key": self._prompt_cache_key(context, tools)}

    def _wire_tools(self, tools: list[ToolSchema], extensions: dict | None = None) -> list[dict] | None:
        """Tool defs sent on the wire. Default = the caller's function tools; DeepSeek reasoner models
        strip them, GLM appends its server-side web_search tool when enabled via extensions."""
        defs = list(self._build_tools(tools) or [])
        if self._wire_dialect is not None:
            if self._model in self._wire_dialect.tool_unsupported_model_ids:
                return None
            if self._wire_dialect.server_tools is not None:
                defs.extend(self._wire_dialect.server_tools(extensions or {}))
        return defs or None

    def _uses_inline_thinking_tags(self) -> bool:
        """Whether streamed ``content`` may carry inline <thinking>…</thinking> tags to split out.
        Default True (OpenAI); reasoning vendors emit reasoning out-of-band, so they return False."""
        return self._wire_dialect.inline_thinking_tags if self._wire_dialect is not None else True

    def _expose_reasoning_delta(self, extensions: dict | None) -> bool:
        """Whether to surface streamed ``reasoning_content`` as ThinkingDelta events. Default True;
        vendors gate this behind an ``expose_reasoning`` extension."""
        if self._wire_dialect is not None and self._wire_dialect.expose_reasoning is not None:
            return bool(self._wire_dialect.expose_reasoning(extensions or {}))
        return True

    def _capture_reasoning_details(self) -> bool:
        """Whether to accumulate ``reasoning_details`` (split reasoning) for replay. Default False;
        MiniMax returns True."""
        return bool(self._wire_dialect and self._wire_dialect.capture_reasoning_details)

    def _remember_complete_replay(self, content: str, tool_calls: list, reasoning: OpenAIChatTurnReasoning) -> None:
        if self._wire_dialect is not None:
            self._remember_dialect_replay(content, tool_calls, reasoning, phase="complete")

    def _remember_stream_replay(self, content: str, tool_calls: list, reasoning: OpenAIChatTurnReasoning) -> None:
        """Persist replay after a streamed turn using the endpoint dialect when one is bound."""
        if self._wire_dialect is not None:
            self._remember_dialect_replay(content, tool_calls, reasoning, phase="stream")
        else:
            self.remember_reasoning_for_turn(content, tool_calls, reasoning.reasoning_content)

    def _remember_dialect_replay(
        self,
        content: str,
        tool_calls: list,
        reasoning: OpenAIChatTurnReasoning,
        *,
        phase: str,
    ) -> None:
        dialect = self._wire_dialect
        if dialect.replay_strategy == "generic_stream":
            if phase == "stream" and (tool_calls or reasoning.reasoning_content):
                self.remember_reasoning_for_turn(content, tool_calls, reasoning.reasoning_content)
            return
        if dialect.replay_strategy == "deepseek":
            if not reasoning.reasoning_content.strip():
                return
            envelope: dict[str, Any] = {
                "provider": dialect.provider_id, "protocol": "openai-chat",
                "model": self._model, "reasoning_content": reasoning.reasoning_content,
            }
        elif dialect.replay_strategy == "minimax":
            if not reasoning.reasoning_content.strip() and reasoning.reasoning_details is None:
                return
            envelope = {"provider": dialect.provider_id, "protocol": "openai-chat", "model": self._model}
            if reasoning.reasoning_content.strip():
                envelope["reasoning_content"] = reasoning.reasoning_content
            if reasoning.reasoning_details is not None:
                envelope["reasoning_details"] = reasoning.reasoning_details
        else:
            return
        if reasoning.native_tool_calls:
            envelope["tool_calls"] = reasoning.native_tool_calls
        self.remember_replay_fields(Message(role="assistant", content=content, tool_calls=tool_calls or None), envelope)

    def assess_replayability(self, context: RenderedContext, extensions: dict | None = None) -> dict:
        """Pre-flight query: would this history validate against this provider with
        the given extensions, without sending the request? Returns the tool-call
        ids whose turn lacks the reasoning replay this provider requires, so an
        embedder can route around the failure before issuing the request. Seed any
        persisted replay first. ``ok`` is True when no reasoning replay is required."""
        if not self._require_non_empty_reasoning_replay_for_tool_turns(extensions):
            return {"ok": True, "offending_call_ids": []}
        return self.assess_reasoning(context)

    def _build_messages(self, context: RenderedContext, extensions: dict | None = None) -> list[dict]:
        require_reasoning = self._require_non_empty_reasoning_replay_for_tool_turns(extensions)
        degrade = self._degrade_missing_reasoning_replay(extensions)
        validate_openai_chat_replay(
            context.turns,
            descriptor=self.descriptor(),
            require_non_empty_reasoning_for_tool_calls=require_reasoning,
            degrade_missing_reasoning=degrade,
            replay_for_assistant=lambda content, tool_calls: self._replay_fields.get(
                assistant_replay_key(content, tool_calls)
            ),
        )
        serialized = to_openai_message_params(context, getattr(self, "_resolved_runtime", None))
        messages = self._merge_replay_into_openai_messages(
            serialized, context, degrade_missing_reasoning=require_reasoning and degrade,
        )
        if self._wire_dialect is not None and self._wire_dialect.supports_context_cache:
            cache_message = self._context_cache_message(extensions)
            if cache_message is not None:
                return [cache_message, *messages]
        return messages

    def _context_cache_message(self, extensions: dict | None) -> dict | None:
        ext = extensions or {}
        cache_id = ext.get("context_cache_id")
        cache_tag = ext.get("context_cache_tag")
        if cache_id:
            reference = f"cache_id={cache_id}"
        elif cache_tag:
            reference = f"tag={cache_tag}"
        else:
            return None
        if ext.get("context_cache_reset_ttl") is not None:
            reference += f";reset_ttl={int(ext['context_cache_reset_ttl'])}"
        return {"role": "cache", "content": reference}

    def _cache_model_family(self) -> str:
        return "moonshot-v1" if self._model.startswith("moonshot-v1") else self._model

    async def create_context_cache(
        self,
        messages: list[dict],
        *,
        model: str | None = None,
        tools: list[dict] | None = None,
        ttl: int | None = 3600,
        expired_at: int | None = None,
        name: str | None = None,
        tags: list[str] | None = None,
    ) -> dict:
        if self._wire_dialect is None or not self._wire_dialect.supports_context_cache:
            raise AttributeError("This OpenAI-chat dialect does not support context caching")
        body: dict[str, Any] = {"model": model or self._cache_model_family(), "messages": messages}
        if tools is not None:
            body["tools"] = tools
        if name is not None:
            body["name"] = name
        if tags is not None:
            body["tags"] = tags
        if expired_at is not None:
            body["expired_at"] = expired_at
        elif ttl is not None:
            body["ttl"] = ttl
        async with httpx.AsyncClient() as client:
            response = await client.post(
                f"{self._base_url}/caching",
                headers={"Authorization": f"Bearer {self._client.api_key}", "Content-Type": "application/json"},
                json=body,
                timeout=30,
            )
            response.raise_for_status()
            return response.json()

    async def resolve_cache_tag(self, tag: str) -> dict:
        if self._wire_dialect is None or not self._wire_dialect.supports_context_cache:
            raise AttributeError("This OpenAI-chat dialect does not support context caching")
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self._base_url}/caching/refs/tags/{tag}",
                headers={"Authorization": f"Bearer {self._client.api_key}"},
                timeout=30,
            )
            response.raise_for_status()
            return response.json()

    def _build_tools(self, tools: list[ToolSchema]) -> list[dict] | None:
        if not tools:
            return None
        return [
            {
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": json.loads(t.parameters),
                },
            }
            for t in tools
        ]

    def _headers(self) -> dict[str, str]:
        return {
            "authorization": f"Bearer {self._client.api_key}",
            "content-type": "application/json",
        }

    def _build_body(
        self,
        messages: list[dict],
        tools: list[ToolSchema],
        stream: bool,
        extensions: dict | None = None,
    ) -> dict:
        body = {
            **wire_request_extensions(extensions),
            "model": self._model,
            "messages": messages,
            "stream": stream,
        }
        tool_defs = self._build_tools(tools)
        if tool_defs:
            body["tools"] = tool_defs
        return body

    def _prompt_cache_key(self, context: RenderedContext, tools: list[ToolSchema]) -> str:
        """Default ``prompt_cache_key`` from the cacheable prefix (system prompt +
        tool names). A caller-supplied key in extensions wins (setdefault). Unknown
        to non-OpenAI compatible endpoints, which ignore it."""
        return stable_prompt_cache_key([context.system_text, ",".join(t.name for t in tools)])

    def _canonical_input(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None):
        return normalize_canonical_adapter_input(
            context,
            tools,
            extensions=extensions,
            resolved=getattr(self, "_resolved_runtime", None),
        )

    def _replay_for_assistant(self, content: str, tool_calls: list[ToolCall]) -> dict | None:
        return self._replay_fields.get(assistant_replay_key(content, tool_calls))

    async def _complete_with_adapter(
        self,
        context: RenderedContext,
        tools: list[ToolSchema],
        extensions: dict | None,
    ) -> Message:
        adapter_input = self._canonical_input(context, tools, extensions)
        plan = self._adapter.build_request(adapter_input, self._wire_dialect, self._replay_for_assistant)
        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                response = await self._client.chat.completions.create(**plan.params)
                self._circuit.record_success()
                decoded = self._adapter.decode_complete(response, adapter_input, self._wire_dialect)
                if decoded.replay:
                    self.remember_replay_fields(decoded.message, decoded.replay)
                return decoded.message
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    import asyncio
                    await asyncio.sleep(self._retry.base_delay * (2 ** attempt))
        raise last_exc or RuntimeError("Complete failed")

    async def _stream_with_adapter(
        self,
        context: RenderedContext,
        tools: list[ToolSchema],
        extensions: dict | None,
    ) -> AsyncIterator[StreamEvent]:
        adapter_input = self._canonical_input(context, tools, extensions)
        plan = self._adapter.build_request(adapter_input, self._wire_dialect, self._replay_for_assistant)
        stream = await self._client.chat.completions.create(
            **plan.params,
            stream=True,
            stream_options={"include_usage": True},
        )
        stream_state = self._adapter.create_stream_state(adapter_input, self._wire_dialect)
        async for chunk in stream:
            output = self._adapter.push_stream_chunk(chunk, stream_state)
            if output.replay:
                self.remember_replay_fields(
                    Message(role="assistant", content=stream_state.accumulated_content,
                            tool_calls=self._adapter._final_tool_calls(stream_state.tool_call_buffers) or None),
                    output.replay,
                )
            for event in output.events:
                yield event
        output = self._adapter.finish_stream(stream_state)
        if output.replay:
            self.remember_replay_fields(
                Message(role="assistant", content=stream_state.accumulated_content,
                        tool_calls=self._adapter._final_tool_calls(stream_state.tool_call_buffers) or None),
                output.replay,
            )
        for event in output.events:
            yield event

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        if self._wire_dialect is not None:
            return await self._complete_with_adapter(context, tools, extensions)

        prepared = self._prepare_extensions(extensions)
        msgs = self._build_messages(context, extensions)
        tool_defs = self._wire_tools(tools, extensions)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                request_extensions = wire_request_extensions(prepared)
                for k, v in self._cache_key_params(context, tools).items():
                    request_extensions.setdefault(k, v)
                resp = await self._client.chat.completions.create(
                    **request_extensions,
                    model=self._model,
                    messages=msgs,
                    tools=tool_defs,
                )
                self._circuit.record_success()

                choice = resp.choices[0].message
                content = choice.content or ""
                native_tool_calls = choice.tool_calls or []
                tool_calls: list[ToolCall] = []
                for tc in native_tool_calls:
                    normalized = normalize_tool_call(tc.id, tc.function.name, tc.function.arguments)
                    if normalized:
                        tool_calls.append(normalized)

                self._remember_complete_replay(content, tool_calls, OpenAIChatTurnReasoning(
                    reasoning_content=_extra_field(choice, "reasoning_content") or "",
                    reasoning_details=_extra_field(choice, "reasoning_details"),
                    native_tool_calls=[
                        {"id": tc.id, "type": "function", "function": {"name": tc.function.name, "arguments": tc.function.arguments}}
                        for tc in native_tool_calls
                    ],
                ))

                return Message(
                    role="assistant",
                    content=content,
                    token_count=resp.usage.total_tokens if resp.usage else None,
                    tool_calls=tool_calls or None,
                )
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    import asyncio
                    delay = self._retry.base_delay * (2 ** attempt)
                    await asyncio.sleep(delay)

        raise last_exc or RuntimeError("Complete failed")

    async def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        if self._wire_dialect is not None:
            async for event in self._stream_with_adapter(context, tools, extensions):
                yield event
            return
        prepared = self._prepare_extensions(extensions)
        msgs = self._build_messages(context, extensions)
        tool_defs = self._wire_tools(tools, extensions)
        expose_reasoning = self._expose_reasoning_delta(extensions)
        use_tags = self._uses_inline_thinking_tags()
        capture_details = self._capture_reasoning_details()
        tool_call_bufs: dict[int, dict] = {}
        emitted_tool_call_indexes: set[int] = set()
        extractor = ThinkingTagStreamExtractor()
        accumulated_reasoning = ""
        accumulated_details = None
        accumulated_content = ""
        final_tool_calls = []

        def _remember() -> None:
            self._remember_stream_replay(accumulated_content, final_tool_calls, OpenAIChatTurnReasoning(
                reasoning_content=accumulated_reasoning,
                reasoning_details=accumulated_details,
                native_tool_calls=_native_tool_calls_from_bufs(tool_call_bufs),
            ))

        request_extensions = wire_request_extensions(prepared)
        for k, v in self._cache_key_params(context, tools).items():
            request_extensions.setdefault(k, v)
        stream = await self._client.chat.completions.create(
            **request_extensions,
            model=self._model,
            messages=msgs,
            tools=tool_defs,
            stream=True,
            stream_options={"include_usage": True},
        )

        # Phase 4: OpenAI signals an output-cap truncation via finish_reason="length", which arrives
        # on a choices frame BEFORE the trailing usage frame — capture it and attach it to the usage
        # event. The kernel treats "length" as a truncation (== Anthropic "max_tokens").
        finish_reason_seen: "str | None" = None
        async for chunk in stream:
            usage = getattr(chunk, "usage", None)
            if usage:
                # Automatic prefix-cache hits surface as prompt_tokens_details.cached_tokens
                # (a subset of prompt_tokens, which stays the full prompt count).
                yield UsageEvent(
                    total_tokens=getattr(usage, "total_tokens", 0) or 0,
                    input_tokens=getattr(usage, "prompt_tokens", 0) or 0,
                    output_tokens=getattr(usage, "completion_tokens", 0) or 0,
                    cache_read_input_tokens=openai_cached_prompt_tokens(usage),
                    stop_reason=canonicalize_stop_reason(finish_reason_seen),
                    raw_stop_reason=finish_reason_seen,
                    provider_usage=normalize_usage(usage),
                )
                continue
            choice = chunk.choices[0] if chunk.choices else None
            if not choice:
                continue
            if choice.finish_reason:
                finish_reason_seen = choice.finish_reason

            delta = getattr(choice, "delta", None)
            if not delta:
                continue

            native_reasoning = _extra_field(delta, "reasoning_content")
            if native_reasoning:
                accumulated_reasoning += str(native_reasoning)
                if expose_reasoning:
                    yield ThinkingDelta(delta=native_reasoning)
            if capture_details:
                details = _extra_field(delta, "reasoning_details")
                if details is not None:
                    accumulated_details = details

            if delta.content:
                if use_tags:
                    for part in extractor.feed(delta.content):
                        if part["type"] == "thinking":
                            accumulated_reasoning += part["content"]
                            yield ThinkingDelta(delta=part["content"])
                        else:
                            accumulated_content += part["content"]
                            yield TextDelta(delta=part["content"])
                else:
                    accumulated_content += str(delta.content)
                    yield TextDelta(delta=delta.content)

            for tc in delta.tool_calls or []:
                idx = tc.index
                if idx not in tool_call_bufs:
                    tool_call_bufs[idx] = {"id": tc.id or "", "name": "", "args_buf": ""}
                if tc.function and tc.function.name:
                    tool_call_bufs[idx]["name"] += tc.function.name
                if tc.function and tc.function.arguments:
                    tool_call_bufs[idx]["args_buf"] += tc.function.arguments

            if choice.finish_reason == "tool_calls":
                for idx, tb in tool_call_bufs.items():
                    if idx in emitted_tool_call_indexes:
                        continue
                    try:
                        args = json.loads(tb["args_buf"] or "{}")
                    except json.JSONDecodeError:
                        args = {}
                    tc_obj = normalize_tool_call(tb["id"], tb["name"], args)
                    if tc_obj:
                        final_tool_calls.append(tc_obj)
                        emitted_tool_call_indexes.add(idx)
                        yield ToolCallEvent(id=tc_obj.id, name=tc_obj.name, arguments=args)
                _remember()

        if use_tags:
            for part in extractor.flush():
                if part["type"] == "thinking":
                    accumulated_reasoning += part["content"]
                    yield ThinkingDelta(delta=part["content"])
                else:
                    accumulated_content += part["content"]
                    yield TextDelta(delta=part["content"])

        for idx, tb in tool_call_bufs.items():
            if idx in emitted_tool_call_indexes:
                continue
            try:
                args = json.loads(tb["args_buf"] or "{}")
            except json.JSONDecodeError:
                args = {}
            tc_obj = normalize_tool_call(tb["id"], tb["name"], args)
            if tc_obj:
                final_tool_calls.append(tc_obj)
                yield ToolCallEvent(id=tc_obj.id, name=tc_obj.name, arguments=args)

        _remember()
