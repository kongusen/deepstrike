"""O5 — host tool hooks (the PreToolUse/PostToolUse-hook analog):
``on_tool_call`` can BLOCK a kernel-approved call with a reason (fed back to the model as a
denied tool result); ``on_tool_result`` can replace the output and/or inject a note into the
signal stream. The seam for STATEFUL host policy — declarative rules stay in governance_policy."""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool


class TwoToolTurnsProvider:
    def __init__(self) -> None:
        self.calls: list[RenderedContext] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.calls.append(context)
        if len(self.calls) <= 2:
            yield ToolCallEvent(id=f"call_{len(self.calls)}", name="write_thing", arguments={"v": "x"})
            return
        yield TextDelta(delta="done")


def _make_runner(provider, executed, **hooks):
    @tool
    def write_thing(v: str) -> str:
        """Write."""
        executed.append(v)
        return "written"

    plane = LocalExecutionPlane().register(write_thing)
    return RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=2048,
        max_turns=6,
        **hooks,
    ))


def _all_text(calls) -> str:
    parts = []
    for c in calls:
        for attr in ("system_text", "system_stable", "system_knowledge"):
            v = getattr(c, attr, None)
            if v:
                parts.append(v)
        st = getattr(c, "state_turn", None)
        if st is not None and getattr(st, "content", None):
            parts.append(st.content)
        for m in c.turns:
            parts.append(str(getattr(m, "content", "") or ""))
            for cp in getattr(m, "content_parts", None) or []:
                # tool_result parts carry the payload in `output`; ContentPart repr hides it.
                parts.append(str(getattr(cp, "output", "") or ""))
                parts.append(str(getattr(cp, "text", "") or ""))
    return "\n".join(parts)


@pytest.mark.asyncio
async def test_on_tool_call_fails_closed_unless_explicitly_open():
    def unavailable(_call):
        raise RuntimeError("policy backend unavailable")

    closed_executed = []
    closed = _make_runner(TwoToolTurnsProvider(), closed_executed, on_tool_call=unavailable)
    async for _ in closed.run(goal="write"):
        pass

    open_executed = []
    opened = _make_runner(
        TwoToolTurnsProvider(), open_executed,
        on_tool_call=unavailable, on_tool_call_failure="open",
    )
    async for _ in opened.run(goal="write"):
        pass

    assert closed_executed == []
    assert len(open_executed) > 0


@pytest.mark.asyncio
async def test_on_tool_call_blocks_statefully_and_feeds_reason_back():
    provider = TwoToolTurnsProvider()
    executed: list[str] = []
    seen: dict[str, int] = {}

    def deny_duplicates(call: dict):
        key = f"{call['name']}:{call['arguments']}"
        seen[key] = seen.get(key, 0) + 1
        if seen[key] >= 2:
            return {"block": True, "reason": "duplicate call — do something different"}
        return None

    runner = _make_runner(provider, executed, on_tool_call=deny_duplicates)
    async for _ in runner.run(goal="write"):
        pass

    assert executed == ["x"], "the duplicate must be vetoed before execution"
    later = _all_text(provider.calls[2:])
    assert "duplicate call — do something different" in later


@pytest.mark.asyncio
async def test_on_tool_result_replaces_output_and_injects_note():
    provider = TwoToolTurnsProvider()
    executed: list[str] = []

    async def annotate(result: dict):
        if result["output"] == "written":
            return {"replace_output": "written (no change detected)", "note": "the write was a no-op"}
        return None

    runner = _make_runner(provider, executed, on_tool_result=annotate)
    async for _ in runner.run(goal="write"):
        pass

    assert len(executed) >= 1
    all_text = _all_text(provider.calls)
    assert "written (no change detected)" in all_text
    assert "[SIGNAL] the write was a no-op" in all_text
