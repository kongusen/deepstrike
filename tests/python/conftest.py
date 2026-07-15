"""
Shared fixtures and helpers for the Python SDK integration test suite.
Mirrors tests/node/helpers.ts.
"""
from __future__ import annotations

import os
import sys
import uuid
from pathlib import Path
from typing import Any

import pytest

ROOT = Path(__file__).resolve().parents[2]

try:
    from dotenv import load_dotenv
    load_dotenv(ROOT / ".env")
except ImportError:
    pass

sys.path.insert(0, str(ROOT / "python"))

from deepstrike import (
    OpenAIProvider,
    WorkingMemory,
    PermissionManager, PermissionMode,
    Governance,
    SkillRegistry,
    KnowledgeSource,
    SignalGateway,
)
from deepstrike.runtime import (
    RuntimeRunner,
    RuntimeOptions,
    InMemorySessionLog,
    LocalExecutionPlane,
    collect_text,
)
from deepstrike.providers.stream import StreamEvent, TextDelta, DoneEvent
from deepstrike.memory.protocols import (
    DreamStore, SessionData, MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord,
    MemoryScope,
)

ENV = {
    "api_key":  os.environ.get("OPENAI_API_KEY", ""),
    "model":    os.environ.get("OPENAI_MODEL", "gpt-4o-mini"),
    "base_url": os.environ.get("OPENAI_BASE_URL", "https://api.openai.com/v1"),
}

SKILL_DIR = str(Path(__file__).parent / "fixtures" / "skills")


def make_provider():
    from deepstrike.providers.base import RetryConfig
    return OpenAIProvider(
        api_key=ENV["api_key"],
        model=ENV["model"],
        retry_config=RetryConfig(max_retries=2, base_delay=0.5),
        base_url=ENV["base_url"],
    )


class RunnerHandle:
    """Test helper: RuntimeRunner with ergonomic register/run helpers."""

    def __init__(self, runner: RuntimeRunner) -> None:
        self._runner = runner

    def register(self, *tools: Any) -> "RunnerHandle":
        for t in tools:
            self._runner.execution_plane.register(t)
        return self

    def interrupt(self) -> None:
        self._runner.interrupt()

    @property
    def _interrupted(self) -> bool:
        return self._runner._interrupted

    def run_streaming(self, goal: str, **kwargs: Any):
        return self._runner.run_streaming(goal, **kwargs)

    async def run(self, goal: str, **kwargs: Any) -> str:
        session_id = kwargs.pop("session_id", None) or str(uuid.uuid4())
        return await collect_text(self._runner.run_streaming(
            goal,
            session_id=session_id,
            **kwargs,
        ))

def make_runner(**overrides: Any) -> RunnerHandle:
    defaults: dict[str, Any] = dict(max_tokens=4096, max_turns=10)
    defaults.update(overrides)
    provider = defaults.pop("provider", None) or make_provider()
    session_log = defaults.pop("session_log", None) or InMemorySessionLog()
    tools = defaults.pop("tools", [])
    plane = LocalExecutionPlane()
    for t in tools:
        plane.register(t)
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=session_log,
        execution_plane=plane,
        **defaults,
    ))
    return RunnerHandle(runner)


# Test-only alias (not the removed Agent class)
make_agent = make_runner


class MockDreamStore:
    def __init__(self):
        self._memories: dict[str, list[MemoryRecord]] = {}
        self.saved_sessions: list[SessionData] = []

    async def upsert(self, agent_id: str, incoming: MemoryRecord):
        records = list(self._memories.get(agent_id, []))
        index = next((i for i, record in enumerate(records)
                      if record.scope == incoming.scope and record.kind == incoming.kind and record.name == incoming.name), None)
        if index is None:
            records.append(incoming)
        else:
            records[index] = incoming
        self._memories[agent_id] = records

    async def search(self, agent_id: str, query: MemoryQuery) -> list[MemoryRecall]:
        records = [record for record in self._memories.get(agent_id, []) if record.scope == query.scope]
        return [MemoryRecall(record=record, score=record.confidence, why="fixture") for record in records[:query.top_k]]

    async def save_session(self, data: SessionData) -> None:
        self.saved_sessions.append(data)


class MockKnowledgeSource:
    def __init__(self, snippets: list[str]):
        self._snippets = snippets
        self.init_called = 0

    async def init(self) -> None:
        self.init_called += 1

    async def retrieve(self, query: str, top_k: int = 5) -> list[str]:
        return self._snippets[:top_k]


async def collect_events(gen) -> list[StreamEvent]:
    events: list[StreamEvent] = []
    async for e in gen:
        events.append(e)
    return events


def text(events: list[StreamEvent]) -> str:
    return "".join(e.delta for e in events if isinstance(e, TextDelta))
