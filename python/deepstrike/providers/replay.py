from __future__ import annotations

import json
from typing import Any

from deepstrike._kernel import Message, ToolCall

from .base import RenderedContext


def assistant_replay_key(content: str, tool_calls: list[ToolCall]) -> str:
    return json.dumps({
        "content": content,
        "tool_calls": [
            {"id": tc.id, "name": tc.name, "arguments": tc.arguments}
            for tc in tool_calls
        ],
    }, sort_keys=True)


class ReasoningReplayMixin:
    """OpenAI-compatible providers: persist reasoning_content across tool turns."""

    _replay_fields: dict[str, dict[str, Any]]

    def _init_replay_store(self) -> None:
        self._replay_fields = {}

    def remember_replay_fields(self, message: Message, fields: dict[str, Any]) -> None:
        self._replay_fields[assistant_replay_key(message.content, message.tool_calls or [])] = fields

    def peek_provider_replay(self, content: str, tool_calls: list[ToolCall]) -> dict | None:
        fields = self._replay_fields.get(assistant_replay_key(content, tool_calls))
        if fields and fields.get("reasoning_content"):
            return {"reasoning_content": fields["reasoning_content"]}
        return None

    def seed_provider_replay(self, content: str, tool_calls: list[ToolCall], replay: dict) -> None:
        reasoning = replay.get("reasoning_content")
        if reasoning:
            self.remember_replay_fields(
                Message(role="assistant", content=content, tool_calls=tool_calls or None),
                {"reasoning_content": reasoning},
            )

    def _merge_replay_into_openai_messages(
        self,
        serialized: list[dict[str, Any]],
        context: RenderedContext,
    ) -> list[dict[str, Any]]:
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
                if replay:
                    serialized[cursor] = {**serialized[cursor], **replay}
            cursor += 1
        return serialized

    def remember_reasoning_for_turn(
        self,
        content: str,
        tool_calls: list[ToolCall],
        reasoning_content: str,
    ) -> None:
        if tool_calls and reasoning_content:
            self.remember_replay_fields(
                Message(role="assistant", content=content, tool_calls=tool_calls),
                {"reasoning_content": reasoning_content},
            )
