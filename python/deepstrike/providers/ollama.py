from __future__ import annotations
import logging
from typing import AsyncIterator
import httpx
from deepstrike._kernel import Message, ToolSchema
from .stream import StreamEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, RuntimePolicy
from .ollama_adapter import OllamaAdapter
from deepstrike.types.content import normalize_canonical_adapter_input

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
        self._adapter = OllamaAdapter(model)

    def runtime_policy(self) -> RuntimePolicy:
        m = self._model.lower()
        for prefix, policy in _OLLAMA_PREFIX_POLICIES:
            if m.startswith(prefix):
                return policy
        return RuntimePolicy(max_turns=20)

    def _build_body(self, context: RenderedContext, tools: list[ToolSchema], stream: bool, extensions: dict | None = None) -> dict:
        canonical = normalize_canonical_adapter_input(
            context,
            tools,
            extensions=extensions,
            resolved=getattr(self, "_resolved_runtime", None),
        )
        body = self._adapter.build_request(canonical)
        body["stream"] = stream
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

                self._circuit.record_success()
                canonical = normalize_canonical_adapter_input(
                    context, tools, extensions=extensions,
                    resolved=getattr(self, "_resolved_runtime", None),
                )
                return self._adapter.decode_complete(data, canonical)
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
        canonical = normalize_canonical_adapter_input(
            context, tools, extensions=extensions,
            resolved=getattr(self, "_resolved_runtime", None),
        )
        state = self._adapter.create_stream_state(canonical)
        decoder = self._adapter.create_ndjson_decoder()

        async with httpx.AsyncClient() as client:
            async with client.stream(
                "POST",
                f"{self._base_url}/api/chat",
                json=self._build_body(context, tools, stream=True, extensions=extensions),
                timeout=120,
            ) as resp:
                resp.raise_for_status()
                if hasattr(resp, "aiter_text"):
                    async for text in resp.aiter_text():
                        for chunk in decoder.push(text):
                            for event in self._adapter.push_stream_chunk(chunk, state).events:
                                yield event
                else:
                    async for line in resp.aiter_lines():
                        for chunk in decoder.push(f"{line}\n"):
                            for event in self._adapter.push_stream_chunk(chunk, state).events:
                                yield event
                for chunk in decoder.finish():
                    for event in self._adapter.push_stream_chunk(chunk, state).events:
                        yield event

        for event in self._adapter.finish_stream(state).events:
            yield event
