from __future__ import annotations

from typing import Any, Callable

from deepstrike._kernel import Message


class ProviderReplayValidationError(Exception):
    """Raised before dispatch when a rebuilt history would violate the
    OpenAI-compatible tool / reasoning protocol (fail fast instead of 400)."""


def validate_openai_chat_replay(
    turns: list[Message],
    *,
    descriptor: Any = None,
    require_non_empty_reasoning_for_tool_calls: bool = False,
    replay_for_assistant: Callable[[str, list], dict | None] | None = None,
) -> None:
    _validate_strict_tool_result_pairing(turns)
    if require_non_empty_reasoning_for_tool_calls:
        _validate_reasoning_replay_for_assistant_tool_calls(turns, descriptor, replay_for_assistant)


def _tool_result_parts(message: Message) -> list:
    return [p for p in (getattr(message, "content_parts", None) or []) if p.type == "tool_result"]


def _validate_strict_tool_result_pairing(turns: list[Message]) -> None:
    pending_ids: set[str] | None = None
    completed_ids: set[str] = set()

    for message in turns:
        if message.role == "assistant":
            tool_calls = getattr(message, "tool_calls", None) or []
            pending_ids = {tc.id for tc in tool_calls} if tool_calls else None
            completed_ids = set()
            continue

        if message.role != "tool":
            pending_ids = None
            completed_ids = set()
            continue

        for part in _tool_result_parts(message):
            if not pending_ids or part.call_id not in pending_ids:
                raise ProviderReplayValidationError(
                    f"OpenAI-compatible replay has orphan tool result {part.call_id}: "
                    "no preceding assistant tool_call with the same id."
                )
            if part.call_id in completed_ids:
                raise ProviderReplayValidationError(
                    f"OpenAI-compatible replay has duplicate tool result {part.call_id}."
                )
            completed_ids.add(part.call_id)


def _validate_reasoning_replay_for_assistant_tool_calls(
    turns: list[Message],
    descriptor: Any,
    replay_for_assistant: Callable[[str, list], dict | None] | None,
) -> None:
    for message in turns:
        if message.role != "assistant" or not getattr(message, "tool_calls", None):
            continue
        replay = replay_for_assistant(message.content, message.tool_calls) if replay_for_assistant else None
        reasoning = replay.get("reasoning_content") if isinstance(replay, dict) else None
        if not (isinstance(reasoning, str) and reasoning.strip()):
            call_ids = ", ".join(tc.id for tc in message.tool_calls)
            who = f"{descriptor.provider}/{descriptor.model}" if descriptor is not None else "provider"
            raise ProviderReplayValidationError(
                f"{who} replay requires non-empty reasoning_content for assistant tool call turn {call_ids}. "
                "Disable thinking, rebuild this history with provider replay, or switch to a provider "
                "that can replay this turn."
            )
