from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Literal, Protocol, TypedDict

from deepstrike._kernel import ToolCall, ToolResult


class RunStartedEvent(TypedDict, total=False):
    kind: Literal["run_started"]
    run_id: str
    goal: str
    criteria: list[str]
    agent_id: str
    system_prompt: str


class LlmCompletedEvent(TypedDict, total=False):
    kind: Literal["llm_completed"]
    turn: int
    content: str
    token_count: int
    tool_calls: list[ToolCall]
    provider_replay: dict


class ToolRequestedEvent(TypedDict, total=False):
    kind: Literal["tool_requested"]
    turn: int
    calls: list[ToolCall]


class ToolCompletedEvent(TypedDict, total=False):
    kind: Literal["tool_completed"]
    turn: int
    results: list[ToolResult]


class CompressedEvent(TypedDict, total=False):
    kind: Literal["compressed"]
    turn: int
    archived_seq_range: tuple[int, int]
    action: str
    summary: str
    summary_tokens: int
    archive_ref: str
    preserved_refs: list[str]


class RunTerminalEvent(TypedDict, total=False):
    kind: Literal["run_terminal"]
    reason: str
    turns_used: int
    total_tokens: int


SessionEvent = (
    RunStartedEvent
    | LlmCompletedEvent
    | ToolRequestedEvent
    | ToolCompletedEvent
    | CompressedEvent
    | RunTerminalEvent
)


@dataclass
class SessionEntry:
    seq: int
    event: SessionEvent


class SessionLog(Protocol):
    async def append(self, session_id: str, event: SessionEvent) -> int: ...
    async def read(self, session_id: str, from_seq: int = 0) -> list[SessionEntry]: ...
    async def latest_seq(self, session_id: str) -> int: ...


class InMemorySessionLog:
    def __init__(self) -> None:
        self._store: dict[str, list[SessionEntry]] = {}

    async def append(self, session_id: str, event: SessionEvent) -> int:
        if session_id not in self._store:
            self._store[session_id] = []
        seq = len(self._store[session_id])
        self._store[session_id].append(SessionEntry(seq=seq, event=event))
        return seq

    async def read(self, session_id: str, from_seq: int = 0) -> list[SessionEntry]:
        return [e for e in self._store.get(session_id, []) if e.seq >= from_seq]

    async def latest_seq(self, session_id: str) -> int:
        entries = self._store.get(session_id)
        return len(entries) - 1 if entries else -1


class FileSessionLog:
    """Single-writer per session. Safe for sequential appends within one instance.
    Cross-instance (multi-process) safety requires an external lock."""

    def __init__(self, directory: str | Path) -> None:
        self._dir = Path(directory)
        # Lazy-initialized per-session counter; avoids re-reading on every append.
        self._seq_counters: dict[str, int] = {}

    def _path(self, session_id: str) -> Path:
        return self._dir / f"{session_id}.jsonl"

    async def _next_seq(self, session_id: str) -> int:
        if session_id not in self._seq_counters:
            existing = await self.read(session_id)
            self._seq_counters[session_id] = len(existing)
        seq = self._seq_counters[session_id]
        self._seq_counters[session_id] = seq + 1
        return seq

    async def append(self, session_id: str, event: SessionEvent) -> int:
        self._dir.mkdir(parents=True, exist_ok=True)
        seq = await self._next_seq(session_id)
        line = json.dumps({"seq": seq, "event": _event_to_json(event)}, ensure_ascii=False)
        with self._path(session_id).open("a", encoding="utf-8") as f:
            f.write(line + "\n")
        return seq

    async def read(self, session_id: str, from_seq: int = 0) -> list[SessionEntry]:
        path = self._path(session_id)
        if not path.exists():
            return []
        results: list[SessionEntry] = []
        with path.open(encoding="utf-8") as f:
            for line in f:
                if not line.strip():
                    continue
                raw = json.loads(line)
                entry = SessionEntry(seq=int(raw["seq"]), event=_event_from_json(raw["event"]))
                if entry.seq >= from_seq:
                    results.append(entry)
        return results

    async def latest_seq(self, session_id: str) -> int:
        entries = await self.read(session_id)
        return len(entries) - 1


def _event_to_json(event: SessionEvent) -> dict:
  kind = event["kind"]
  if kind == "llm_completed":
    return {
      **event,
      "tool_calls": [
        {"id": c.id, "name": c.name, "arguments": c.arguments}
        for c in event.get("tool_calls", [])
      ],
    }
  if kind == "tool_requested":
    return {
      **event,
      "calls": [{"id": c.id, "name": c.name, "arguments": c.arguments} for c in event["calls"]],
    }
  if kind == "tool_completed":
    return {
      **event,
      "results": [
        {"call_id": r.call_id, "output": r.output, "is_error": r.is_error}
        for r in event["results"]
      ],
    }
  return dict(event)


def _event_from_json(raw: dict) -> SessionEvent:
  kind = raw["kind"]
  if kind == "llm_completed":
    return {
      "kind": "llm_completed",
      "turn": raw["turn"],
      "content": raw.get("content", ""),
      "token_count": raw.get("token_count"),
      "tool_calls": [
        ToolCall(id=c["id"], name=c["name"], arguments=c["arguments"])
        for c in raw.get("tool_calls", [])
      ],
      **({"provider_replay": raw["provider_replay"]} if "provider_replay" in raw else {}),
    }
  if kind == "tool_requested":
    return {
      "kind": "tool_requested",
      "turn": raw["turn"],
      "calls": [ToolCall(id=c["id"], name=c["name"], arguments=c["arguments"]) for c in raw["calls"]],
    }
  if kind == "tool_completed":
    return {
      "kind": "tool_completed",
      "turn": raw["turn"],
      "results": [
        ToolResult(call_id=r["call_id"], output=r["output"], is_error=r.get("is_error", False))
        for r in raw["results"]
      ],
    }
  return raw  # type: ignore[return-value]
