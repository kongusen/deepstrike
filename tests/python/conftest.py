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
    DreamStore, SessionData, MemoryEntry, CurationResult, CurationStats,
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

    async def dream(self, agent_id: str, now_ms: int | None = None):
        return await self._runner.dream(agent_id, now_ms)


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
        self._sessions: dict[str, list[SessionData]] = {}
        self._memories: dict[str, list[MemoryEntry]] = {}
        self.saved_sessions: list[SessionData] = []

    def add_session(self, agent_id: str, session: SessionData):
        self._sessions.setdefault(agent_id, []).append(session)

    async def load_sessions(self, agent_id: str) -> list[SessionData]:
        return self._sessions.get(agent_id, [])

    async def load_memories(self, agent_id: str) -> list[MemoryEntry]:
        return self._memories.get(agent_id, [])

    async def commit(self, agent_id: str, result: CurationResult, existing: list[MemoryEntry]):
        kept = [e for i, e in enumerate(existing) if i not in result.to_remove_indices]
        self._memories[agent_id] = kept + result.to_add

    async def search(self, agent_id: str, query: str, top_k: int = 5) -> list[MemoryEntry]:
        return (self._memories.get(agent_id, []))[:top_k]

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
