"""O2 — ``inject_note`` (the system-reminder channel): an imperative host push into the run's
signal stream, without wiring a full ``SignalSource``. A normal-urgency note is queued by the
kernel attention policy and drained at the next turn boundary, so it renders as a
``[SIGNAL] <text>`` line in the state turn of the FOLLOWING provider call (same timing as a
polled ``signal_source`` signal — the in-flight turn's context is already rendered when the
note is applied)."""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool


class CapturingToolProvider:
    def __init__(self) -> None:
        self.calls: list[RenderedContext] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.calls.append(context)
        if len(self.calls) <= 2:
            yield ToolCallEvent(id=f"call_{len(self.calls)}", name="set_title", arguments={"title": "same"})
            return
        yield TextDelta(delta="done")


def _rendered_text(ctx: RenderedContext) -> str:
    parts = [
        getattr(ctx, "system_text", None),
        getattr(ctx, "system_stable", None),
        getattr(ctx, "system_knowledge", None),
    ]
    state_turn = getattr(ctx, "state_turn", None)
    if state_turn is not None:
        parts.append(getattr(state_turn, "content", None))
    parts.extend(getattr(m, "content", None) for m in ctx.turns)
    return "\n".join(p for p in parts if p)


@pytest.mark.asyncio
async def test_inject_note_renders_as_signal_after_next_turn_boundary():
    provider = CapturingToolProvider()
    runner_ref: list[RuntimeRunner] = []

    @tool
    def set_title(title: str) -> str:
        """Set the document title."""
        # Host-detected no-op write: feed precise negative feedback back to the model.
        runner_ref[0].inject_note(f'title is already "{title}" — the write was a no-op, stop repeating it')
        return "unchanged"

    plane = LocalExecutionPlane().register(set_title)
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=2048,
        max_turns=6,
    ))
    runner_ref.append(runner)

    async for _ in runner.run(goal="set the title"):
        pass

    assert len(provider.calls) >= 3
    # Turn 2's context was already rendered when the note from turn 1's tool run was applied;
    # the note must surface by turn 3's prompt.
    assert '[SIGNAL] title is already "same" — the write was a no-op, stop repeating it' in _rendered_text(provider.calls[2])
