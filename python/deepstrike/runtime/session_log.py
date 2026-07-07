from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Literal, Protocol, TypedDict

from deepstrike._kernel import ToolCall, ToolResult
from deepstrike.runtime.kernel_event_log import (
    primitive_for_kind,
)


class RollbackReason(TypedDict, total=False):
    kind: Literal[
        "fatal_tool_error",
        "governance_denied",
        "provider_failure",
        "timeout",
        "user_interrupt",
        "malformed_replay",
    ]
    tool_name: str
    error: str
    reason: str


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


class ToolArgumentRepairedEvent(TypedDict, total=False):
    kind: Literal["tool_argument_repaired"]
    turn: int
    tool: str
    original_arguments: str
    repaired_arguments: str


class ToolDeniedEvent(TypedDict, total=False):
    kind: Literal["tool_denied"]
    turn: int
    call_id: str
    tool_name: str
    reason: str


class PermissionRequestedEvent(TypedDict, total=False):
    kind: Literal["permission_requested"]
    turn: int
    tool: str
    arguments: str
    reason: str


class PermissionResolvedEvent(TypedDict, total=False):
    kind: Literal["permission_resolved"]
    turn: int
    approved: bool
    responder: str


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


class RollbackedEvent(TypedDict, total=False):
    kind: Literal["rollbacked"]
    turn: int
    checkpoint_history_len: int
    reason: RollbackReason


class CapabilityChangedEvent(TypedDict, total=False):
    kind: Literal["capability_changed"]
    turn: int
    added: list[str]
    removed: list[str]
    change_kind: str
    capability_id: str
    version: str
    mounted_by: str
    mount_reason: str


class MilestoneAdvancedEvent(TypedDict, total=False):
    kind: Literal["milestone_advanced"]
    turn: int
    phase_id: str
    capabilities_unlocked: list[str]


class MilestoneBlockedEvent(TypedDict, total=False):
    kind: Literal["milestone_blocked"]
    turn: int
    phase_id: str
    reason: str


class CheckpointTakenEvent(TypedDict, total=False):
    kind: Literal["checkpoint_taken"]
    turn: int
    history_len: int


class EntropySampleEvent(TypedDict, total=False):
    kind: Literal["entropy_sample"]
    turn: int
    score: float
    score_version: int
    rho: float
    repeat_pressure: float
    failure_rate: float
    rollbacks_in_window: int
    window_turns: int


class EntropyAlertEvent(TypedDict, total=False):
    kind: Literal["entropy_alert"]
    turn: int
    score: float
    threshold: float


class AgentProcessChangedEvent(TypedDict, total=False):
    kind: Literal["agent_process_changed"]
    turn: int
    agent_id: str
    parent_session_id: str
    role: str
    isolation: str
    context_inheritance: str
    state: str
    permitted_capability_ids: list[str]
    result_termination: str


class PageOutEvent(TypedDict, total=False):
    kind: Literal["page_out"]
    turn: int
    action: str
    summary: str
    tier_hint: str
    message_count: int


class PageInEvent(TypedDict, total=False):
    kind: Literal["page_in"]
    turn: int
    entry_count: int


class LargeResultSpooledEvent(TypedDict, total=False):
    kind: Literal["large_result_spooled"]
    turn: int
    call_id: str
    tool: str
    original_size: int
    preview_size: int
    spool_ref: str


class SuspendedEvent(TypedDict, total=False):
    kind: Literal["suspended"]
    turn: int
    reason: str
    pending_calls: list[str]


class ResumedEvent(TypedDict, total=False):
    kind: Literal["resumed"]
    turn: int
    approved: list[str]
    denied: list[str]


class ToolGatedEvent(TypedDict, total=False):
    kind: Literal["tool_gated"]
    turn: int
    call_id: str
    tool: str
    reason: str


class SignalDisposedEvent(TypedDict, total=False):
    kind: Literal["signal_disposed"]
    turn: int
    signal_id: str
    disposition: str
    queue_depth: int


class BudgetExceededEvent(TypedDict, total=False):
    kind: Literal["budget_exceeded"]
    turn: int
    budget: str


class ContextRenewedEvent(TypedDict, total=False):
    kind: Literal["context_renewed"]
    turn: int
    sprint: int
    handoff_ref: str


class MemoryWrittenEvent(TypedDict, total=False):
    kind: Literal["memory_written"]
    turn: int
    memory_id: str
    memory_kind: str
    size_bytes: int


class MemoryQueriedEvent(TypedDict, total=False):
    kind: Literal["memory_queried"]
    turn: int
    query_context: str
    requested_k: int
    requires_async_response: bool


class MemoryValidationFailedEvent(TypedDict, total=False):
    kind: Literal["memory_validation_failed"]
    turn: int
    memory_id: str
    error: str


class MemoryRetrievalResultEvent(TypedDict, total=False):
    kind: Literal["memory_retrieval_result"]
    selected_memory_ids: list[str]
    selection_rationale: str


class WorkflowNodeCompletedEvent(TypedDict, total=False):
    kind: Literal["workflow_node_completed"]
    turn: int
    agent_id: str
    termination: str
    # W-1: result-borne control signals, persisted so resume replays control flow faithfully —
    # a classifier re-prunes its rejected branches, a recorded loop stop is honored.
    classify_branch: str
    tournament_winner: str
    loop_continue: bool
    # W-1: the node's final output text — resume re-seeds the driver's outputs map from it so
    # post-resume reduce/judge/dependent nodes still see their dependencies' outputs.
    output: str


class WorkflowBatchSpawnedEvent(TypedDict, total=False):
    kind: Literal["workflow_batch_spawned"]
    turn: int
    node_count: int
    node_ids: list[str]


class WorkflowCompletedEvent(TypedDict, total=False):
    kind: Literal["workflow_completed"]
    turn: int
    completed: list[str]
    failed: list[str]
    total_nodes: int


SessionEvent = (
    RunStartedEvent
    | LlmCompletedEvent
    | ToolRequestedEvent
    | ToolCompletedEvent
    | ToolArgumentRepairedEvent
    | ToolDeniedEvent
    | PermissionRequestedEvent
    | PermissionResolvedEvent
    | CompressedEvent
    | RollbackedEvent
    | CapabilityChangedEvent
    | MilestoneAdvancedEvent
    | MilestoneBlockedEvent
    | CheckpointTakenEvent
    | EntropySampleEvent
    | EntropyAlertEvent
    | AgentProcessChangedEvent
    | PageOutEvent
    | PageInEvent
    | LargeResultSpooledEvent
    | SuspendedEvent
    | ResumedEvent
    | ToolGatedEvent
    | SignalDisposedEvent
    | BudgetExceededEvent
    | ContextRenewedEvent
    | MemoryWrittenEvent
    | MemoryQueriedEvent
    | MemoryValidationFailedEvent
    | MemoryRetrievalResultEvent
    | WorkflowNodeCompletedEvent
    | WorkflowBatchSpawnedEvent
    | WorkflowCompletedEvent
    | RunTerminalEvent
)


@dataclass
class SessionEntry:
    seq: int
    event: SessionEvent


class SessionLog(Protocol):
    async def append(self, session_id: str, event: SessionEvent) -> int: ...
    async def read(
        self,
        session_id: str,
        from_seq: int = 0,
        primitive_filter: KernelPrimitive | None = None,
    ) -> list[SessionEntry]: ...
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

    async def read(
        self,
        session_id: str,
        from_seq: int = 0,
        primitive_filter: KernelPrimitive | None = None,
    ) -> list[SessionEntry]:
        entries = self._store.get(session_id, [])
        return [
            e for e in entries
            if e.seq >= from_seq
            and (primitive_filter is None or primitive_for_kind(e.event["kind"]) == primitive_filter)
        ]

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

    async def read(
        self,
        session_id: str,
        from_seq: int = 0,
        primitive_filter: KernelPrimitive | None = None,
    ) -> list[SessionEntry]:
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
                    if primitive_filter is not None and primitive_for_kind(entry.event["kind"]) != primitive_filter:
                        continue
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
        {
          "call_id": r.call_id,
          "output": r.output,
          "is_error": r.is_error,
          "is_fatal": getattr(r, "is_fatal", False),
          "error_kind": getattr(r, "error_kind", None),
          "token_count": r.token_count,
        }
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
    results = []
    for r in raw["results"]:
      result = ToolResult(
        call_id=r["call_id"],
        output=r["output"],
        is_error=r.get("is_error", False),
        token_count=r.get("token_count"),
      )
      if hasattr(result, "is_fatal"):
        result.is_fatal = r.get("is_fatal", False)
      if hasattr(result, "error_kind"):
        result.error_kind = r.get("error_kind")
      results.append(result)
    return {
      "kind": "tool_completed",
      "turn": raw["turn"],
      "results": results,
    }
  return raw  # type: ignore[return-value]
