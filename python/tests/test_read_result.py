"""O7 — the `read_result` meta-tool: once the kernel evicts (spools) a large tool result from
context, it exposes `read_result` in the toolset so the model can re-fetch the full output by
`call_id`. The kernel only advertises the capability; the HOST resolves the content (in-memory
pending map -> on-disk result spool -> session-log scan). Mirrors the Node
`tests/read-result.test.ts` integration test."""

import shutil
import asyncio
from pathlib import Path

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent, ToolResultEvent
from deepstrike.runtime.large_result_spool import LargeResultSpool
from deepstrike.tools.registry import tool

SPOOL_DIR = Path.cwd() / ".spool-read-result-test-py"


class SpoolThenReadProvider:
    def __init__(self) -> None:
        self.calls: list[RenderedContext] = []
        self.seen_tools: list[list] = []
        self.read_result_output: str | None = None

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.calls.append(context)
        self.seen_tools.append(list(tools))
        if len(self.calls) == 1:
            # Turn 1: produce the oversized result the kernel will spool out of context.
            yield ToolCallEvent(id="big-1", name="big_out", arguments={})
            return
        if any(t.name == "read_result" for t in tools) and self.read_result_output is None:
            # Turn 2+: the kernel now advertises `read_result` (a handle left residency).
            yield ToolCallEvent(id="read-1", name="read_result", arguments={"call_id": "big-1"})
            return
        yield TextDelta(delta="done")


@pytest.fixture(autouse=True)
def _clean_spool_dir():
    yield
    shutil.rmtree(SPOOL_DIR, ignore_errors=True)


@pytest.mark.asyncio
async def test_read_result_refetches_spooled_output_by_call_id():
    huge = "y" * (100 * 1024)
    spool = LargeResultSpool(spool_dir=str(SPOOL_DIR))
    provider = SpoolThenReadProvider()

    @tool
    def big_out() -> str:
        """Return an oversized result."""
        return huge

    plane = LocalExecutionPlane().register(big_out)
    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=session_log,
        execution_plane=plane,
        max_tokens=128_000,
        max_turns=8,
        result_spool=spool,
    ))

    read_result_output = None
    async for evt in runner.run(goal="fetch big output", session_id="read-result-run"):
        if isinstance(evt, ToolResultEvent) and evt.call_id == "read-1":
            read_result_output = evt.content

    # Sanity: the kernel did actually spool the oversized result out of context.
    logged = await session_log.read("read-result-run")
    assert any(e.event.get("kind") == "large_result_spooled" for e in logged)

    # The toolset advertised `read_result` only once eviction happened (progressive disclosure).
    assert not any(t.name == "read_result" for t in provider.seen_tools[0])
    assert any(any(t.name == "read_result" for t in ts) for ts in provider.seen_tools)

    # The host resolved the call_id back to the ORIGINAL full content.
    assert read_result_output is not None
    assert f"of {len(huge)}" in read_result_output
    assert huge[:4000] in read_result_output


@pytest.mark.asyncio
async def test_spool_hashes_untrusted_call_ids_and_commits_atomically(tmp_path):
    spool = LargeResultSpool(spool_dir=str(tmp_path / "spool"))
    call_id = "../../outside/../tool-call"
    refs = await asyncio.gather(*[
        spool.persist_output("session", call_id, "stable-output") for _ in range(8)
    ])

    assert len(set(refs)) == 1
    ref = Path(refs[0])
    assert ref.parent == tmp_path / "spool"
    assert "tool-call" not in ref.name
    assert not list(ref.parent.glob("*.tmp"))
    assert await spool.find_by_call_id("session", call_id) == "stable-output"


@pytest.mark.asyncio
async def test_spool_call_id_lookup_is_session_scoped(tmp_path):
    """The spool dir is shared across sessions and outlives runs, while vendor call ids can be
    index-style ("call_0") and repeat — an unscoped key let read_result in one session fetch
    another session's spooled output (data bleed) or a stale run's content."""
    spool = LargeResultSpool(spool_dir=str(tmp_path / "spool"))

    await spool.persist_output("session-a", "call_0", "secret output of session A")
    await spool.persist_output("session-b", "call_0", "output of session B")

    assert await spool.find_by_call_id("session-a", "call_0") == "secret output of session A"
    assert await spool.find_by_call_id("session-b", "call_0") == "output of session B"
    assert await spool.find_by_call_id("session-c", "call_0") is None
