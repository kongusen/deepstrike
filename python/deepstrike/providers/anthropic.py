from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from anthropic import AsyncAnthropic
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent
from .base import RetryConfig, CircuitBreaker, ProviderDescriptor, RenderedContext, RuntimePolicy, normalize_tool_call, parse_tool_arguments, to_anthropic_content, to_anthropic_messages

logger = logging.getLogger(__name__)

_CLAUDE_POLICIES: dict[str, RuntimePolicy] = {
    "claude-opus-4-1":           RuntimePolicy(max_turns=50),
    "claude-opus-4-7":           RuntimePolicy(max_turns=50),
    "claude-opus-4-6":           RuntimePolicy(max_turns=50),
    "claude-opus-4-0":           RuntimePolicy(max_turns=50),
    "claude-sonnet-4-6":         RuntimePolicy(max_turns=25),
    "claude-sonnet-4-0":         RuntimePolicy(max_turns=25),
    "claude-haiku-4-5":          RuntimePolicy(max_turns=15),
    "claude-3-5-haiku-latest":   RuntimePolicy(max_turns=15),
}


class AnthropicProvider:
    def __init__(
        self,
        api_key: str,
        model: str = "claude-sonnet-4-6",
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        client_kwargs: dict = {"api_key": api_key}
        if base_url:
            client_kwargs["base_url"] = base_url
        self._client = AsyncAnthropic(**client_kwargs)
        self._native_assistant_blocks: dict[str, list[dict]] = {}

    def runtime_policy(self) -> RuntimePolicy:
        return _CLAUDE_POLICIES.get(self._model, RuntimePolicy())

    def _provider_name(self) -> str:
        """Identity advertised in the descriptor; overridden by Anthropic-compatible vendors (e.g. MiniMax)."""
        return "anthropic"

    def descriptor(self) -> ProviderDescriptor:
        return ProviderDescriptor(
            provider=self._provider_name(),
            protocol="anthropic-messages",
            model=self._model,
            reasoning={"supported": True, "preserve_across_tool_turns": True, "requires_replay_for_tool_turns": True},
            tool_calls={"supported": True, "requires_strict_pairing": True},
        )

    def peek_provider_replay(self, content: str, tool_calls: list[ToolCall]) -> dict | None:
        blocks = self._native_assistant_blocks.get(self._assistant_replay_key_parts(content, tool_calls))
        return {"native_blocks": blocks} if blocks else None

    def seed_provider_replay(self, content: str, tool_calls: list[ToolCall], replay: dict) -> None:
        blocks = replay.get("native_blocks")
        if blocks:
            self._native_assistant_blocks[self._assistant_replay_key_parts(content, tool_calls)] = blocks
            return
        # Legacy log without persisted native blocks: reconstruct neutral
        # text + tool_use blocks from the transcript so a tool-use turn can be
        # replayed. Thinking blocks were never persisted and are not recovered.
        reconstructed = _reconstruct_anthropic_blocks(content, tool_calls)
        if reconstructed:
            self._native_assistant_blocks[self._assistant_replay_key_parts(content, tool_calls)] = reconstructed

    def _build_system(self, context: RenderedContext):
        """Structured system blocks with cache_control when the kernel partitioned
        the prompt (system_stable / system_knowledge); else the flat system_text
        string (no cache breakpoint), or None."""
        stable = getattr(context, "system_stable", "") or ""
        knowledge = getattr(context, "system_knowledge", "") or ""
        if not stable and not knowledge:
            return context.system_text or None
        # System cache_control is emitted under "default" and "system-only". Other strategies keep
        # the text-block structure for protocol parity but omit cache_control.
        emit = strategy in ("default", "system-only")
        cc = {"cache_control": {"type": "ephemeral"}} if emit else {}
        blocks: list[dict] = []
        if stable:
            blocks.append({"type": "text", "text": stable, **cc})
        if knowledge:
            blocks.append({"type": "text", "text": knowledge, **cc})
        return blocks or None

    def _build_messages(self, turns: list[Message], state_turn=None, frozen_prefix_len=None, strategy: str = "default") -> list[dict]:
        msgs = to_anthropic_messages(
            turns,
            native_replay=lambda message: self._native_assistant_blocks.get(
                self._assistant_replay_key(message)
            ),
        )
        # Cache breakpoints anchor on the stable history; the volatile State turn
        # is appended AFTER them as the uncached tail. On un-rebuilt bindings
        # state_turn is None and the state is already inside turns (rendered above).
        # frozen_prefix_len (P1-E) pins the deep breakpoint at the compaction
        # boundary; None ⇒ rolling-pair fallback.
        _apply_message_cache_control(msgs, frozen_prefix_len, strategy)
        if state_turn is not None:
            # Render through to_anthropic_messages so assistant tool_use blocks
            # and tool-role tool_result parts are serialized correctly —
            # to_anthropic_content only handles contentParts/content and would
            # silently drop tool_calls.
            state_msgs = to_anthropic_messages(
                [state_turn],
                native_replay=lambda message: self._native_assistant_blocks.get(
                    self._assistant_replay_key(message)
                ),
            )
            msgs.extend(state_msgs)
        return msgs

    def _build_tools(self, tools: list[ToolSchema], anchor_cache: bool, strategy: str = "default") -> list[dict] | None:
        if not tools:
            return None
        defs = [
            {
                "name": t.name,
                "description": t.description,
                "input_schema": json.loads(t.parameters),
            }
            for t in tools
        ]
        # Tool cache_control is emitted under "default" and "tools-only".
        emit_on_last_tool = anchor_cache and strategy in ("default", "tools-only")
        if emit_on_last_tool:
            defs[-1]["cache_control"] = {"type": "ephemeral"}
        return defs

    def _remember_native_blocks(self, content: str, tool_calls: list[ToolCall], blocks: list[dict]) -> None:
        if not blocks:
            return
        if not tool_calls and not any(b.get("type") == "thinking" for b in blocks):
            return
        self._native_assistant_blocks[self._assistant_replay_key_parts(content, tool_calls)] = blocks

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        strategy = _resolve_cache_breakpoint_strategy(extensions)
        msgs = self._build_messages(context.turns, context.state_turn, context.frozen_prefix_len, strategy)
        system = self._build_system(context, strategy)
        tool_defs = self._build_tools(tools, anchor_cache=not isinstance(system, list), strategy=strategy)
        _assert_cache_budget(system, len(tools))

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                request_extensions = {k: v for k, v in (extensions or {}).items() if k not in {"model", "messages", "system", "tools", "stream", "max_tokens"}}
                resp = await self._client.messages.create(
                    **request_extensions,
                    model=self._model,
                    max_tokens=(extensions or {}).get("max_tokens", 8096),
                    system=system,
                    messages=msgs,
                    tools=tool_defs,
                )
                self._circuit.record_success()

                content = ""
                tool_calls: list[ToolCall] = []
                native_blocks: list[dict] = []

                for block in resp.content:
                    if block.type == "text":
                        content += block.text
                        native_blocks.append({"type": "text", "text": block.text})
                    elif block.type == "tool_use":
                        tc = normalize_tool_call(block.id, block.name, block.input)
                        if tc:
                            tool_calls.append(tc)
                        native_blocks.append({
                            "type": "tool_use",
                            "id": block.id,
                            "name": block.name,
                            "input": block.input,
                        })
                    elif block.type == "thinking":
                        native_blocks.append({
                            "type": "thinking",
                            "thinking": block.thinking,
                            "signature": getattr(block, "signature", None),
                        })

                self._remember_native_blocks(content, tool_calls, native_blocks)

                # token_count is the turn total: full prompt (uncached + cache
                # read + cache write) + output, so it stays accurate once caching
                # moves most of the prompt into cache_read.
                usage = resp.usage
                full_input = (
                    (usage.input_tokens or 0)
                    + (getattr(usage, "cache_read_input_tokens", 0) or 0)
                    + (getattr(usage, "cache_creation_input_tokens", 0) or 0)
                )
                return Message(
                    role="assistant",
                    content=content,
                    token_count=full_input + (usage.output_tokens or 0),
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
        strategy = _resolve_cache_breakpoint_strategy(extensions)
        msgs = self._build_messages(context.turns, context.state_turn, context.frozen_prefix_len, strategy)
        system = self._build_system(context, strategy)
        tool_defs = self._build_tools(tools, anchor_cache=not isinstance(system, list), strategy=strategy)
        _assert_cache_budget(system, len(tools))

        native_blocks: dict[int, dict] = {}
        tool_blocks: dict[int, dict] = {}
        final_text = ""
        final_tool_calls: list[ToolCall] = []
        uncached_input = 0
        cache_read = 0
        cache_creation = 0
        output_tokens = 0

        request_extensions = {k: v for k, v in (extensions or {}).items() if k not in {"model", "messages", "system", "tools", "stream", "max_tokens"}}
        async with self._client.messages.stream(
            **request_extensions,
            model=self._model,
            max_tokens=(extensions or {}).get("max_tokens", 8096),
            system=system,
            messages=msgs,
            tools=tool_defs,
        ) as stream:
            async for event in stream:
                if event.type in ("message_start", "message_delta"):
                    usage = getattr(event, "usage", None) or getattr(getattr(event, "message", None), "usage", None)
                    if usage is not None:
                        # input + cache counts are pinned at message_start; a later
                        # message_delta may omit them — max() prevents zeroing.
                        uncached_input = max(uncached_input, getattr(usage, "input_tokens", 0) or 0)
                        cache_read = max(cache_read, getattr(usage, "cache_read_input_tokens", 0) or 0)
                        cache_creation = max(cache_creation, getattr(usage, "cache_creation_input_tokens", 0) or 0)
                        output_tokens = max(output_tokens, getattr(usage, "output_tokens", 0) or 0)
                        # input_tokens is the FULL prompt size (uncached + cache
                        # read + cache write) for accurate context accounting.
                        full_input = uncached_input + cache_read + cache_creation
                        yield UsageEvent(
                            total_tokens=full_input + output_tokens,
                            input_tokens=full_input,
                            output_tokens=output_tokens,
                            cache_read_input_tokens=cache_read,
                            cache_creation_input_tokens=cache_creation,
                        )
                elif event.type == "content_block_start":
                    idx = event.index
                    block = event.content_block
                    native_blocks[idx] = {"type": block.type}
                    if block.type == "thinking":
                        native_blocks[idx]["thinking"] = getattr(block, "thinking", "") or ""
                        native_blocks[idx]["signature"] = getattr(block, "signature", "") or ""
                    elif block.type == "text":
                        native_blocks[idx]["text"] = getattr(block, "text", "") or ""
                    elif block.type == "tool_use":
                        native_blocks[idx].update({
                            "id": block.id,
                            "name": block.name,
                            "input": getattr(block, "input", {}) or {},
                        })
                        tool_blocks[idx] = {"id": block.id, "name": block.name, "args_buf": ""}
                elif event.type == "content_block_delta":
                    delta = event.delta
                    idx = event.index
                    if delta.type == "text_delta":
                        final_text += delta.text
                        native_blocks[idx]["text"] = native_blocks[idx].get("text", "") + delta.text
                        yield TextDelta(delta=delta.text)
                    elif delta.type == "thinking_delta":
                        native_blocks[idx]["thinking"] = native_blocks[idx].get("thinking", "") + delta.thinking
                        yield ThinkingDelta(delta=delta.thinking)
                    elif delta.type == "signature_delta":
                        native_blocks[idx]["signature"] = native_blocks[idx].get("signature", "") + delta.signature
                    elif delta.type == "input_json_delta" and idx in tool_blocks:
                        tool_blocks[idx]["args_buf"] += delta.partial_json
                elif event.type == "content_block_stop":
                    idx = event.index
                    if idx in tool_blocks:
                        tb = tool_blocks.pop(idx)
                        try:
                            args = json.loads(tb["args_buf"] or "{}")
                        except json.JSONDecodeError:
                            args = {}
                        native_blocks[idx]["input"] = args
                        tc = normalize_tool_call(tb["id"], tb["name"], args)
                        if tc:
                            final_tool_calls.append(tc)
                            yield ToolCallEvent(id=tc.id, name=tc.name, arguments=args)

        ordered_blocks = [native_blocks[i] for i in sorted(native_blocks)]
        self._remember_native_blocks(final_text, final_tool_calls, ordered_blocks)

    def _assistant_replay_key(self, message: Message) -> str:
        return self._assistant_replay_key_parts(message.content, message.tool_calls or [])

    def _assistant_replay_key_parts(self, content: str, tool_calls: list[ToolCall]) -> str:
        return json.dumps({
            "content": content,
            "tool_calls": [
                {"id": _tc_field(tc, "id"), "name": _tc_field(tc, "name"), "arguments": _tc_field(tc, "arguments")}
                for tc in tool_calls
            ],
        }, sort_keys=True)


# Anthropic accepts at most 4 cache_control breakpoints per request; the static
# system/tools prefix uses up to 2, leaving 2 for the rolling message history.
_MAX_CACHE_BREAKPOINTS = 4
_MESSAGE_CACHE_BREAKPOINTS = 2

# Recognised values for the `cacheBreakpointStrategy` extension. See node/src/types.ts
# `CacheBreakpointStrategy` for the canonical documentation; this mirrors the same surface.
_CACHE_BREAKPOINT_STRATEGIES = {"default", "tools-only", "system-only", "frozen-prefix", "none"}


def _resolve_cache_breakpoint_strategy(extensions: "dict | None") -> str:
    """Pull `cacheBreakpointStrategy` from per-call extensions; unrecognised values → 'default'."""
    if not extensions:
        return "default"
    raw = extensions.get("cacheBreakpointStrategy")
    if isinstance(raw, str) and raw in _CACHE_BREAKPOINT_STRATEGIES:
        return raw
    return "default"


def _apply_message_cache_control(msgs: list[dict], frozen_prefix_len: "int | None" = None, strategy: str = "default") -> None:
    """Place the (<=2) message-history cache breakpoints. The final message always
    gets one (writes the current full prefix). The second is placed by one of two
    strategies:

      * Deep anchor (P1-E): when ``frozen_prefix_len`` marks a distinct frozen prefix
        (the compaction boundary), pin the second breakpoint there. It is byte-stable
        across turns, so ``[0..frozen]`` is re-read cheaply every turn and is immune to
        the 20-block lookback miss on heavy tool turns; the tail then writes only the
        incremental ``[frozen..tail]``.
      * Rolling fallback: otherwise mark the nearest preceding user turn (the previous
        turn's read anchor); Anthropic's 20-block lookback bridges light turns.

    Without this the cached prefix stops at ``system`` and the whole tool-result history
    is re-billed at full price every turn (~quadratic cumulative cost). A bare string
    body is promoted to a cache-bearing text block."""
    if not msgs:
        return
    # Message-level cache_control is emitted under "default" and "frozen-prefix" only.
    if strategy in ("tools-only", "system-only", "none"):
        return
    targets = {len(msgs) - 1}
    if isinstance(frozen_prefix_len, int) and 1 <= frozen_prefix_len < len(msgs):
        # Deep anchor at the frozen-prefix boundary (last frozen turn). Fixed between compactions.
        targets.add(frozen_prefix_len - 1)
    elif strategy == "default":
        i = len(msgs) - 2
        while i >= 0 and len(targets) < _MESSAGE_CACHE_BREAKPOINTS:
            if msgs[i].get("role") == "user":
                targets.add(i)
            i -= 1
    for idx in targets:
        _mark_last_block_cacheable(msgs[idx])


def _mark_last_block_cacheable(msg: dict) -> None:
    cache_control = {"type": "ephemeral"}
    content = msg.get("content")
    if isinstance(content, str):
        if not content:
            return  # don't synthesize an empty (API-rejected) text block
        msg["content"] = [{"type": "text", "text": content, "cache_control": cache_control}]
        return
    if isinstance(content, list) and content:
        content[-1]["cache_control"] = cache_control


def _assert_cache_budget(system: object, tool_count: int) -> None:
    """Regression guard: fail loudly before the API would reject the request for
    exceeding the cache_control breakpoint limit."""
    system_breakpoints = len(system) if isinstance(system, list) else 0
    tool_breakpoints = 1 if tool_count > 0 and not isinstance(system, list) else 0
    worst_case = system_breakpoints + tool_breakpoints + _MESSAGE_CACHE_BREAKPOINTS
    if worst_case > _MAX_CACHE_BREAKPOINTS:
        raise ValueError(
            f"Anthropic cache_control budget exceeded: {system_breakpoints} system + "
            f"{tool_breakpoints} tool + {_MESSAGE_CACHE_BREAKPOINTS} message > {_MAX_CACHE_BREAKPOINTS}"
        )


def _tc_field(tc: object, field_name: str) -> object:
    if isinstance(tc, dict):
        return tc.get(field_name)
    return getattr(tc, field_name, None)


def _reconstruct_anthropic_blocks(content: str, tool_calls: list) -> list[dict]:
    """Reconstruct Anthropic assistant content blocks from a neutral transcript
    when no provider replay was persisted. Only meaningful for tool-use turns."""
    if not tool_calls:
        return []
    blocks: list[dict] = []
    if content:
        blocks.append({"type": "text", "text": content})
    for tc in tool_calls:
        blocks.append({
            "type": "tool_use",
            "id": _tc_field(tc, "id"),
            "name": _tc_field(tc, "name"),
            "input": parse_tool_arguments(_tc_field(tc, "arguments")),
        })
    return blocks
