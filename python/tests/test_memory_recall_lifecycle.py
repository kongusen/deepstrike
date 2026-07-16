"""T5: every memory query route shares ONE kernel recall lifecycle.

The prefetch path used to call ``dream_store.search`` directly and hand-assemble a history
message, so ``memory_recalled`` never fired for prefetched hits — recall counts froze and
promotions could never trigger. Prefetch now routes each query through the kernel's
``query_memory → memory_query_result`` effect: the kernel injects each routed hit into history
itself (``[MEMORY …]``, one message per hit — same shape as an in-run query) and derives the
recall lifecycle statelessly from the routed hits. The store stays a pure query; the runner
mirrors ``memory_recalled → record_recall`` and surfaces the kernel's edge-triggered
``promotion_suggested``. Mirrors node/tests/memory-recall-lifecycle.test.ts.
"""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.runtime.runner import MemoryPolicy
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta
from deepstrike.memory.protocols import (
    MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord, MemoryScope,
)

SCOPE = MemoryScope("agent-lifecycle", "t5")


def record(record_id: str, content: str, recall_count: int) -> MemoryRecord:
    return MemoryRecord(
        record_id=record_id, scope=SCOPE, name=record_id, kind="reference", content=content,
        description="t5 fixture",
        provenance=MemoryProvenance(author="host", trust="host_verified", evidence_refs=[]),
        created_at=1, updated_at=1, recall_count=recall_count, confidence=0.9,
    )


class TrackingStore:
    """Store whose record_recall really persists — so a later query sees the updated count."""

    def __init__(self, initial_count: int = 0, *, with_record_recall: bool = True, fail_search: bool = False):
        self.state = {"recall_count": initial_count}
        self.recall_calls: list[list] = []
        self._fail_search = fail_search
        if with_record_recall:
            self.record_recall = self._record_recall

    async def upsert(self, *args, **kwargs):
        return None

    async def save_session(self, *args, **kwargs):
        return None

    async def search(self, agent_id, query: MemoryQuery):
        if self._fail_search:
            raise RuntimeError("store offline")
        return [MemoryRecall(
            record=record("record-t5", "LONGTERM_FACT_T5", self.state["recall_count"]),
            score=0.9, why="fixture",
        )]

    async def _record_recall(self, agent_id, recalls):
        self.recall_calls.append(recalls)
        for r in recalls:
            self.state["recall_count"] = int(r.recall_count)


class TextProvider:
    def __init__(self, on_context=None):
        self._on_context = on_context

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        if self._on_context is not None:
            self._on_context(context)
        yield TextDelta(delta="done")


def make_runner(store, **extra) -> RuntimeRunner:
    opts = dict(
        agent_id="agent-lifecycle",
        memory_scope=SCOPE,
        dream_store=store,
        pre_query_memory=lambda goal: [MemoryQuery(SCOPE, "past facts", top_k=5)],
    )
    opts.update(extra)
    return RuntimeRunner(RuntimeOptions(
        provider=opts.pop("provider"),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=2048,
        max_turns=4,
        **opts,
    ))


async def collect_text(stream) -> str:
    text = ""
    async for evt in stream:
        if isinstance(evt, TextDelta):
            text += evt.delta
    return text


@pytest.mark.asyncio
async def test_initial_prefetch_routes_through_kernel():
    """Initial prefetch → record_recall(1) + kernel-shaped injection."""
    store = TrackingStore(0)
    captured = {"turns": ""}

    def on_context(context: RenderedContext):
        if not captured["turns"]:
            captured["turns"] = repr(context.turns)

    runner = make_runner(store, provider=TextProvider(on_context))
    await collect_text(runner.run(goal="use the fact", session_id="t5-initial"))

    assert len(store.recall_calls) == 1
    assert len(store.recall_calls[0]) == 1
    assert store.recall_calls[0][0].record_id == "record-t5"
    assert store.recall_calls[0][0].recall_count == 1
    # Kernel injection shape: one `[MEMORY …]` message per hit — the old hand-assembled
    # combined `[memory …]` message is gone.
    assert "[MEMORY record_id=record-t5" in captured["turns"]
    assert "LONGTERM_FACT_T5" in captured["turns"]
    assert "[memory record_id=" not in captured["turns"]


@pytest.mark.asyncio
async def test_two_prefetch_queries_same_record_recall_and_inject_once():
    store = TrackingStore(0)
    captured = {"turns": ""}

    def on_context(context: RenderedContext):
        if not captured["turns"]:
            captured["turns"] = repr(context.turns)

    runner = make_runner(
        store, provider=TextProvider(on_context),
        pre_query_memory=lambda goal: [
            MemoryQuery(SCOPE, "first angle", top_k=5),
            MemoryQuery(SCOPE, "second angle", top_k=5),
        ],
    )
    await collect_text(runner.run(goal="use the fact", session_id="t5-dedupe"))

    assert len(store.recall_calls) == 1
    assert len(store.recall_calls[0]) == 1
    assert captured["turns"].count("[MEMORY record_id=record-t5") == 1


@pytest.mark.asyncio
async def test_promotion_fires_on_threshold_crossing_and_never_refires():
    promotions: list[dict] = []

    def on_promotion(record_id, recall_count):
        promotions.append({"record_id": record_id, "recall_count": recall_count})

    # before=1 → after=2 crosses threshold 2: exactly one suggestion.
    crossing = TrackingStore(1)
    runner = make_runner(
        crossing, provider=TextProvider(),
        memory_policy=MemoryPolicy(promotion_recall_threshold=2),
        on_promotion_suggested=on_promotion,
    )
    await collect_text(runner.run(goal="use the fact", session_id="t5-promote"))
    assert promotions == [{"record_id": "record-t5", "recall_count": 2}]

    # before=2 (already at threshold) → after=3: no repeat suggestion.
    past = TrackingStore(2)
    runner2 = make_runner(
        past, provider=TextProvider(),
        memory_policy=MemoryPolicy(promotion_recall_threshold=2),
        on_promotion_suggested=on_promotion,
    )
    await collect_text(runner2.run(goal="use the fact", session_id="t5-past"))
    assert len(promotions) == 1
    assert len(past.recall_calls) == 1
    assert past.recall_calls[0][0].record_id == "record-t5"
    assert past.recall_calls[0][0].recall_count == 3


@pytest.mark.asyncio
async def test_failing_store_search_stays_errs_open():
    store = TrackingStore(0, fail_search=True)
    runner = make_runner(store, provider=TextProvider())

    text = await collect_text(runner.run(goal="use the fact", session_id="t5-fail"))

    assert "done" in text
    assert len(store.recall_calls) == 0


@pytest.mark.asyncio
async def test_store_without_record_recall_still_runs_and_promotes():
    promotions: list[dict] = []

    def on_promotion(record_id, recall_count):
        promotions.append({"record_id": record_id, "recall_count": recall_count})

    store = TrackingStore(1, with_record_recall=False)
    runner = make_runner(
        store, provider=TextProvider(),
        memory_policy=MemoryPolicy(promotion_recall_threshold=2),
        on_promotion_suggested=on_promotion,
    )

    text = await collect_text(runner.run(goal="use the fact", session_id="t5-norecord"))

    assert "done" in text
    assert len(store.recall_calls) == 0
    assert promotions == [{"record_id": "record-t5", "recall_count": 2}]


@pytest.mark.asyncio
async def test_host_query_memory_shares_recall_lifecycle():
    """Prefetch 1 → host query_memory() 2: the prefetch's record_recall persisted before the
    host query, so the kernel's stateless derivation continues the count instead of restarting it.
    """
    store = TrackingStore(0)
    runner = make_runner(store, provider=TextProvider())
    await collect_text(runner.run(goal="use the fact", session_id="t5-host"))
    assert len(store.recall_calls) == 1
    assert store.recall_calls[0][0].recall_count == 1

    hits = await runner.query_memory(
        MemoryQuery(SCOPE, "past facts", top_k=5),
        session_id="t5-host",
    )
    assert len(hits) == 1
    assert len(store.recall_calls) == 2
    assert store.recall_calls[1][0].recall_count == 2
