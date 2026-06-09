from __future__ import annotations

from typing import Any, Callable

from deepstrike._kernel import Message

# Placeholder reasoning injected for an assistant tool-call turn that has no
# stored reasoning replay when the caller opted into graceful degradation
# (``degrade_missing_reasoning``). Keeps the wire message well-formed for a
# thinking-on provider without fabricating substantive reasoning.
DEGRADED_REASONING_PLACEHOLDER = "[reasoning unavailable on replay]"


class ProviderReplayValidationError(Exception):
    """Raised before dispatch when a rebuilt history would violate the
    OpenAI-compatible tool / reasoning protocol (fail fast instead of 400)."""


def validate_openai_chat_replay(
    turns: list[Message],
    *,
    descriptor: Any = None,
    require_non_empty_reasoning_for_tool_calls: bool = False,
    degrade_missing_reasoning: bool = False,
    replay_for_assistant: Callable[[str, list], dict | None] | None = None,
) -> None:
    _validate_strict_tool_result_pairing(turns)
    if require_non_empty_reasoning_for_tool_calls and not degrade_missing_reasoning:
        assessment = assess_reasoning_replay(turns, replay_for_assistant)
        if not assessment["ok"]:
            raise _reasoning_replay_error(assessment["offending_call_ids"], descriptor)


def assess_reasoning_replay(
    turns: list[Message],
    replay_for_assistant: Callable[[str, list], dict | None] | None,
) -> dict:
    """Pure, raise-free assessment: which assistant tool-call turns lack the
    non-empty reasoning replay a reasoning-requiring provider needs. Lets an
    embedder decide per-candidate whether to keep thinking on, disable it, or
    skip the candidate — before sending."""
    offending_call_ids: list[str] = []
    for message in turns:
        if message.role != "assistant" or not getattr(message, "tool_calls", None):
            continue
        replay = replay_for_assistant(message.content, message.tool_calls) if replay_for_assistant else None
        reasoning = replay.get("reasoning_content") if isinstance(replay, dict) else None
        if not (isinstance(reasoning, str) and reasoning.strip()):
            offending_call_ids.extend(tc.id for tc in message.tool_calls)
    return {"ok": not offending_call_ids, "offending_call_ids": offending_call_ids}


def _reasoning_replay_error(call_ids: list[str], descriptor: Any) -> ProviderReplayValidationError:
    who = f"{descriptor.provider}/{descriptor.model}" if descriptor is not None else "provider"
    return ProviderReplayValidationError(
        f"{who} replay requires non-empty reasoning_content for assistant tool call turn {', '.join(call_ids)}. "
        "Disable thinking, rebuild this history with provider replay, switch to a provider that can replay this turn, "
        "or pass extensions['degrade_missing_reasoning_replay'] to send a degraded turn."
    )


def _tool_result_parts(message: Message) -> list:
    return [p for p in (getattr(message, "content_parts", None) or []) if p.type == "tool_result"]


def _validate_strict_tool_result_pairing(turns: list[Message]) -> None:
    pending_ids: set[str] | None = None
    completed_ids: set[str] = set()

    def assert_all_completed() -> None:
        if not pending_ids:
            return
        missing = [cid for cid in pending_ids if cid not in completed_ids]
        if missing:
            raise ProviderReplayValidationError(
                f"OpenAI-compatible replay has assistant tool_calls with no tool result for {', '.join(missing)}: "
                "every tool_call must be answered by a tool message before the next assistant or user turn."
            )

    for message in turns:
        if message.role == "assistant":
            assert_all_completed()
            tool_calls = getattr(message, "tool_calls", None) or []
            pending_ids = {tc.id for tc in tool_calls} if tool_calls else None
            completed_ids = set()
            continue

        if message.role != "tool":
            assert_all_completed()
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

    assert_all_completed()
