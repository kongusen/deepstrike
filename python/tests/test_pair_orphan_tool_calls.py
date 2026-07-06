"""Regression tests for `_pair_orphan_tool_calls` — the loop-replay pairing of kernel-consumed
meta-tool (e.g. `pace`) tool_calls. Mirrors the Node SDK's wake-recovery.test.ts pairing tests.

A kernel-consumed meta-tool is answered by a synthetic tool result the kernel keeps in its OWN
history but never emits as a `tool_completed` session event, so on a loop round's replay it left an
assistant tool_call unanswered — which strict OpenAI-compatible providers reject. The fix re-pairs
such orphans, but only when the run continued past them (a later non-tool message exists); a
genuinely pending tail tool_call (wake/recovery) must stay unpaired so wake executes it.
"""
from __future__ import annotations

from deepstrike._kernel import Message, ContentPartObj, ToolCall
from deepstrike.runtime.runner import _pair_orphan_tool_calls


def _user(text: str) -> Message:
    return Message(role="user", content=text, tool_calls=[])


def _asst(content: str, calls: list[tuple[str, str]]) -> Message:
    return Message(role="assistant", content=content,
                   tool_calls=[ToolCall(id=i, name=n, arguments="{}") for i, n in calls])


def _tool(call_id: str) -> Message:
    return Message(role="tool", content="", tool_calls=[],
                   content_parts=[ContentPartObj(type="tool_result", call_id=call_id, output="ok", is_error=False)])


def test_pairs_orphan_meta_tool_when_run_continued_past_it():
    # user -> assistant(pace tool_call, no result) -> assistant(final): the kernel consumed `pace`.
    out = _pair_orphan_tool_calls([_user("go"), _asst("", [("call_pace", "pace")]), _asst("round report", [])])
    # A synthetic tool result is spliced in right after the pace call -> strict validators pass.
    assert len(out) == 4
    assert out[2].role == "tool"
    assert out[2].content_parts[0].call_id == "call_pace"
    assert out[3].content == "round report"


def test_leaves_pending_tail_tool_call_unpaired():
    # The run stopped right after emitting a real tool_call — nothing follows. Wake must execute it,
    # so it must NOT be pre-answered by a synthetic result.
    out = _pair_orphan_tool_calls([_user("go"), _asst("", [("call_ping", "ping")])])
    assert len(out) == 2
    assert out[1].tool_calls[0].id == "call_ping"


def test_does_not_touch_already_answered_tool_calls():
    out = _pair_orphan_tool_calls([_user("go"), _asst("", [("c1", "read")]), _tool("c1"), _asst("done", [])])
    assert len(out) == 4  # unchanged — no synthetic insert
