from __future__ import annotations
import json
import logging
from typing import AsyncIterator
try:
    from google import genai as google_genai
except ImportError:  # pragma: no cover - exercised only when optional provider dep is absent.
    google_genai = None
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent, UsageEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, RuntimePolicy, normalize_tool_call, turns_with_state_appended

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
        contents = []
        for msg in turns:
            if msg.role == "tool":
                parts = []
                for p in getattr(msg, "content_parts", []):
                    if p.type == "tool_result":
                        tool_name = p.call_id
                        for turn in reversed(turns):
                            if turn.role == "assistant" and turn.tool_calls:
                                matched = next((tc for tc in turn.tool_calls if tc.id == p.call_id), None)
                                if matched:
                                    tool_name = matched.name
                                    break
                        parts.append({
                            "function_response": {
                                "name": tool_name,
                                "response": {"output": p.output},
                            }
                        })
                if parts:
                    contents.append({"role": "user", "parts": parts})
                continue

            role = "model" if msg.role == "assistant" else "user"
            parts = []
            if msg.tool_calls:
                for tc in msg.tool_calls:
                    try:
                        args = json.loads(tc.arguments)
                    except json.JSONDecodeError:
                        args = {}
                    parts.append({"function_call": {"name": tc.name, "args": args}})
            # Multimodal: render content_parts (text + image) when present, else the
            # plain text body. Without this, image inputs to Gemini were dropped.
            cparts = getattr(msg, "content_parts", None) or []
            if cparts:
                for p in cparts:
                    if p.type == "text":
                        parts.append({"text": p.text})
                    elif p.type == "image":
                        if getattr(p, "data", None):
                            parts.append({"inline_data": {"mime_type": p.media_type or "image/png", "data": p.data}})
                        elif getattr(p, "url", None):
                            parts.append({"file_data": {"mime_type": p.media_type or "image/png", "file_uri": p.url}})
            elif msg.content:
                parts.append({"text": msg.content})
            if parts:
                contents.append({"role": role, "parts": parts})
        return contents

    def _build_tools(self, tools: list[ToolSchema]) -> list[dict] | None:
        if not tools:
            return None
        declarations = [
            {
                "name": t.name,
                "description": t.description,
                "parameters_json_schema": json.loads(t.parameters),
            }
            for t in tools
        ]
        return [{"function_declarations": declarations}]

    def _build_config(self, system: str | None, tools: list[ToolSchema], extensions: dict | None = None) -> dict | None:
        ext = extensions or {}
        config: dict = {}
        if system:
            config["system_instruction"] = system
        tool_defs = list(self._build_tools(tools) or [])
        # Google Search grounding (server tool; gemini-2.0+). Coexists with function tools on current
        # models. `google_search` truthy → default tool; a dict passes config through.
        gs = ext.get("google_search")
        if gs:
            tool_defs.append({"google_search": gs if isinstance(gs, dict) else {}})
        if tool_defs:
            config["tools"] = tool_defs
            config["automatic_function_calling"] = {"disable": True}
        # Thinking (gemini-2.5 / 3 only; caller-provided budget/include_thoughts — 0=off, -1=auto).
        if ext.get("thinking_config") is not None:
            config["thinking_config"] = ext["thinking_config"]
        # Structured output (not combinable with google_search — the API rejects that pairing).
        if ext.get("response_mime_type") is not None:
            config["response_mime_type"] = ext["response_mime_type"]
        if ext.get("response_schema") is not None:
            config["response_schema"] = ext["response_schema"]
        # Explicit context cache reference (the `cachedContents/…` name from create_context_cache()).
        if ext.get("cached_content") is not None:
            config["cached_content"] = ext["cached_content"]
        return config or None

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

    def _response_parts(self, response) -> list:
        if getattr(response, "parts", None):
            return list(response.parts)
        candidates = getattr(response, "candidates", None) or []
        if not candidates:
            return []
        return list(getattr(candidates[0].content, "parts", []) or [])

    def _part_text(self, part) -> str | None:
        text = getattr(part, "text", None)
        if text is not None:
            return text
        if isinstance(part, dict):
            return part.get("text")
        return None

    def _part_function_call(self, part):
        fc = getattr(part, "function_call", None)
        if fc is not None:
            return fc
        if isinstance(part, dict):
            return part.get("function_call")
        return None

    def _function_call_name_args(self, function_call) -> tuple[str, dict]:
        if isinstance(function_call, dict):
            return str(function_call.get("name") or ""), dict(function_call.get("args") or {})
        return str(getattr(function_call, "name", "") or ""), dict(getattr(function_call, "args", {}) or {})

    def _usage_tokens(self, response) -> int | None:
        usage = getattr(response, "usage_metadata", None)
        return getattr(usage, "total_token_count", None) if usage else None

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        system = context.system_text or None
        contents = self._build_contents(turns_with_state_appended(context))
        config = self._build_config(system, tools, extensions)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                if self._model is not None:
                    resp = await self._model.generate_content_async(contents)
                else:
                    resp = await self._require_client().aio.models.generate_content(
                        model=self._model_name,
                        contents=contents,
                        config=config,
                    )
                self._circuit.record_success()

                content = ""
                tool_calls: list[ToolCall] = []

                for part in self._response_parts(resp):
                    text = self._part_text(part)
                    if text:
                        content += text
                        continue
                    fc = self._part_function_call(part)
                    if fc:
                        name, args = self._function_call_name_args(fc)
                        tc = normalize_tool_call(name, name, args)
                        if tc:
                            tool_calls.append(tc)

                return Message(
                    role="assistant",
                    content=content,
                    token_count=self._usage_tokens(resp),
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
        system = context.system_text or None
        contents = self._build_contents(turns_with_state_appended(context))
        config = self._build_config(system, tools, extensions)

        tool_calls: list[dict] = []
        last_usage = None

        if self._model is not None:
            stream = await self._model.generate_content_async(contents, stream=True)
        else:
            stream = await self._require_client().aio.models.generate_content_stream(
                model=self._model_name,
                contents=contents,
                config=config,
            )

        async for chunk in stream:
            # usage_metadata is cumulative and populated on the final chunk(s).
            if getattr(chunk, "usage_metadata", None):
                last_usage = chunk.usage_metadata
            for part in self._response_parts(chunk):
                text = self._part_text(part)
                if text:
                    yield TextDelta(delta=text)
                    continue
                fc = self._part_function_call(part)
                if fc:
                    name, args = self._function_call_name_args(fc)
                    tool_calls.append({"id": f"call_{len(tool_calls) + 1}", "name": name, "args": args})

        for tc in tool_calls:
            yield ToolCallEvent(id=tc["id"], name=tc["name"], arguments=tc["args"])

        if last_usage is not None:
            # Implicit/explicit cache hits surface as cached_content_token_count,
            # a subset of prompt_token_count (kept as the full prompt count).
            total = getattr(last_usage, "total_token_count", 0) or 0
            if total:
                yield UsageEvent(
                    total_tokens=total,
                    input_tokens=getattr(last_usage, "prompt_token_count", 0) or 0,
                    output_tokens=getattr(last_usage, "candidates_token_count", 0) or 0,
                    cache_read_input_tokens=getattr(last_usage, "cached_content_token_count", 0) or 0,
                )
