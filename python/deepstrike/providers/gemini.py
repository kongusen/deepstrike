from __future__ import annotations
import logging
from typing import AsyncIterator
try:
    from google import genai as google_genai
except ImportError:  # pragma: no cover - exercised only when optional provider dep is absent.
    google_genai = None
from deepstrike._kernel import Message, ToolSchema
from .stream import StreamEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, RuntimePolicy
from .gemini_adapter import GeminiAdapter
from deepstrike.types.content import normalize_canonical_adapter_input

logger = logging.getLogger(__name__)

_GEMINI_POLICIES: dict[str, RuntimePolicy] = {
    "gemini-3-pro-preview": RuntimePolicy(max_turns=50),
    "gemini-3-flash-preview": RuntimePolicy(max_turns=25),
    "gemini-3.5-flash": RuntimePolicy(max_turns=30),
    "gemini-2.5-pro":        RuntimePolicy(max_turns=35),
    "gemini-2.5-flash":      RuntimePolicy(max_turns=20),
    "gemini-2.0-flash":      RuntimePolicy(max_turns=15),
    "gemini-2.0-flash-lite": RuntimePolicy(max_turns=10),
    "gemini-1.5-pro":        RuntimePolicy(max_turns=30),
    "gemini-1.5-flash":      RuntimePolicy(max_turns=15),
}


class GeminiProvider:
    def __init__(
        self,
        api_key: str,
        model: str = "gemini-2.0-flash",
        retry_config: RetryConfig | None = None,
        base_url: str = "https://generativelanguage.googleapis.com",
    ):
        self._model_name = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        self._base_url = base_url.rstrip("/")
        self._api_key = api_key
        self._client = None
        self._model = None
        self._adapter = GeminiAdapter(model)

    def _create_client(self, api_key: str):
        if google_genai is None:
            return None
        if self._base_url == "https://generativelanguage.googleapis.com":
            return google_genai.Client(api_key=api_key)
        return google_genai.Client(api_key=api_key, http_options={"base_url": self._base_url})

    def _require_client(self):
        if self._client is None:
            self._client = self._create_client(self._api_key)
        if self._client is None:
            raise RuntimeError("GeminiProvider requires the google-genai package. Install with: pip install google-genai")
        return self._client

    def runtime_policy(self) -> RuntimePolicy:
        return _GEMINI_POLICIES.get(self._model_name, RuntimePolicy())

    def _build_contents(self, turns: list[Message]) -> list[dict]:
        return self._adapter.build_contents(turns)

    def _build_tools(self, tools: list[ToolSchema]) -> list[dict] | None:
        return self._adapter.build_tools(tools)

    def _build_config(self, system: str | None, tools: list[ToolSchema], extensions: dict | None = None) -> dict | None:
        return self._adapter.build_config(system, tools, extensions)

    async def create_context_cache(
        self,
        *,
        system_instruction: str | None = None,
        contents: list | None = None,
        tools: list | None = None,
        ttl: str = "3600s",
        display_name: str | None = None,
        model: str | None = None,
    ):
        """Create a Gemini explicit context cache; returns the ``CachedContent`` (pass its ``.name`` as
        ``extensions={"cached_content": name}`` on later calls). ``ttl`` is a ``"<seconds>s"`` string.
        Explicit caches have a per-model minimum input-token floor (~1024 flash / ~4096 pro)."""
        client = self._require_client()
        # Plain-dict config (the SDK dict-coerces it to CreateCachedContentConfig), consistent with the
        # generate_content config this provider already passes as a dict.
        cfg: dict = {"ttl": ttl}
        if system_instruction is not None:
            cfg["system_instruction"] = system_instruction
        if contents is not None:
            cfg["contents"] = contents
        if tools is not None:
            cfg["tools"] = tools
        if display_name is not None:
            cfg["display_name"] = display_name
        return await client.aio.caches.create(model=model or self._model_name, config=cfg)

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
        plan = self._adapter.build_request(adapter_input)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                if self._model is not None:
                    resp = await self._model.generate_content_async(plan.contents)
                else:
                    resp = await self._require_client().aio.models.generate_content(
                        model=self._model_name,
                        contents=plan.contents,
                        config=plan.config,
                    )
                self._circuit.record_success()
                return self._adapter.decode_complete(resp, adapter_input)
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    import asyncio
                    delay = self._retry.base_delay * (2 ** attempt)
                    await asyncio.sleep(delay)

        raise last_exc or RuntimeError("Complete failed")

    async def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        adapter_input = self._canonical_input(context, tools, extensions)
        plan = self._adapter.build_request(adapter_input)
        stream_state = self._adapter.create_stream_state(adapter_input)

        if self._model is not None:
            stream = await self._model.generate_content_async(plan.contents, stream=True)
        else:
            stream = await self._require_client().aio.models.generate_content_stream(
                model=self._model_name,
                contents=plan.contents,
                config=plan.config,
            )

        async for chunk in stream:
            for event in self._adapter.push_stream_chunk(chunk, stream_state).events:
                yield event
        for event in self._adapter.finish_stream(stream_state).events:
            yield event
