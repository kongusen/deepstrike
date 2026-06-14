from __future__ import annotations

import json
from typing import Any

from deepstrike._kernel import Message, ToolCall

from .base import RenderedContext
from .replay_validator import DEGRADED_REASONING_PLACEHOLDER, assess_reasoning_replay


def _sort_dict_keys(val: Any) -> Any:
    if isinstance(val, dict):
        return {k: _sort_dict_keys(val[k]) for k in sorted(val.keys())}
    if isinstance(val, list):
        return [_sort_dict_keys(item) for item in val]
    return val


def assistant_replay_key(content: str, tool_calls: list[ToolCall]) -> str:
    normalized_calls = []
    for tc in tool_calls:
        args = tc.arguments
        if isinstance(args, str):
            try:
                parsed = json.loads(args)
                args = json.dumps(_sort_dict_keys(parsed), separators=(',', ':'))
            except Exception:
                pass
        elif isinstance(args, (dict, list)):
            args = json.dumps(_sort_dict_keys(args), separators=(',', ':'))
        normalized_calls.append({
            "id": tc.id,
            "name": tc.name,
            "arguments": args
        })
    return json.dumps({
        "content": content,
        "tool_calls": normalized_calls,
    }, sort_keys=True)


def openai_chat_wire_replay_fields(replay: dict[str, Any] | None) -> dict[str, Any]:
    """Only wire-safe reasoning fields may be merged into an OpenAI-compatible
    request message. Envelope metadata (schema_version / provider / protocol /
    model / native_message / tool_calls) is recovery bookkeeping and must never
    be sent to the provider."""
    if not replay:
        return {}
    fields: dict[str, Any] = {}
    if isinstance(replay.get("reasoning_content"), str):
        fields["reasoning_content"] = replay["reasoning_content"]
    if replay.get("reasoning_details") is not None:
        fields["reasoning_details"] = replay["reasoning_details"]
    return fields


class ReasoningReplayMixin:
    """OpenAI-compatible providers: persist reasoning_content / reasoning_details
    across tool turns as a provider-scoped replay envelope."""

    _replay_fields: dict[str, dict[str, Any]]

    def _init_replay_store(self) -> None:
        self._replay_fields = {}

    def remember_replay_fields(self, message: Message, fields: dict[str, Any]) -> None:
        self._replay_fields[assistant_replay_key(message.content, message.tool_calls or [])] = fields

    def peek_provider_replay(self, content: str, tool_calls: list[ToolCall]) -> dict | None:
        fields = self._replay_fields.get(assistant_replay_key(content, tool_calls))
        if fields and ("reasoning_content" in fields or "reasoning_details" in fields):
            return dict(fields)
        return None

    def seed_provider_replay(self, content: str, tool_calls: list[ToolCall], replay: dict) -> None:
        if "reasoning_content" in replay or "reasoning_details" in replay:
            self.remember_replay_fields(
                Message(role="assistant", content=content, tool_calls=tool_calls or None),
                dict(replay),
            )

    def _merge_replay_into_openai_messages(
        self,
        serialized: list[dict[str, Any]],
        context: RenderedContext,
        degrade_missing_reasoning: bool = False,
    ) -> list[dict[str, Any]]:
        # to_openai_message_params appends the State turn LAST, so the cursor only
        # skips the system message to align with context.turns (history).
        cursor = 1 if context.system_text else 0
        for source in context.turns:
            if source.role == "tool":
                cursor += sum(
                    1 for p in (getattr(source, "content_parts", None) or [])
                    if p.type == "tool_result"
                )
                continue
            if source.role == "assistant":
                replay = self._replay_fields.get(
                    assistant_replay_key(source.content, source.tool_calls or [])
                )
                wire = openai_chat_wire_replay_fields(replay)
                if not wire and degrade_missing_reasoning and source.tool_calls:
                    # Reasoning-requiring provider, no stored reasoning for this
                    # tool-call turn, caller opted into degradation: inject a
                    # placeholder so the wire message stays well-formed instead of
                    # failing the whole request.
                    wire = {"reasoning_content": DEGRADED_REASONING_PLACEHOLDER}
                if wire:
                    serialized[cursor] = {**serialized[cursor], **wire}
            cursor += 1
        return serialized

    def assess_reasoning(self, context: RenderedContext) -> dict:
        """Raise-free pre-flight check: which assistant tool-call turns in
        ``context`` lack the non-empty reasoning replay this provider needs."""
        return assess_reasoning_replay(
            context.turns,
            lambda content, tool_calls: self._replay_fields.get(
                assistant_replay_key(content, tool_calls)
            ),
        )

    def remember_reasoning_for_turn(
        self,
        content: str,
        tool_calls: list[ToolCall],
        reasoning_content: str,
    ) -> None:
        if tool_calls or reasoning_content:
            self.remember_replay_fields(
                Message(role="assistant", content=content, tool_calls=tool_calls),
                {"reasoning_content": reasoning_content},
            )
