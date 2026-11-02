from __future__ import annotations
from copy import deepcopy
from dataclasses import replace
from .prepared_request import PreparedProviderRequest
import json
import logging
from types import SimpleNamespace
from typing import AsyncIterator, TypedDict
from anthropic import AsyncAnthropic
from deepstrike._kernel import ModelMessage, ToolCall, ToolSchema
from .stream import StreamEvent
from .base import RetryConfig, CircuitBreaker, ProviderDescriptor, RenderedContext, RuntimePolicy, normalize_tool_call, parse_tool_arguments, to_anthropic_content, to_anthropic_messages
from .anthropic_adapter import AnthropicMessagesAdapter
from .anthropic_compaction import (
    THRESHOLD_COMPACTION_BETA,
    AnthropicCompaction,
    OnDemandCompactor,
    compaction_block,
    resolve_compaction,
    resume_after_compaction_pause,
    without_compaction_pause,
)
from deepstrike.types.content import normalize_canonical_adapter_input

logger = logging.getLogger(__name__)
_OFFICIAL_ANTHROPIC_BASE_URL = "https://api.anthropic.com"
_CONTEXT_MANAGEMENT_BETA = "context-management-2025-06-27"


class AnthropicContextEditing(TypedDict, total=False):
    trigger_input_tokens: int
    keep_tool_uses: int
    clear_at_least_input_tokens: int
    exclude_tools: list[str]
    clear_tool_inputs: bool


def _clear_tool_uses_edit(config: bool | AnthropicContextEditing) -> dict:
    values = {} if config is True else config
    edit: dict = {"type": "clear_tool_uses_20250919"}
    if "trigger_input_tokens" in values:
        edit["trigger"] = {"type": "input_tokens", "value": values["trigger_input_tokens"]}
    if "keep_tool_uses" in values:
        edit["keep"] = {"type": "tool_uses", "value": values["keep_tool_uses"]}
    if "clear_at_least_input_tokens" in values:
        edit["clear_at_least"] = {"type": "input_tokens", "value": values["clear_at_least_input_tokens"]}
    if values.get("exclude_tools"):
        edit["exclude_tools"] = list(values["exclude_tools"])
    if "clear_tool_inputs" in values:
        edit["clear_tool_inputs"] = values["clear_tool_inputs"]
    return edit


def _native_context_extensions(
    configured: list[tuple[str, dict]],
    extensions: dict | None,
) -> dict:
    """The native edits ride on every request, beside any betas or edits the caller passed: the
    kernel stopped running those tiers on the strength of them."""
    if not configured:
        return {}
    caller = dict(extensions or {})
    betas = list(caller.get("betas", [])) if isinstance(caller.get("betas"), list) else []
    management = caller.get("context_management")
    management = dict(management) if isinstance(management, dict) else {}
    edits = list(management.get("edits", [])) if isinstance(management.get("edits"), list) else []
    for beta, edit in configured:
        if beta not in betas:
            betas.append(beta)
        if not any(isinstance(held, dict) and held.get("type") == edit["type"] for held in edits):
            edits.append(edit)
    management["edits"] = edits
    return {"betas": betas, "context_management": management}


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
        auth_mode: str = "api_key",
        context_editing: bool | AnthropicContextEditing = False,
        compaction: bool | AnthropicCompaction = False,
    ):
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        client_kwargs: dict = ({"auth_token": api_key} if auth_mode == "bearer" else {"api_key": api_key})
        if base_url:
            client_kwargs["base_url"] = base_url
        self._client = AsyncAnthropic(**client_kwargs)
        self._native_assistant_blocks: dict[str, list[dict]] = {}
        self._adapter = AnthropicMessagesAdapter(model)
        self._direct_native_token_counting = (
            base_url is None or base_url.rstrip("/") == _OFFICIAL_ANTHROPIC_BASE_URL
        )
        self._default_textual_tool_call_policy = (
            "off"
            if base_url is None or base_url.rstrip("/") == _OFFICIAL_ANTHROPIC_BASE_URL
            else "reject"
        )
        # Clearing runs first so the compaction trigger reads the input after tool results are cleared.
        # Compaction is summarized server-side; set its trigger below the kernel's renewal threshold,
        # since renewal stays the safety net.
        self._native_context_edits: list[tuple[str, dict]] = []
        if context_editing:
            self._native_context_edits.append((_CONTEXT_MANAGEMENT_BETA, _clear_tool_uses_edit(context_editing)))
        # Threshold compaction with a pause: how many history messages stay verbatim behind the block.
        self._compaction_pause_keep: int | None = None
        self._on_demand: OnDemandCompactor | None = None
        resolved_compaction = resolve_compaction(compaction) if compaction else None
        if resolved_compaction is not None and resolved_compaction.mode == "threshold":
            self._native_context_edits.append((THRESHOLD_COMPACTION_BETA, resolved_compaction.edit))
            if resolved_compaction.edit.get("pause_after_compaction"):
                self._compaction_pause_keep = resolved_compaction.keep_recent_messages or 0
        elif resolved_compaction is not None:
            self._on_demand = OnDemandCompactor(
                resolved_compaction.on_demand,
                lambda body: self._client.beta.messages.create(**body),
            )

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
            native_context={
                **({"tool_result_clearing": True} if any(edit["type"] == "clear_tool_uses_20250919" for _, edit in self._native_context_edits) else {}),
                **({"compaction": True} if self._on_demand is not None or any(edit["type"] == "compact_20260112" for _, edit in self._native_context_edits) else {}),
            },
        )

    def peek_provider_replay(self, content: str, tool_calls: list[ToolCall]) -> dict | None:
        blocks = self._native_assistant_blocks.get(self._assistant_replay_key_parts(content, tool_calls))
        return {"protocol": "anthropic-messages", "native_blocks": blocks} if blocks else None

    def seed_provider_replay(self, content: str, tool_calls: list[ToolCall], replay: dict) -> None:
        blocks = replay.get("native_blocks")
        if replay.get("protocol") == "anthropic-messages" and blocks:
            self._native_assistant_blocks[self._assistant_replay_key_parts(content, tool_calls)] = blocks

    def _build_system(self, context: RenderedContext, strategy: str = "default", cache_control: dict | None = None):
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
        cc = {"cache_control": dict(cache_control or {"type": "ephemeral"})} if emit else {}
        blocks: list[dict] = []
        if stable:
            blocks.append({"type": "text", "text": stable, **cc})
        if knowledge:
            blocks.append({"type": "text", "text": knowledge, **cc})
        return blocks or None

    def _build_messages(self, turns: list[ModelMessage], state_turn=None, frozen_prefix_len=None, strategy: str = "default", cache_control: dict | None = None) -> list[dict]:
        return self._build_wire_messages(turns, state_turn, frozen_prefix_len, strategy, cache_control)[0]

    def _build_wire_messages(self, turns: list[ModelMessage], state_turn=None, frozen_prefix_len=None, strategy: str = "default", cache_control: dict | None = None) -> tuple[list[dict], list[int]]:
        """The wire messages and, for each, the index of the turn it came from (``-1`` for the
        state turn, which always comes last)."""
        def native(message):
            return self._native_assistant_blocks.get(self._assistant_replay_key(message))

        resolved = getattr(self, "_resolved_runtime", None)
        msgs: list[dict] = []
        wire_turns: list[int] = []
        for index, turn in enumerate(turns):
            for message in to_anthropic_messages([turn], native_replay=native, resolved=resolved):
                msgs.append(message)
                wire_turns.append(index)
        # The API ignores everything before a compaction block, so it is not sent.
        head = next(
            (i for i in range(len(msgs) - 1, -1, -1)
             if isinstance(msgs[i].get("content"), list)
             and any(isinstance(b, dict) and b.get("type") == "compaction" for b in msgs[i]["content"])),
            -1,
        )
        if head > 0:
            msgs, wire_turns = msgs[head:], wire_turns[head:]
            if isinstance(frozen_prefix_len, int):
                frozen_prefix_len -= head
        # Cache breakpoints anchor on the stable history; the volatile State turn
        # is appended AFTER them as the uncached tail. On un-rebuilt bindings
        # state_turn is None and the state is already inside turns (rendered above).
        # frozen_prefix_len (P1-E) pins the deep breakpoint at the compaction
        # boundary; None ⇒ rolling-pair fallback.
        _apply_message_cache_control(msgs, frozen_prefix_len, strategy, cache_control)
        if state_turn is not None:
            # Render through to_anthropic_messages so assistant tool_use blocks
            # and tool-role tool_result parts are serialized correctly —
            # to_anthropic_content only handles contentParts/content and would
            # silently drop tool_calls.
            state_msgs = to_anthropic_messages([state_turn], native_replay=native, resolved=resolved)
            msgs.extend(state_msgs)
            wire_turns.extend(-1 for _ in state_msgs)
        return msgs, wire_turns

    def _build_tools(self, tools: list[ToolSchema], anchor_cache: bool, strategy: str = "default", cache_control: dict | None = None) -> list[dict] | None:
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
            defs[-1]["cache_control"] = dict(cache_control or {"type": "ephemeral"})
        return defs

    def _remember_native_blocks(self, content: str, tool_calls: list[ToolCall], blocks: list[dict]) -> None:
        if not blocks:
            return
        if not tool_calls and not any(b.get("type") in ("thinking", "compaction") for b in blocks):
            return
        self._native_assistant_blocks[self._assistant_replay_key_parts(content, tool_calls)] = blocks

    def _build_request_plan(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None):
        strategy = _resolve_cache_breakpoint_strategy(extensions)
        cc = _resolve_cache_control(extensions)
        msgs, wire_turns = self._build_wire_messages(context.turns, context.state_turn, context.frozen_prefix_len, strategy, cc)
        system = self._build_system(context, strategy, cc)
        tool_defs = self._build_tools(tools, anchor_cache=not isinstance(system, list), strategy=strategy, cache_control=cc)
        _assert_cache_budget(system, len(tools))
        adapter_extensions = {
            "textualToolCallPolicy": self._default_textual_tool_call_policy,
            **(extensions or {}),
            **_native_context_extensions(self._native_context_edits, extensions),
        }
        canonical = normalize_canonical_adapter_input(
            context, tools, extensions=adapter_extensions,
            resolved=getattr(self, "_resolved_runtime", None),
        )
        plan = self._adapter.build_request(canonical, messages=msgs, system=system, tools=tool_defs)
        plan = replace(plan, wire_turns=wire_turns)
        if self._on_demand is not None and self._on_demand.apply(plan.params):
            plan = replace(plan, transport="beta")
        return canonical, plan

    async def compaction_idle(self) -> None:
        """On-demand compaction in flight settles; for tests and orderly shutdown."""
        if self._on_demand is not None:
            await self._on_demand.idle()

    def _resume_compaction_pause(self, context: RenderedContext, plan, block: dict) -> tuple[dict, bool]:
        """The continuation of a paused threshold compaction. When recent turns are kept, the block
        now heads an earlier assistant turn, and that turn's replay is rewritten so every later
        request renders the same messages the continuation sent."""
        messages, anchor = resume_after_compaction_pause(
            plan.params["messages"], _history_count(plan), block, self._compaction_pause_keep,
        )
        if anchor is not None:
            wire_index, content = anchor
            turn_index = plan.wire_turns[wire_index]
            if 0 <= turn_index < len(context.turns):
                self._native_assistant_blocks[self._assistant_replay_key(context.turns[turn_index])] = content
        return {**without_compaction_pause(plan.params), "messages": messages}, anchor is not None

    def _observe_usage(self, plan, total_tokens: int) -> None:
        if self._on_demand is not None:
            self._on_demand.observe(plan.params, _history_count(plan), total_tokens)

    async def count_tokens(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None):
        return await self.prepare_request(context, tools, extensions).count_tokens()

    async def _count_prepared(self, plan):
        runtime = getattr(self, "_resolved_runtime", None)
        enabled = (runtime.effective_capabilities.native_token_counting.state == "supported"
                   if runtime is not None else self._direct_native_token_counting)
        if not enabled:
            raise RuntimeError("Native token counting is unavailable on this Anthropic-compatible endpoint")
        params = {
            key: deepcopy(plan.params[key])
            for key in ("model", "system", "messages", "tools")
            if key in plan.params
        }
        # A history that carries a compaction block is only readable under its beta.
        if plan.transport == "beta" and plan.params.get("betas"):
            response = await self._client.beta.messages.count_tokens(**params, betas=list(plan.params["betas"]))
        else:
            response = await self._client.messages.count_tokens(**params)
        return SimpleNamespace(
            input_tokens=response.input_tokens,
            source={"kind": "native", "provider": "anthropic"},
            confidence="exact",
        )

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> ModelMessage:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        canonical, plan = self._build_request_plan(context, tools, extensions)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                messages = self._client.beta.messages if plan.transport == "beta" else self._client.messages
                resp = await messages.create(**plan.params)
                paused = (
                    compaction_block(_field(resp, "content"))
                    if self._compaction_pause_keep is not None and _field(resp, "stop_reason") == "compaction"
                    else None
                )
                unanchored: list[dict] = []
                if paused is not None:
                    params, anchored = self._resume_compaction_pause(context, plan, paused)
                    if not anchored:
                        unanchored = [paused]
                    resp = await self._client.beta.messages.create(**params)
                self._circuit.record_success()
                message, replay = self._adapter.decode_complete(resp, canonical)
                blocks = [*unanchored, *((replay or {}).get("native_blocks") or [])]
                if blocks:
                    self._remember_native_blocks(message.content, message.tool_calls or [], blocks)
                usage = self._adapter.normalize_usage(_field(resp, "usage"))
                if usage is not None:
                    self._observe_usage(plan, usage.input_tokens + usage.output_tokens)
                return message
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    import asyncio
                    delay = self._retry.base_delay * (2 ** attempt)
                    await asyncio.sleep(delay)

        raise last_exc or RuntimeError("Complete failed")

    async def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        async for event in self.prepare_request(context, tools, extensions, state).stream():
            yield event

    def prepare_request(self, context, tools, extensions=None, state=None):
        canonical, plan = self._build_request_plan(context, tools, extensions)
        plan = deepcopy(plan)
        return PreparedProviderRequest("encoded_body", deepcopy(plan.params), deepcopy(state),
            lambda: self._stream_prepared(context, canonical, plan), lambda: self._count_prepared(plan))

    async def _stream_prepared(self, context, canonical, plan):
        stream_state = self._adapter.create_stream_state(canonical, plan.cache_slots)
        messages = self._client.beta.messages if plan.transport == "beta" else self._client.messages
        # With a compaction pause configured, a response that opens with a compaction block is held
        # back: if it stops there, it is the pause, and the continuation's events are the turn's.
        held: list | None = [] if self._compaction_pause_keep is not None else None
        paused = False
        async with messages.stream(**deepcopy(plan.params)) as stream:
            async for event in stream:
                outputs = self._adapter.push_stream_chunk(event, stream_state).events
                kind = _field(event, "type")
                if held is not None and kind == "content_block_start" and _field(_field(event, "content_block"), "type") != "compaction":
                    for output in held:
                        yield output
                    held = None
                if held is not None and kind == "message_delta" and _field(_field(event, "delta"), "stop_reason") == "compaction":
                    paused = True
                if held is not None:
                    held.extend(outputs)
                else:
                    for output in outputs:
                        yield output
        paused_block = next(
            (dict(block) for block in stream_state.native_blocks.values() if block.get("type") == "compaction"),
            None,
        ) if paused else None
        unanchored: list[dict] = []
        if paused_block is not None:
            params, anchored = self._resume_compaction_pause(context, plan, paused_block)
            if not anchored:
                unanchored = [paused_block]
            stream_state = self._adapter.create_stream_state(canonical, plan.cache_slots)
            async with self._client.beta.messages.stream(**deepcopy(params)) as stream:
                async for event in stream:
                    for output in self._adapter.push_stream_chunk(event, stream_state).events:
                        yield output
        elif held:
            for output in held:
                yield output
        finalized = self._adapter.finish_stream(stream_state)
        for event in finalized.events:
            yield event
        blocks = [*unanchored, *((finalized.replay or {}).get("native_blocks") or [])]
        if blocks:
            self._remember_native_blocks(stream_state.final_text, stream_state.final_tool_calls, blocks)
        self._observe_usage(
            plan,
            stream_state.uncached_input + stream_state.cache_read + stream_state.cache_creation + stream_state.output_tokens,
        )

    def _assistant_replay_key(self, message: ModelMessage) -> str:
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


# Control extensions that shape caching but must never be echoed as wire params.
_CACHE_CONTROL_EXTENSION_KEYS = ("cacheBreakpointStrategy", "cacheTtl")


def _resolve_cache_control(extensions: "dict | None") -> dict:
    """The cache_control marker applied to every breakpoint this request. `cacheTtl="1h"` opts into
    Anthropic's extended 1-hour cache (GA on the first-party Claude API — no beta header); anything
    else is the default 5-minute ephemeral. A single TTL for the whole request sidesteps the
    "1h breakpoints must precede 5m" ordering rule."""
    if (extensions or {}).get("cacheTtl") == "1h":
        return {"type": "ephemeral", "ttl": "1h"}
    return {"type": "ephemeral"}


def _count_cache_control_slots(system, tool_defs, msgs) -> dict:
    """I1: which slots of the outgoing request carry a ``cache_control`` breakpoint. Mirrors Node."""
    sys_bp = isinstance(system, list) and any((isinstance(b, dict) and b.get("cache_control") is not None) for b in system)
    tool_bp = bool(tool_defs) and any((isinstance(t, dict) and t.get("cache_control") is not None) for t in tool_defs)
    msg_bp = False
    for m in msgs or []:
        content = m.get("content") if isinstance(m, dict) else None
        if isinstance(content, list):
            if any((isinstance(b, dict) and b.get("cache_control") is not None) for b in content):
                msg_bp = True
                break
    return {"system": sys_bp, "tools": tool_bp, "messages": msg_bp}


def _apply_message_cache_control(msgs: list[dict], frozen_prefix_len: "int | None" = None, strategy: str = "default", cache_control: dict | None = None) -> None:
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
    # ModelMessage-level cache_control is emitted under "default" and "frozen-prefix" only.
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
        _mark_last_block_cacheable(msgs[idx], cache_control)


def _mark_last_block_cacheable(msg: dict, cache_control: dict | None = None) -> None:
    cache_control = dict(cache_control or {"type": "ephemeral"})
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


def _history_count(plan) -> int:
    """The leading wire messages that are history; the state turn and placeholder follow them."""
    return sum(1 for index in plan.wire_turns if index >= 0)


def _field(raw: object, key: str) -> object:
    return raw.get(key) if isinstance(raw, dict) else getattr(raw, key, None)


def _tc_field(tc: object, field_name: str) -> object:
    if isinstance(tc, dict):
        return tc.get(field_name)
    return getattr(tc, field_name, None)
