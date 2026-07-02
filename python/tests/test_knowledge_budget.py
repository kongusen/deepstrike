"""K2 — knowledge budget (mirrors node/tests/knowledge-budget.test.ts).

Over-budget knowledge marks the OLDEST unpinned, non-skill entries for eviction at the next
compaction boundary; pinned entries survive regardless of age; ratio 0 disables the cap.
"""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool

EVICTABLE = "OLD_UNPINNED_REFERENCE_"
PINNED = "PINNED_CRITICAL_REFERENCE"


class BudgetProvider:
    def __init__(self, ratio_zero: bool = False) -> None:
        self.call = 0
        self.final_knowledge = ""
        self.runner: RuntimeRunner | None = None

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.call += 1
        if self.call == 1:
            assert self.runner is not None
            # Budget = 480 × 0.25 = 120 tokens. Pinned first (oldest), then two unpinned 60-token
            # entries ⇒ ~180 used ⇒ the OLDEST unpinned entry gets marked; pinned is exempt.
            self.runner.push_knowledge(PINNED.ljust(240, "p"), 60, key="keep", pinned=True)
            self.runner.push_knowledge((EVICTABLE + "1").ljust(240, "x"), 60, key="old1")
            self.runner.push_knowledge((EVICTABLE + "2").ljust(240, "y"), 60, key="old2")
            yield ToolCallEvent(id=f"b{self.call}", name="bulk", arguments={})
            return
        if self.call <= 10:
            yield ToolCallEvent(id=f"b{self.call}", name="bulk", arguments={})
            return
        self.final_knowledge = context.system_knowledge or ""
        yield TextDelta(delta="done")


def _make_runner(provider: BudgetProvider, session_log: InMemorySessionLog, **extra) -> RuntimeRunner:
    @tool
    def bulk() -> str:
        """Bulk filler output."""
        return "z" * 240

    return RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=session_log,
        execution_plane=LocalExecutionPlane().register(bulk),
        max_tokens=480,
        max_turns=30,
        repeat_fuse=False,
        **extra,
    ))


@pytest.mark.asyncio
async def test_budget_evicts_oldest_unpinned_pinned_survives():
    provider = BudgetProvider()
    session_log = InMemorySessionLog()
    runner = _make_runner(provider, session_log)
    provider.runner = runner

    async for _ in runner.run(goal="exercise the budget", session_id="knowledge-budget"):
        pass

    events = await session_log.read("knowledge-budget")
    assert any(e.event.get("kind") == "compressed" for e in events)

    assert PINNED in provider.final_knowledge
    assert (EVICTABLE + "1") not in provider.final_knowledge


@pytest.mark.asyncio
async def test_budget_ratio_zero_disables():
    provider = BudgetProvider()
    session_log = InMemorySessionLog()
    runner = _make_runner(provider, session_log, knowledge_budget_ratio=0.0)
    provider.runner = runner

    async for _ in runner.run(goal="no cap", session_id="knowledge-budget-off"):
        pass

    assert (EVICTABLE + "1") in provider.final_knowledge
    assert (EVICTABLE + "2") in provider.final_knowledge
