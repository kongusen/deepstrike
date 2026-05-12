"""
Shared fixtures and helpers for the Python SDK integration test suite.
Mirrors tests/node/helpers.ts.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path
from dataclasses import dataclass, field
from typing import Any

import pytest

# ─── Environment ────────────────────────────────────────────────────────────

ROOT = Path(__file__).resolve().parents[2]

try:
    from dotenv import load_dotenv
    load_dotenv(ROOT / ".env")
except ImportError:
    pass

# Ensure the Python SDK package is importable without install
sys.path.insert(0, str(ROOT / "python"))

from deepstrike import (
    Agent, OpenAIProvider,
    WorkingMemory,
    PermissionManager, PermissionMode,
    Governance,
    SkillRegistry,
    KnowledgeSource,
    SignalGateway,
)
from deepstrike.providers.stream import StreamEvent, TextDelta, DoneEvent
from deepstrike.memory.protocols import (
    DreamStore, SessionData, MemoryEntry, CurationResult, CurationStats,
)

# ─── ENV config ─────────────────────────────────────────────────────────────

ENV = {
    "api_key":  os.environ.get("OPENAI_API_KEY", ""),
    "model":    os.environ.get("OPENAI_MODEL", "gpt-4o-mini"),
    "base_url": os.environ.get("OPENAI_BASE_URL", "https://api.openai.com/v1"),
}

SKILL_DIR = str(Path(__file__).parent / "fixtures" / "skills")

# ─── Factory helpers ────────────────────────────────────────────────────────

def make_provider():
    from deepstrike.providers.base import RetryConfig
    return OpenAIProvider(
        api_key=ENV["api_key"],
        model=ENV["model"],
        retry_config=RetryConfig(max_retries=2, base_delay=0.5),
        base_url=ENV["base_url"],
    )


def make_agent(**overrides):
    defaults = dict(max_tokens=4096, max_turns=10)
    defaults.update(overrides)
    provider = defaults.pop("provider", None) or make_provider()
    return Agent(provider, **defaults)

# ─── In-memory DreamStore ───────────────────────────────────────────────────

class MockDreamStore:
    def __init__(self):
        self._sessions: dict[str, list[SessionData]] = {}
        self._memories: dict[str, list[MemoryEntry]] = {}

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

# ─── In-memory KnowledgeSource ──────────────────────────────────────────────

class MockKnowledgeSource:
    def __init__(self, snippets: list[str]):
        self._snippets = snippets

    async def retrieve(self, query: str, top_k: int = 5) -> list[str]:
        return self._snippets[:top_k]

# ─── Stream helpers ─────────────────────────────────────────────────────────

async def collect_events(gen) -> list[StreamEvent]:
    events: list[StreamEvent] = []
    async for e in gen:
        events.append(e)
    return events


def text(events: list[StreamEvent]) -> str:
    return "".join(
        e.delta for e in events if isinstance(e, TextDelta)
    )
