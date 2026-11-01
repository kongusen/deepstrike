"""Server-side compaction for the Anthropic provider, in its two forms (mirrors
node/src/providers/anthropic-compaction.ts).

Threshold compaction (``compact_20260112``) rides on every request and the API summarizes once
input reaches the trigger; with ``pause_after_compaction`` the API stops after writing the summary
so the provider can keep the newest turns verbatim behind it. On-demand compaction
(``compact-2026-09-04``) is a separate request the provider sends in the background once a turn's
input passes a limit; the signed block it returns replaces the messages it summarized on every
later request. Either way the kernel sees an ordinary turn: the provider owns the block, and the
kernel's renewal still decides on the full local history.
"""
from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass
from typing import Any, Awaitable, Callable, Literal, TypedDict

THRESHOLD_COMPACTION_BETA = "compact-2026-01-12"
ON_DEMAND_COMPACTION_BETA = "compact-2026-09-04"


class AnthropicCompactionPause(TypedDict):
    keep_recent_messages: int


class AnthropicCompaction(TypedDict, total=False):
    """Compaction settings. ``mode`` is ``"threshold"`` (default) or ``"on_demand"``.

    Threshold: ``trigger_input_tokens`` (at least 50,000; API default 150,000), ``instructions``,
    and ``pause_after_compaction={"keep_recent_messages": n}`` to stop after the summary and keep
    the newest ``n`` history messages verbatim behind it (their thinking blocks are dropped: they
    were produced with the full history in view and would fail the API's prefix check).

    On demand: ``compact_at_input_tokens`` (required) — once a turn's input plus output reaches it,
    the provider asks for a summary of that turn's history in the background and adopts it on the
    first request after it arrives; ``instructions`` (at most 16,384 characters); ``max_tokens``
    for the summarization call, defaulting to the turn's own.
    """
    mode: Literal["threshold", "on_demand"]
    trigger_input_tokens: int
    instructions: str
    pause_after_compaction: AnthropicCompactionPause
    compact_at_input_tokens: int
    max_tokens: int


@dataclass(frozen=True)
class ResolvedCompaction:
    mode: str
    edit: dict | None = None
    keep_recent_messages: int | None = None
    on_demand: dict | None = None


def _positive_int(value: Any, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise TypeError(f"AnthropicProvider compaction.{field} must be a positive integer")
    return value


def resolve_compaction(config: bool | AnthropicCompaction) -> ResolvedCompaction:
    values: dict = {} if config is True else dict(config)
    mode = values.get("mode", "threshold")
    if mode == "on_demand":
        _positive_int(values.get("compact_at_input_tokens"), "compact_at_input_tokens")
        if "max_tokens" in values:
            _positive_int(values["max_tokens"], "max_tokens")
        for field in ("trigger_input_tokens", "pause_after_compaction"):
            if field in values:
                raise TypeError(f"AnthropicProvider compaction.{field} applies to threshold compaction, not on_demand")
        return ResolvedCompaction(mode="on_demand", on_demand=values)
    if mode != "threshold":
        raise TypeError(f'AnthropicProvider compaction.mode must be "threshold" or "on_demand", got {mode!r}')
    pause = values.get("pause_after_compaction")
    keep = None
    if pause is not None:
        keep = pause.get("keep_recent_messages") if isinstance(pause, dict) else None
        if isinstance(keep, bool) or not isinstance(keep, int) or keep < 0:
            raise TypeError("AnthropicProvider compaction.pause_after_compaction.keep_recent_messages must be a non-negative integer")
    edit: dict = {"type": "compact_20260112"}
    if "trigger_input_tokens" in values:
        edit["trigger"] = {"type": "input_tokens", "value": values["trigger_input_tokens"]}
    if pause is not None:
        edit["pause_after_compaction"] = True
    if "instructions" in values:
        edit["instructions"] = values["instructions"]
    return ResolvedCompaction(mode="threshold", edit=edit, keep_recent_messages=keep)


def without_compaction_pause(params: dict) -> dict:
    """The request that continues a paused threshold compaction: the same edits without the pause."""
    management = params.get("context_management")
    if not isinstance(management, dict) or not isinstance(management.get("edits"), list):
        return params
    edits = [
        {key: value for key, value in edit.items() if key != "pause_after_compaction"}
        if isinstance(edit, dict) and edit.get("type") == "compact_20260112" else edit
        for edit in management["edits"]
    ]
    return {**params, "context_management": {**management, "edits": edits}}


def block_dict(block: Any) -> dict:
    """A response block exactly as the API returned it, with no fields it did not send."""
    if isinstance(block, dict):
        return {key: value for key, value in block.items() if value is not None}
    if hasattr(block, "model_dump"):
        return block.model_dump(exclude_none=True)
    return {key: value for key, value in vars(block).items() if value is not None}


def compaction_block(content: Any) -> dict | None:
    for block in content or []:
        kind = block.get("type") if isinstance(block, dict) else getattr(block, "type", None)
        if kind == "compaction":
            return block_dict(block)
    return None


def _content_blocks(message: dict) -> list[dict]:
    content = message.get("content")
    if isinstance(content, str):
        return [{"type": "text", "text": content}] if content else []
    return list(content) if isinstance(content, list) else []


def resume_after_compaction_pause(
    messages: list[dict],
    history_count: int,
    block: dict,
    keep_recent_messages: int | None,
) -> tuple[list[dict], tuple[int, list[dict]] | None]:
    """How a paused threshold compaction continues: the continuation's messages and, when recent
    turns are kept, ``(wire index, new content)`` of the assistant message the block now heads.

    With ``keep_recent_messages`` the block heads the nearest assistant message at or before the
    first kept history message, so the kept turns follow the summary verbatim; without it (or when
    no assistant message qualifies) the block is appended as an assistant turn of its own, as the
    API's pause flow does. ``history_count`` is the number of leading wire messages that are history
    (the volatile state turn follows them)."""
    keep = keep_recent_messages or 0
    if keep > 0:
        index = min(history_count - keep, history_count - 1)
        while index > 0:
            if messages[index].get("role") == "assistant":
                own = [
                    {key: value for key, value in item.items() if key != "cache_control"}
                    for item in _content_blocks(messages[index])
                    if item.get("type") not in ("thinking", "redacted_thinking")
                ]
                content = [block, *own]
                return [{"role": "assistant", "content": content}, *messages[index + 1:]], (index, content)
            index -= 1
    return [*messages, {"role": "assistant", "content": [block]}], None


def _stable_json(value: Any) -> str:
    """Wire values as the API compares them: ``cache_control`` is a request annotation, not
    content, and a string message body is the same content as one text block (a cache marker turns
    the one into the other)."""
    def normalize(item: Any) -> Any:
        if isinstance(item, dict):
            if isinstance(item.get("role"), str) and isinstance(item.get("content"), str):
                item = {**item, "content": _content_blocks(item)}
            return {key: normalize(inner) for key, inner in item.items() if key != "cache_control"}
        if isinstance(item, list):
            return [normalize(inner) for inner in item]
        return item
    return json.dumps(normalize(value), sort_keys=True, ensure_ascii=False)


def on_demand_compaction_body(sent: dict, history_count: int, config: dict) -> dict:
    """The compaction request a turn's request becomes: the same model, system, tools and thinking,
    its history messages only, and none of the fields the API rejects on a summarization call."""
    body = {key: value for key, value in sent.items() if key not in ("stream", "context_management", "stop_sequences")}
    choice = body.get("tool_choice")
    if isinstance(choice, dict) and choice.get("type") in ("any", "tool"):
        del body["tool_choice"]
    output = body.get("output_config")
    if isinstance(output, dict):
        rest = {key: value for key, value in output.items() if key not in ("format", "task_budget")}
        budget = output.get("task_budget")
        if isinstance(budget, dict):
            rest["task_budget"] = {key: value for key, value in budget.items() if key != "remaining"}
        if rest:
            body["output_config"] = rest
        else:
            del body["output_config"]
    betas = [beta for beta in (sent.get("betas") or []) if beta != THRESHOLD_COMPACTION_BETA]
    if ON_DEMAND_COMPACTION_BETA not in betas:
        betas.append(ON_DEMAND_COMPACTION_BETA)
    compaction: dict = {"type": "summarize"}
    if (config.get("instructions") or "").strip():
        compaction["instructions"] = config["instructions"]
    body.update({
        "messages": list(sent["messages"][:history_count]),
        "compaction": compaction,
        "betas": betas,
    })
    if "max_tokens" in config:
        body["max_tokens"] = config["max_tokens"]
    return body


@dataclass
class _Adoption:
    summarized: list[str]
    frame: str
    message: dict


class OnDemandCompactor:
    """Owns on-demand compaction for one provider: decides when to ask for a summary, runs the
    request in the background, and swaps the block in for the messages it summarized. Adoptions
    chain — a later summary covers the earlier block — and every request re-derives the swap from
    the full history the kernel rendered, so a history the summary no longer matches (a renewal, a
    changed system prompt or toolset) is sent whole and the stale adoptions are dropped. State is in
    memory: after a restart the first turn past the limit compacts again."""

    def __init__(self, config: dict, send: Callable[[dict], Awaitable[Any]]):
        self._config = config
        self._send = send
        self._adoptions: list[_Adoption] = []
        self._pending: asyncio.Task | None = None

    def apply(self, params: dict) -> bool:
        """Swap the adopted summaries into a request's messages. Mutates ``params``; returns whether
        a block was swapped in (the request then needs the on-demand beta)."""
        if self._pending is not None and self._pending.done():
            adoption = None if self._pending.cancelled() else self._pending.result()
            if adoption is not None:
                self._adoptions.append(adoption)
            self._pending = None
        if not self._adoptions:
            return False
        frame = _stable_json([params.get("system"), params.get("tools")])
        messages = params["messages"]
        applied = 0
        for adoption in self._adoptions:
            if adoption.frame != frame or len(adoption.summarized) > len(messages):
                break
            if any(_stable_json(messages[i]) != digest for i, digest in enumerate(adoption.summarized)):
                break
            messages = [adoption.message, *messages[len(adoption.summarized):]]
            applied += 1
        del self._adoptions[applied:]
        if not applied:
            return False
        params["messages"] = messages
        betas = list(params.get("betas") or [])
        if ON_DEMAND_COMPACTION_BETA not in betas:
            betas.append(ON_DEMAND_COMPACTION_BETA)
        params["betas"] = betas
        return True

    def observe(self, sent: dict, history_count: int, total_tokens: int) -> None:
        """After a turn completes: once its input plus output reaches the limit, summarize the
        history that turn sent, unless a summary is already being written. Never raises; a summary
        that does not come back leaves the history as it is, and the next turn past the limit asks
        again."""
        if self._pending is not None or total_tokens < self._config["compact_at_input_tokens"] or history_count <= 0:
            return
        body = on_demand_compaction_body(sent, history_count, self._config)
        summarized = [_stable_json(message) for message in body["messages"]]
        frame = _stable_json([body.get("system"), body.get("tools")])

        async def run() -> _Adoption | None:
            try:
                raw = await self._send(body)
            except Exception:
                return None
            stop = raw.get("stop_reason") if isinstance(raw, dict) else getattr(raw, "stop_reason", None)
            content = raw.get("content") if isinstance(raw, dict) else getattr(raw, "content", None)
            block = compaction_block(content) if stop == "compaction" else None
            if block is None:
                return None
            return _Adoption(summarized=summarized, frame=frame, message={"role": "assistant", "content": [block]})

        self._pending = asyncio.get_running_loop().create_task(run())

    async def idle(self) -> None:
        """Returns once any summary in flight has settled. For tests and orderly shutdown."""
        if self._pending is not None:
            await asyncio.wait({self._pending})
