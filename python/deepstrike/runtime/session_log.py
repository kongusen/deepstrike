from __future__ import annotations

import asyncio
import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal, Protocol, TypedDict

from deepstrike._kernel import ToolCall, ToolResult
from deepstrike.runtime.durable_content import decode_durable_content
from deepstrike.runtime.kernel_event_log import (
    primitive_for_kind,
)
from deepstrike.runtime.kernel_journal import (
    FileKernelJournal,
    InMemoryKernelJournal,
    KernelJournal,
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


class PromptMeasuredEvent(TypedDict, total=False):
    kind: Literal["prompt_measured"]
    turn: int
    measurement: dict[str, Any]


class ToolRequestedEvent(TypedDict, total=False):
    kind: Literal["tool_requested"]
    turn: int
    calls: list[ToolCall]


class ToolCompletedEvent(TypedDict, total=False):
    kind: Literal["tool_completed"]
    turn: int
    results: list[ToolResult | dict[str, Any]]


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
    parent_task_id: str
    # Host audit identity; canonical kernel observations never populate it.
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
    archive_ref: str


class PageInEvent(TypedDict, total=False):
    kind: Literal["page_in"]
    turn: int
    entry_count: int


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


class SignalDeliveryDisposedEvent(TypedDict, total=False):
    kind: Literal["signal_delivery_disposed"]
    turn: int
    operation_id: str
    delivery_id: str
    attempt: int
    signal_id: str
    disposition: str
    queue_depth: int


class BudgetExceededEvent(TypedDict, total=False):
    kind: Literal["budget_exceeded"]
    turn: int
    operation_id: str
    reservation_id: str
    budget: str


class BudgetUsageReportedEvent(TypedDict, total=False):
    kind: Literal["budget_usage_reported"]
    turn: int
    operation_id: str
    reservation_id: str
    tokens: int
    subagents: int
    rounds: int


class OperationCancelledEvent(TypedDict, total=False):
    kind: Literal["operation_cancelled"]
    turn: int
    operation_id: str
    reason: Literal["user", "deadline", "lease_lost", "host_shutdown"]
    pending_call_ids: list[str]


class ContextRenewedEvent(TypedDict, total=False):
    kind: Literal["context_renewed"]
    turn: int
    sprint: int
    handoff_ref: str


class MemoryWrittenEvent(TypedDict, total=False):
    kind: Literal["memory_written"]
    turn: int
    record_id: str
    scope: dict[str, str]
    memory_kind: str
    name: str
    size_bytes: int


class MemoryQueriedEvent(TypedDict, total=False):
    kind: Literal["memory_queried"]
    turn: int
    scope: dict[str, str]
    query: str
    requested_k: int
    requires_async_response: bool


class MemoryValidationFailedEvent(TypedDict, total=False):
    kind: Literal["memory_validation_failed"]
    turn: int
    record_id: str
    error: str


class MemoryRetrievalResultEvent(TypedDict, total=False):
    kind: Literal["memory_retrieval_result"]
    hits: list[dict[str, Any]]


class WorkflowNodeCompletedEvent(TypedDict, total=False):
    kind: Literal["workflow_node_completed"]
    turn: int
    agent_id: str
    status: str
    termination: str
    # W-1: result-borne control signals, persisted so resume replays control flow faithfully —
    # a classifier re-prunes its rejected branches, a recorded loop stop is honored.
    classify_branch: str
    tournament_winner: str
    loop_continue: bool
    output: dict[str, Any]


class WorkflowBatchSpawnedEvent(TypedDict, total=False):
    kind: Literal["workflow_batch_spawned"]
    turn: int
    node_count: int
    node_ids: list[str]


class WorkflowCompletedEvent(TypedDict, total=False):
    kind: Literal["workflow_completed"]
    turn: int
    node_outcomes: list[dict[str, Any]]
    total_nodes: int


SessionEvent = (
    RunStartedEvent
    | LlmCompletedEvent
    | PromptMeasuredEvent
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
    | SuspendedEvent
    | ResumedEvent
    | ToolGatedEvent
    | SignalDeliveryDisposedEvent
    | BudgetExceededEvent
    | BudgetUsageReportedEvent
    | OperationCancelledEvent
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
    """The business-projection log (spec §9.2): run started/terminal, stream events, observations,
    provider/tool presentation, and audit metadata. Canonical durable records live exclusively in
    :class:`~deepstrike.runtime.kernel_journal.KernelJournal` (spec §9.1).
    """

    async def append(self, session_id: str, event: SessionEvent) -> int: ...
    async def read(
        self,
        session_id: str,
        from_seq: int = 0,
        primitive_filter: KernelPrimitive | None = None,
    ) -> list[SessionEntry]: ...
    async def latest_seq(self, session_id: str) -> int: ...


class InMemorySessionLog:
    """**Single-process dev/test implementation** of both capabilities (spec §9.4: one class may
    provide several capabilities; the *interfaces* stay separate). Its ``KernelJournal`` half is
    :class:`InMemoryKernelJournal`, whose CAS is atomic within one process only.
    """

    def __init__(self) -> None:
        self._store: dict[str, list[SessionEntry]] = {}
        #: Business event sequence space only — journal records number themselves by `step_seq`.
        self._seq_counters: dict[str, int] = {}
        #: The durable transaction capability, held rather than inherited (spec §9.1/§9.4).
        self.kernel_journal: KernelJournal = InMemoryKernelJournal()

    def _next_seq(self, session_id: str) -> int:
        seq = self._seq_counters.get(session_id, 0)
        self._seq_counters[session_id] = seq + 1
        return seq

    async def append(self, session_id: str, event: SessionEvent) -> int:
        if session_id not in self._store:
            self._store[session_id] = []
        seq = self._next_seq(session_id)
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
        return self._seq_counters.get(session_id, 0) - 1

class FileSessionLog:
    """File-backed ``SessionLog``. Business appends are single-writer per session: safe within one
    instance, **not** across processes — that limitation is confined to the projection log now.

    The durable transaction capability is delegated to :class:`FileKernelJournal`, which *is*
    cross-process atomic (spec Task 8b), so the two capabilities no longer share a file, a sequence
    space, or a concurrency story.
    """

    def __init__(self, directory: str | Path) -> None:
        self._dir = Path(directory)
        # Lazy-initialized per-session counter for business events only.
        self._seq_counters: dict[str, int] = {}
        self._locks: dict[str, asyncio.Lock] = {}
        #: The durable transaction capability, held rather than inherited (spec §9.1/§9.4).
        self.kernel_journal: KernelJournal = FileKernelJournal(self._dir / "kernel-journal")

    def _path(self, session_id: str) -> Path:
        return self._dir / f"{session_id}.jsonl"

    def _lock(self, session_id: str) -> asyncio.Lock:
        return self._locks.setdefault(session_id, asyncio.Lock())

    def _next_seq(self, session_id: str) -> int:
        if session_id not in self._seq_counters:
            existing = self._read_records(session_id)
            self._seq_counters[session_id] = max(
                (int(record["seq"]) + 1 for record in existing),
                default=0,
            )
        seq = self._seq_counters[session_id]
        self._seq_counters[session_id] = seq + 1
        return seq

    async def append(self, session_id: str, event: SessionEvent) -> int:
        async with self._lock(session_id):
            seq = self._next_seq(session_id)
            self._append_record(session_id, {"seq": seq, "event": _event_to_json(event)})
            return seq

    async def read(
        self,
        session_id: str,
        from_seq: int = 0,
        primitive_filter: KernelPrimitive | None = None,
    ) -> list[SessionEntry]:
        results: list[SessionEntry] = []
        for raw in self._read_records(session_id):
            if "event" not in raw:
                continue
            entry = SessionEntry(seq=int(raw["seq"]), event=_event_from_json(raw["event"]))
            if entry.seq >= from_seq:
                if primitive_filter is not None and primitive_for_kind(entry.event["kind"]) != primitive_filter:
                    continue
                results.append(entry)
        return results

    async def latest_seq(self, session_id: str) -> int:
        records = self._read_records(session_id)
        return max((int(record["seq"]) for record in records), default=-1)

    def _append_record(self, session_id: str, record: dict) -> None:
        self._dir.mkdir(parents=True, exist_ok=True)
        path = self._path(session_id)
        is_new_file = not path.exists()
        line = json.dumps(record, ensure_ascii=False, separators=(",", ":"))
        with path.open("a", encoding="utf-8") as file:
            file.write(line + "\n")
            file.flush()
            os.fsync(file.fileno())
        if is_new_file:
            directory_fd = os.open(self._dir, os.O_RDONLY)
            try:
                os.fsync(directory_fd)
            finally:
                os.close(directory_fd)

    def _read_records(self, session_id: str) -> list[dict]:
        """Read only the canonical ``{seq, event}`` session-record shape."""
        path = self._path(session_id)
        if not path.exists():
            return []
        with path.open(encoding="utf-8") as file:
            return [_decode_persisted_session_record(json.loads(line)) for line in file if line.strip()]


def _decode_persisted_session_record(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("session record must be an object")
    if set(value) != {"seq", "event"}:
        raise ValueError("session record must contain only seq and event")
    seq = value["seq"]
    if isinstance(seq, bool) or not isinstance(seq, int) or seq < 0:
        raise ValueError("session record seq must be a non-negative integer")
    event = value["event"]
    if not isinstance(event, dict):
        raise ValueError("session record event must be an object")
    if event.get("kind") == "llm_completed" and "provider_replay" in event:
        from deepstrike.runtime.provider_replay import is_replay_compatible_with_provider
        is_replay_compatible_with_provider(event["provider_replay"], None)
    if event.get("kind") == "tool_completed":
        results = event.get("results")
        if not isinstance(results, list):
            raise ValueError("tool_completed results must be an array")
        for result in results:
            if not isinstance(result, dict):
                raise ValueError("tool_completed result must be an object")
            if "blocks" in result:
                raise ValueError("tool_completed result has removed blocks field")
            decode_durable_content(result.get("content"))
    return value


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
    for result in event["results"]:
      if not isinstance(result, dict) or not isinstance(result.get("content"), dict):
        raise ValueError("tool_completed results require canonical content blocks")
    return {
      **event,
      "results": [
        {
          "call_id": r["call_id"] if isinstance(r, dict) else r.call_id,
          "output": r["output"] if isinstance(r, dict) else r.output,
          "is_error": r.get("is_error", False) if isinstance(r, dict) else r.is_error,
          "is_fatal": r.get("is_fatal", False) if isinstance(r, dict) else getattr(r, "is_fatal", False),
          "error_kind": r.get("error_kind") if isinstance(r, dict) else getattr(r, "error_kind", None),
          "token_count": r.get("token_count") if isinstance(r, dict) else getattr(r, "token_count", None),
          "content": r["content"],
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
      if not isinstance(r.get("content"), dict):
        raise ValueError("tool_completed results require canonical content blocks")
      results.append({
        "call_id": r["call_id"], "output": r.get("output", ""),
        "is_error": r.get("is_error", False), "token_count": r.get("token_count"),
        "content": r["content"],
      })
    return {
      "kind": "tool_completed",
      "turn": raw["turn"],
      "results": results,
    }
  return raw  # type: ignore[return-value]
