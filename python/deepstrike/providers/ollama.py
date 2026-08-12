from __future__ import annotations
import json
import logging
from typing import AsyncIterator
import httpx
from deepstrike._kernel import Message, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, RuntimePolicy, normalize_tool_call, turns_with_state_appended
from deepstrike.providers.base import UnsupportedModalityError
from deepstrike.types.content import normalize_tool_result, project_tool_output_to_text
from .stop_reason import canonicalize_stop_reason

logger = logging.getLogger(__name__)

_DEFAULT_BASE_URL = "http://localhost:11434"

_OLLAMA_PREFIX_POLICIES: list[tuple[str, RuntimePolicy]] = [
    ("deepseek-r1",  RuntimePolicy(max_turns=40)),
    ("qwq",          RuntimePolicy(max_turns=35)),
    ("llama3.3",     RuntimePolicy(max_turns=25)),
    ("llama3.2",     RuntimePolicy(max_turns=20)),
    ("llama3.1",     RuntimePolicy(max_turns=20)),
    ("llama3",       RuntimePolicy(max_turns=20)),
    ("mistral",      RuntimePolicy(max_turns=20)),
    ("gemma2",       RuntimePolicy(max_turns=20)),
    ("phi4",         RuntimePolicy(max_turns=20)),
    ("phi3",         RuntimePolicy(max_turns=15)),
    ("codellama",    RuntimePolicy(max_turns=20)),
]


class OllamaProvider:
    def __init__(self, model: str = "llama3", base_url: str = _DEFAULT_BASE_URL, retry_config: RetryConfig | None = None):
        self._model = model
        self._base_url = base_url.rstrip("/")
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)

    def runtime_policy(self) -> RuntimePolicy:
        m = self._model.lower()
        for prefix, policy in _OLLAMA_PREFIX_POLICIES:
            if m.startswith(prefix):
                return policy
        return RuntimePolicy(max_turns=20)

    def _build_body(self, context: RenderedContext, tools: list[ToolSchema], stream: bool, extensions: dict | None = None) -> dict:
        msgs = []
        if context.system_text:
            msgs.append({"role": "system", "content": context.system_text})
        for m in turns_with_state_appended(context):
            entry: dict = {"role": m.role, "content": m.content}
            parts = getattr(m, "content_parts", None)
            if parts:
                if any(p.type == "audio" for p in parts):
                    raise UnsupportedModalityError("audio", "ollama")
                images = [p.data for p in parts if p.type == "image" and p.data]
                tool_results = [p for p in parts if p.type == "tool_result"]
                if tool_results:
                    part = tool_results[0]
                    entry["content"] = project_tool_output_to_text(
                        normalize_tool_result(
                            part.call_id,
                            part.output,
                            part.is_error,
                            getattr(part, "content_parts", None),
                        ).blocks
                    )
                if images:
                    entry["images"] = images
            msgs.append(entry)
        body: dict = {
            **{k: v for k, v in (extensions or {}).items() if k not in {"model", "messages", "tools", "stream"}},
            "model": self._model,
            "messages": msgs,
            "stream": stream,
        }
        if tools:
            body["tools"] = [
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
        return body

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise Exception("Circuit breaker open")

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                async with httpx.AsyncClient() as client:
                    resp = await client.post(
                        f"{self._base_url}/api/chat",
                        json=self._build_body(context, tools, stream=False, extensions=extensions),
                        timeout=120,
                    )
                    resp.raise_for_status()
                    data = resp.json()

                msg = data.get("message", {})
                content_text = msg.get("content") or ""
                tool_calls = []
                for tc in msg.get("tool_calls") or []:
                    fn = tc.get("function", {})
                    normalized = normalize_tool_call(tc.get("id", ""), fn.get("name", ""), fn.get("arguments", {}))
                    if normalized:
                        tool_calls.append(normalized)

                self._circuit.record_success()
                from deepstrike._kernel import Message as KMessage
                return KMessage(role="assistant", content=content_text, token_count=0, tool_calls=tool_calls)
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    import asyncio
                    delay = self._retry.base_delay * (2 ** attempt)
                    logger.warning("Retry %d/%d after %.1fs: %s", attempt + 1, self._retry.max_retries, delay, exc)
                    await asyncio.sleep(delay)

        raise last_exc or Exception("Complete failed")

    def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        return self._stream_gen(context, tools, extensions)

    async def _stream_gen(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        pending_tool_calls: dict[str, dict] = {}
        raw_stop_reason: str | None = None

        async with httpx.AsyncClient() as client:
            async with client.stream(
                "POST",
                f"{self._base_url}/api/chat",
                json=self._build_body(context, tools, stream=True, extensions=extensions),
                timeout=120,
            ) as resp:
                resp.raise_for_status()
                async for line in resp.aiter_lines():
                    if not line:
                        continue
                    chunk = json.loads(line)
                    msg = chunk.get("message", {})
                    done = chunk.get("done", False)
                    if done:
                        reason = chunk.get("done_reason")
                        if isinstance(reason, str) and reason:
                            raw_stop_reason = reason

                    if text := msg.get("content"):
                        yield TextDelta(delta=text)

                    for tc in msg.get("tool_calls") or []:
                        fn = tc.get("function", {})
                        args = fn.get("arguments", {})
                        normalized = normalize_tool_call("", fn.get("name", ""), args)
                        if normalized:
                            key = f"{normalized.name}:{normalized.arguments}"
                            if key not in pending_tool_calls:
                                pending_tool_calls[key] = {
                                    "id": f"call_{len(pending_tool_calls) + 1}",
                                    "name": normalized.name,
                                    "arguments": args,
                                }

        for tc in pending_tool_calls.values():
            yield ToolCallEvent(id=tc["id"], name=tc["name"], arguments=tc["arguments"])

        # Ollama does not report usage on the stream; emit the stop-reason carrier only.
        if raw_stop_reason is not None:
            from .stream import UsageEvent
            yield UsageEvent(
                total_tokens=0,
                input_tokens=0,
                output_tokens=0,
                stop_reason=canonicalize_stop_reason(raw_stop_reason),
                raw_stop_reason=raw_stop_reason,
            )
