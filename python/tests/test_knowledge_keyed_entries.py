"""K1 — keyed knowledge entries (mirrors node/tests/knowledge-keyed-entries.test.ts).

Knowledge renders into the cached system[1] block, so identity mutations are boundary-deferred:
a same-key ``push_knowledge`` stages an upsert and ``remove_knowledge`` stages a drop, both
applied only when a compaction rewrites the prompt-cache prefix anyway. Mid-generation the
ORIGINAL bytes keep rendering; after the boundary the staged state lands.
"""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool

V1 = "KEYED_REF_CONTENT_V1"
V2 = "KEYED_REF_CONTENT_V2"
TMP = "TEMPORARY_NOTE_TO_DROP"


class LifecycleProvider:
    def __init__(self) -> None:
        self.call = 0
        self.mid_run_knowledge = ""
        self.final_knowledge = ""
        self.runner: RuntimeRunner | None = None

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.call += 1
        if self.call == 1:
            assert self.runner is not None
            self.runner.push_knowledge(V1, key="ref")
            self.runner.push_knowledge(V2, key="ref")
            self.runner.push_knowledge(TMP, key="tmp")
            self.runner.remove_knowledge("tmp")
            yield ToolCallEvent(id=f"b{self.call}", name="bulk", arguments={})
            return
        if self.call == 2:
            self.mid_run_knowledge = context.system_knowledge or ""
            yield ToolCallEvent(id=f"b{self.call}", name="bulk", arguments={})
            return
        if self.call <= 11:
            yield ToolCallEvent(id=f"b{self.call}", name="bulk", arguments={})
            return
        self.final_knowledge = context.system_knowledge or ""
        yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_keyed_upsert_and_remove_apply_at_compaction_boundary():
    provider = LifecycleProvider()
    session_log = InMemorySessionLog()

    @tool
    def bulk() -> str:
        """Bulk filler output."""
        return "z" * 240

    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=session_log,
        execution_plane=LocalExecutionPlane().register(bulk),
        max_tokens=480,
        max_turns=30,
        # The script repeats an identical `bulk()` call to build pressure — incidental to the
        # repeat fuse's intent, so disabled (same as the paging integration tests).
        repeat_fuse=False,
    ))
    provider.runner = runner

    async for _ in runner.run(goal="exercise keyed knowledge", session_id="keyed-entries"):
        pass

    # Pre-boundary: one entry rendering V1 (upsert staged, not applied); TMP still visible.
    assert V1 in provider.mid_run_knowledge
    assert V2 not in provider.mid_run_knowledge
    assert TMP in provider.mid_run_knowledge

    # A compaction boundary definitely happened.
    events = await session_log.read("keyed-entries")
    assert any(e.event.get("kind") == "compressed" for e in events)

    # Post-boundary: upsert applied (V2), removal applied (TMP gone).
    assert V2 in provider.final_knowledge
    assert V1 not in provider.final_knowledge
    assert TMP not in provider.final_knowledge
