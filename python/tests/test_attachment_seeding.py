"""Attachment seeding is idempotent per session (mirrors node/tests/multimodal.test.ts).

A same-session retry (the AttemptLoop continue_session shape) must not double the image:
replay already reconstructs it from the first ``run_started``, so only the first run records
and live-seeds it. Different attachments in a later same-session run are still seeded.
"""
import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta


class CapturingProvider:
    def __init__(self) -> None:
        self.calls: list[RenderedContext] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.calls.append(context)
        yield TextDelta(delta="done")


def _image(data: str) -> dict:
    return {"type": "image", "data": data, "media_type": "image/png"}


def _count_image_parts(ctx: RenderedContext) -> int:
    return sum(
        1
        for message in ctx.turns
        for part in (getattr(message, "content_parts", None) or [])
        if getattr(part, "type", None) == "image"
    )


def _make_runner(provider: CapturingProvider, session_log: InMemorySessionLog) -> RuntimeRunner:
    return RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=session_log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=2048,
        max_turns=6,
    ))


@pytest.mark.asyncio
async def test_same_session_retry_does_not_double_the_image():
    attachments = [_image("iVBORw0KGgo=")]
    provider = CapturingProvider()
    session_log = InMemorySessionLog()
    runner = _make_runner(provider, session_log)

    async for _ in runner.run(goal="attempt 1", session_id="retry", attachments=attachments):
        pass
    async for _ in runner.run(goal="attempt 2", session_id="retry", attachments=attachments):
        pass

    # Only the first run_started records the attachments — replay reconstructs from it, so a
    # second record (or live seed) would double the image in history.
    starts = [e for e in await session_log.read("retry") if e.event.get("kind") == "run_started"]
    assert len(starts) == 2
    assert starts[0].event.get("attachments") == attachments
    assert "attachments" not in starts[1].event

    assert _count_image_parts(provider.calls[-1]) == 1


@pytest.mark.asyncio
async def test_different_attachments_in_later_same_session_run_are_seeded():
    provider = CapturingProvider()
    session_log = InMemorySessionLog()
    runner = _make_runner(provider, session_log)

    async for _ in runner.run(goal="first", session_id="two", attachments=[_image("AAAA")]):
        pass
    async for _ in runner.run(goal="second", session_id="two", attachments=[_image("BBBB")]):
        pass

    starts = [e for e in await session_log.read("two") if e.event.get("kind") == "run_started"]
    assert starts[1].event.get("attachments") == [_image("BBBB")]

    # Run 2 renders BOTH: image A replayed from run 1's history plus the newly seeded image B.
    assert _count_image_parts(provider.calls[-1]) == 2
