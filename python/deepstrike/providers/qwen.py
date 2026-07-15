from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from http import HTTPStatus
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, RuntimePolicy, normalize_tool_call, openai_cached_prompt_tokens, to_openai_message_params, UnsupportedModalityError
from .replay import ReasoningReplayMixin
from .anthropic_compatible import AnthropicCompatibleProvider
from .vendor_profiles import QWEN_POLICIES as _QWEN_POLICIES, ANTHROPIC_VENDOR_PROFILES

logger = logging.getLogger(__name__)


class QwenAnthropicProvider(AnthropicCompatibleProvider):
    """Qwen over its Anthropic-compatible endpoint.

    Deprecated: prefer ``qwen(protocol="anthropic")``. Data-driven via
    ``ANTHROPIC_VENDOR_PROFILES["qwen"]``; thin shim for backward compat / isinstance.
    """

    def __init__(
        self,
        api_key: str,
        model: str | None = None,
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        super().__init__(ANTHROPIC_VENDOR_PROFILES["qwen"], api_key, model, retry_config, base_url)


class QwenProvider(ReasoningReplayMixin):
    def __init__(
        self,
        api_key: str,
        model: str = "qwen3.6-plus",
        retry_config: RetryConfig | None = None,
        base_url: str = "https://dashscope.aliyuncs.com/compatible-mode/v1",
    ):
        self._api_key = api_key
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        self._base_url = base_url
        self._init_replay_store()
        import dashscope  # noqa: PLC0415
        dashscope.api_key = api_key
        from dashscope import AioGeneration
        from dashscope.api_entities.dashscope_response import Role
        self._generation = AioGeneration
        self._role = Role
        try:
            from dashscope import AioMultiModalConversation
            self._mm_generation = AioMultiModalConversation
        except ImportError:  # pragma: no cover - older dashscope without multimodal
            self._mm_generation = None

    def runtime_policy(self) -> RuntimePolicy:
        return _QWEN_POLICIES.get(self._model, RuntimePolicy())

    def _build_messages(self, context: RenderedContext) -> list[dict]:
        serialized = self._merge_replay_into_openai_messages(
            to_openai_message_params(context),
            context,
        )
        for msg in serialized:
            if msg["role"] == "system":
                msg["role"] = self._role.SYSTEM
            elif msg["role"] == "assistant":
                msg["role"] = self._role.ASSISTANT
            elif msg["role"] == "user":
                msg["role"] = self._role.USER
            elif msg["role"] == "tool":
                msg["role"] = self._role.TOOL
        return serialized

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

    # ── Multimodal (qwen-vl / qwen3-omni via dashscope MultiModalConversation) ──
    # Image input requires the MultiModalConversation API (a different dashscope class from the
    # text-only Generation path): content is a list of single-modality dicts and the response
    # message.content comes back as a list too. Routed to only when the input carries an image.

    @staticmethod
    def _has_image_input(context: RenderedContext) -> bool:
        for turn in context.turns:
            for p in getattr(turn, "content_parts", None) or []:
                if getattr(p, "type", None) == "image":
                    return True
        return False

    def _build_mm_messages(self, context: RenderedContext) -> list[dict]:
        msgs: list[dict] = []
        if context.system_text:
            msgs.append({"role": "system", "content": [{"text": context.system_text}]})
        for turn in context.turns:
            parts: list[dict] = []
            for p in getattr(turn, "content_parts", None) or []:
                ptype = getattr(p, "type", None)
                if ptype == "text":
                    parts.append({"text": p.text})
                elif ptype == "image":
                    data = getattr(p, "data", None)
                    if data:
                        parts.append({"image": f"data:{getattr(p, 'media_type', None) or 'image/png'};base64,{data}"})
                    elif getattr(p, "url", None):
                        parts.append({"image": p.url})
                elif ptype == "audio":
                    raise UnsupportedModalityError("audio", "qwen")
            if not parts and turn.content:
                parts.append({"text": turn.content})
            if parts:
                msgs.append({"role": turn.role, "content": parts})
        return msgs

    @staticmethod
    def _mm_text(content) -> str:
        # MultiModalConversation returns message.content as a list of {"text": ...} dicts (not a str).
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            return "".join(p.get("text", "") for p in content if isinstance(p, dict))
        return ""

    def _mm_call_kwargs(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None, *, stream: bool) -> dict:
        ext = extensions or {}
        kwargs: dict = {"model": self._model, "messages": self._build_mm_messages(context), "result_format": "message"}
        if stream:
            kwargs["stream"] = True
            kwargs["incremental_output"] = True
        tool_defs = self._build_tools(tools)
        if tool_defs:
            kwargs["tools"] = tool_defs
        if ext.get("enable_search"):
            kwargs["enable_search"] = True
            if ext.get("search_options") is not None:
                kwargs["search_options"] = ext["search_options"]
        return kwargs

    async def _complete_mm(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None) -> Message:
        if self._mm_generation is None:
            raise RuntimeError("Qwen multimodal needs a dashscope build with MultiModalConversation")
        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                resp = await self._mm_generation.call(**self._mm_call_kwargs(context, tools, extensions, stream=False))
                if resp.status_code != HTTPStatus.OK:
                    raise RuntimeError(f"DashScope error: {resp.code} - {resp.message}")
                self._circuit.record_success()
                choice = resp.output.choices[0].message
                tool_calls: list[ToolCall] = []
                for tc in getattr(choice, "tool_calls", None) or []:
                    normalized = normalize_tool_call(tc.function.name, tc.function.name, tc.function.arguments)
                    if normalized:
                        tool_calls.append(normalized)
                return Message(
                    role="assistant",
                    content=self._mm_text(choice.content),
                    token_count=getattr(resp.usage, "total_tokens", None) if resp.usage else None,
                    tool_calls=tool_calls or None,
                )
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    import asyncio
                    await asyncio.sleep(self._retry.base_delay * (2 ** attempt))
        raise last_exc or RuntimeError("Complete failed")

    async def _stream_mm(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None) -> AsyncIterator[StreamEvent]:
        if self._mm_generation is None:
            raise RuntimeError("Qwen multimodal needs a dashscope build with MultiModalConversation")
        last_usage = None
        stream = await self._mm_generation.call(**self._mm_call_kwargs(context, tools, extensions, stream=True))
        async for chunk in stream:
            if chunk.status_code != HTTPStatus.OK:
                continue
            if getattr(chunk, "usage", None):
                last_usage = chunk.usage
            choice = chunk.output.choices[0] if chunk.output.choices else None
            if not choice:
                continue
            delta = getattr(choice, "message", None)
            if not delta:
                continue
            text = self._mm_text(getattr(delta, "content", None))
            if text:
                yield TextDelta(delta=text)
        if last_usage is not None:
            input_tokens = getattr(last_usage, "input_tokens", 0) or 0
            output_tokens = getattr(last_usage, "output_tokens", 0) or 0
            total = getattr(last_usage, "total_tokens", 0) or (input_tokens + output_tokens)
            yield UsageEvent(total_tokens=total, input_tokens=input_tokens, output_tokens=output_tokens)

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")
        if self._has_image_input(context):
            return await self._complete_mm(context, tools, extensions)

        msgs = self._build_messages(context)
        tool_defs = self._build_tools(tools)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                ext = extensions or {}
                kwargs = {
                    "model": self._model,
                    "messages": msgs,
                    "result_format": "message",
                    "tools": tool_defs,
                }
                if ext.get("enable_thinking") or ext.get("enableThinking"):
                    kwargs["enable_thinking"] = True
                    if "thinking_budget" in ext or "thinkingBudget" in ext:
                        kwargs["thinking_budget"] = int(ext.get("thinking_budget", ext.get("thinkingBudget")))
                # DashScope web search (Qwen vendor feature): enable_search + optional search_options.
                if ext.get("enable_search"):
                    kwargs["enable_search"] = True
                    if ext.get("search_options") is not None:
                        kwargs["search_options"] = ext["search_options"]
                for key, value in ext.items():
                    if key not in {"model", "messages", "tools", "stream", "enable_thinking", "enableThinking", "thinking_budget", "thinkingBudget", "enable_search", "search_options"}:
                        kwargs[key] = value
                resp = await self._generation.call(**kwargs)
                if resp.status_code != HTTPStatus.OK:
                    raise RuntimeError(f"DashScope error: {resp.code} - {resp.message}")

                self._circuit.record_success()
                choice = resp.output.choices[0].message
                content = choice.content or ""
                tool_calls: list[ToolCall] = []

                for tc in choice.tool_calls or []:
                    normalized = normalize_tool_call(tc.function.name, tc.function.name, tc.function.arguments)
                    if normalized:
                        tool_calls.append(normalized)

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
        if self._has_image_input(context):
            async for evt in self._stream_mm(context, tools, extensions):
                yield evt
            return

        msgs = self._build_messages(context)
        tool_defs = self._build_tools(tools)

        ext = extensions or {}
        kwargs = {
            "model": self._model,
            "messages": msgs,
            "result_format": "message",
            "stream": True,
            "incremental_output": True,
        }
        if tool_defs:
            kwargs["tools"] = tool_defs
        if ext.get("enable_thinking") or ext.get("enableThinking"):
            kwargs["enable_thinking"] = True
            if "thinking_budget" in ext or "thinkingBudget" in ext:
                kwargs["thinking_budget"] = int(ext.get("thinking_budget", ext.get("thinkingBudget")))
        # DashScope web search (Qwen vendor feature) — was dropped on the stream path before.
        if ext.get("enable_search"):
            kwargs["enable_search"] = True
            if ext.get("search_options") is not None:
                kwargs["search_options"] = ext["search_options"]

        tool_call_bufs: dict[int, dict] = {}
        emitted_tool_call_indexes: set[int] = set()
        reasoning_content = ""
        final_text = ""
        final_tool_calls: list[ToolCall] = []
        last_usage = None

        stream = await self._generation.call(**kwargs)
        async for chunk in stream:
            if chunk.status_code != HTTPStatus.OK:
                continue

            # DashScope reports cumulative usage on each chunk; keep the latest.
            if getattr(chunk, "usage", None):
                last_usage = chunk.usage

            choice = chunk.output.choices[0] if chunk.output.choices else None
            if not choice:
                continue

            delta = getattr(choice, "message", None)
            if not delta:
                continue
            if hasattr(delta, "reasoning_content") and delta.reasoning_content:
                reasoning_content += str(delta.reasoning_content)
                yield ThinkingDelta(delta=delta.reasoning_content)
            if delta.content:
                final_text += str(delta.content)
                yield TextDelta(delta=delta.content)

            for tc in delta.tool_calls or []:
                idx = getattr(tc, "index", 0)
                if idx not in tool_call_bufs:
                    tool_call_bufs[idx] = {"id": tc.function.name, "name": "", "args_buf": ""}
                if tc.function.name:
                    tool_call_bufs[idx]["name"] = tc.function.name
                if tc.function.arguments:
                    tool_call_bufs[idx]["args_buf"] = tc.function.arguments

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

        self.remember_reasoning_for_turn(final_text, final_tool_calls, reasoning_content)

        if last_usage is not None:
            # DashScope usage uses input_tokens / output_tokens / total_tokens.
            input_tokens = getattr(last_usage, "input_tokens", 0) or 0
            output_tokens = getattr(last_usage, "output_tokens", 0) or 0
            total = getattr(last_usage, "total_tokens", 0) or (input_tokens + output_tokens)
            yield UsageEvent(
                total_tokens=total,
                input_tokens=input_tokens,
                output_tokens=output_tokens,
                cache_read_input_tokens=openai_cached_prompt_tokens(last_usage),
            )
