from __future__ import annotations

import asyncio
import json
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal, Protocol, TypedDict

from deepstrike._kernel import ToolCall, ToolExecutionResult
from deepstrike.runtime.durable_content import decode_durable_content
from deepstrike.runtime._session_event_registry import (
    SESSION_EVENT_KINDS as SESSION_EVENT_KINDS,
    SESSION_EVENT_REGISTRY,
)
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


class _RunStartedRequired(TypedDict):
    kind: Literal["run_started"]
    run_id: str
    goal: str
    criteria: list[str]


class RunStartedEvent(_RunStartedRequired, total=False):
    agent_id: str
    system_prompt: str
    route: dict[str, Any]
    # Shared wire fields other SDKs write (canonical content parts / host run metadata); the Python
    # runner reads them back but does not populate them yet.
    attachments: list[dict[str, Any]]
    metadata: dict[str, Any]


class _LlmCompletedRequired(TypedDict):
    kind: Literal["llm_completed"]
    turn: int
    content: str
    tool_calls: list[ToolCall]


class LlmCompletedEvent(_LlmCompletedRequired, total=False):
    token_count: int
    effect_id: str
    invocation_id: str
    wire_evidence: dict[str, Any]


class _PromptMeasuredRequired(TypedDict):
    kind: Literal["prompt_measured"]
    turn: int
    measurement: dict[str, Any]


class PromptMeasuredEvent(_PromptMeasuredRequired, total=False):
    effect_id: str


class ContextPreparedEvent(TypedDict):
    kind: Literal["context_prepared"]
    turn: int
    effect_id: str
    preparation: dict[str, Any]


class _ProviderAttemptRequired(TypedDict):
    kind: Literal["provider_attempt"]
    effect_id: str
    attempt_seq: int
    route: dict[str, Any]
    request_fingerprint: str
    status: Literal["success", "transport_exhausted", "aborted", "rejected"]
    transport_rungs: int
    started_at_ms: int
    finished_at_ms: int


class ProviderAttemptEvent(_ProviderAttemptRequired, total=False):
    last_error_class: str
    usage: dict[str, Any]
    wire_evidence: dict[str, Any]
    accounting_policy_id: str


class ToolRequestedEvent(TypedDict):
    kind: Literal["tool_requested"]
    turn: int
    calls: list[ToolCall]


class _ToolCompletedRequired(TypedDict):
    kind: Literal["tool_completed"]
    turn: int
    results: list[ToolExecutionResult | dict[str, Any]]


class ToolCompletedEvent(_ToolCompletedRequired, total=False):
    effect_id: str


class ToolArgumentRepairedEvent(TypedDict):
    kind: Literal["tool_argument_repaired"]
    turn: int
    tool: str
    original_arguments: str
    repaired_arguments: str


class ToolDeniedEvent(TypedDict):
    kind: Literal["tool_denied"]
    turn: int
    call_id: str
    tool_name: str
    reason: str


class _PermissionRequestedRequired(TypedDict):
    kind: Literal["permission_requested"]
    turn: int
    tool: str
    arguments: str


class PermissionRequestedEvent(_PermissionRequestedRequired, total=False):
    reason: str


class PermissionResolvedEvent(TypedDict):
    kind: Literal["permission_resolved"]
    turn: int
    approved: bool
    responder: str


class _CompressedRequired(TypedDict):
    kind: Literal["compressed"]
    turn: int
    archived_seq_range: tuple[int, int]


class CompressedEvent(_CompressedRequired, total=False):
    action: str
    summary: str
    summary_tokens: int
    preserved_refs: list[str]


class RunTerminalEvent(TypedDict):
    kind: Literal["run_terminal"]
    reason: str
    turns_used: int
    total_tokens: int


class _RollbackedRequired(TypedDict):
    kind: Literal["rollbacked"]
    turn: int
    checkpoint_history_len: int


class RollbackedEvent(_RollbackedRequired, total=False):
    reason: RollbackReason


class _CapabilityChangedRequired(TypedDict):
    kind: Literal["capability_changed"]
    turn: int
    added: list[str]
    removed: list[str]


class CapabilityChangedEvent(_CapabilityChangedRequired, total=False):
    change_kind: str
    capability_id: str
    version: str
    mounted_by: str
    mount_reason: str


class MilestoneAdvancedEvent(TypedDict):
    kind: Literal["milestone_advanced"]
    turn: int
    phase_id: str
    capabilities_unlocked: list[str]


class MilestoneBlockedEvent(TypedDict):
    kind: Literal["milestone_blocked"]
    turn: int
    phase_id: str
    reason: str


class CheckpointTakenEvent(TypedDict):
    kind: Literal["checkpoint_taken"]
    turn: int
    history_len: int


class EntropySampleEvent(TypedDict):
    kind: Literal["entropy_sample"]
    turn: int
    score: float
    rho: float
    repeat_pressure: float
    failure_rate: float
    rollbacks_in_window: int
    window_turns: int


class EntropyAlertEvent(TypedDict):
    kind: Literal["entropy_alert"]
    turn: int
    score: float
    threshold: float


class _AgentProcessChangedRequired(TypedDict):
    kind: Literal["agent_process_changed"]
    turn: int
    agent_id: str
    role: str
    isolation: str
    context_inheritance: str
    permitted_capability_ids: list[str]


class AgentProcessChangedEvent(_AgentProcessChangedRequired, total=False):
    parent_task_id: str
    # Host audit identity; canonical kernel observations never populate it.
    parent_session_id: str
    state: str
    result_termination: str


class _PageOutRequired(TypedDict):
    kind: Literal["page_out"]
    turn: int


class PageOutEvent(_PageOutRequired, total=False):
    action: str
    summary: str
    tier_hint: str
    message_count: int
    archive_ref: str


class PageInEvent(TypedDict):
    kind: Literal["page_in"]
    turn: int
    entry_count: int


class _SemanticArchivePendingRequired(TypedDict):
    kind: Literal["semantic_archive_pending"]
    effect_id: str


class SemanticArchivePendingEvent(_SemanticArchivePendingRequired, total=False):
    action: str


class SemanticArchiveCompletedEvent(TypedDict):
    kind: Literal["semantic_archive_completed"]
    effect_id: str
    record_id: str


class SemanticArchiveFailedEvent(TypedDict):
    kind: Literal["semantic_archive_failed"]
    effect_id: str
    error: str


class _SuspendedRequired(TypedDict):
    kind: Literal["suspended"]
    turn: int
    reason: str


class SuspendedEvent(_SuspendedRequired, total=False):
    pending_calls: list[str]


class _ResumedRequired(TypedDict):
    kind: Literal["resumed"]
    turn: int


class ResumedEvent(_ResumedRequired, total=False):
    approved: list[str]
    denied: list[str]


class ToolGatedEvent(TypedDict):
    kind: Literal["tool_gated"]
    turn: int
    call_id: str
    tool: str
    reason: str


class SignalDeliveryDisposedEvent(TypedDict):
    kind: Literal["signal_delivery_disposed"]
    turn: int
    operation_id: str
    delivery_id: str
    attempt: int
    signal_id: str
    disposition: str
    queue_depth: int


class _BudgetExceededRequired(TypedDict):
    kind: Literal["budget_exceeded"]
    turn: int
    operation_id: str
    budget: str


class BudgetExceededEvent(_BudgetExceededRequired, total=False):
    reservation_id: str


class BudgetUsageReportedEvent(TypedDict):
    kind: Literal["budget_usage_reported"]
    turn: int
    operation_id: str
    reservation_id: str
    tokens: int
    subagents: int
    rounds: int


class OperationCancelledEvent(TypedDict):
    kind: Literal["operation_cancelled"]
    turn: int
    operation_id: str
    reason: Literal["user", "deadline", "lease_lost", "host_shutdown"]
    pending_call_ids: list[str]


class ContextRenewedEvent(TypedDict):
    kind: Literal["context_renewed"]
    turn: int
    sprint: int
    handoff_ref: str


class MemoryWrittenEvent(TypedDict):
    kind: Literal["memory_written"]
    turn: int
    record_id: str
    scope: dict[str, str]
    memory_kind: str
    name: str
    size_bytes: int


class MemoryQueriedEvent(TypedDict):
    kind: Literal["memory_queried"]
    turn: int
    scope: dict[str, str]
    query: str
    requested_k: int
    requires_async_response: bool


class MemoryValidationFailedEvent(TypedDict):
    kind: Literal["memory_validation_failed"]
    turn: int
    record_id: str
    error: str


class MemoryWriteFailedEvent(TypedDict):
    kind: Literal["memory_write_failed"]
    turn: int
    record_id: str
    error: str


class _MemoryCandidatePendingRequired(TypedDict):
    kind: Literal["memory_candidate_pending"]
    turn: int
    candidate_id: str
    source: str
    name: str


class MemoryCandidatePendingEvent(_MemoryCandidatePendingRequired, total=False):
    """M2 · a write handed to the candidate sink and held for review: nothing is remembered yet.

    ``receipt_id`` is absent when the fact comes from the kernel's own observation.
    """
    receipt_id: str


class MemoryCandidateRejectedEvent(TypedDict):
    """M2 · the candidate sink refused the write."""
    kind: Literal["memory_candidate_rejected"]
    turn: int
    candidate_id: str
    source: str
    name: str
    receipt_id: str
    reason_code: str


class MemoryQueryFailedEvent(TypedDict):
    kind: Literal["memory_query_failed"]
    turn: int
    scope: dict[str, str]
    query: str
    error: str


class MemoryRetrievalResultEvent(TypedDict):
    kind: Literal["memory_retrieval_result"]
    hits: list[dict[str, Any]]


class _WorkflowNodeCompletedRequired(TypedDict):
    kind: Literal["workflow_node_completed"]
    turn: int
    agent_id: str
    status: str
    termination: str


class WorkflowNodeCompletedEvent(_WorkflowNodeCompletedRequired, total=False):
    # W-1: result-borne control signals, persisted so resume replays control flow faithfully —
    # a classifier re-prunes its rejected branches, a recorded loop stop is honored.
    classify_branch: str
    tournament_winner: str
    loop_continue: bool
    output: dict[str, Any]


class WorkflowBatchSpawnedEvent(TypedDict):
    kind: Literal["workflow_batch_spawned"]
    turn: int
    node_count: int
    node_ids: list[str]


class WorkflowCompletedEvent(TypedDict):
    kind: Literal["workflow_completed"]
    turn: int
    node_outcomes: list[dict[str, Any]]
    total_nodes: int


class _WorkflowNodesSubmittedRequired(TypedDict):
    kind: Literal["workflow_nodes_submitted"]
    turn: int
    # Kernel-shape (snake_case) submitted node specs — persisted so resume can re-apply them.
    nodes: list[dict[str, Any]]


class WorkflowNodesSubmittedEvent(_WorkflowNodesSubmittedRequired, total=False):
    # R3-1: graph base index the batch was appended at — lets resume rebuild exact indices.
    base_index: int
    # W-N3: the submitting node's agent id (absent = host/bootstrap).
    submitter_agent_id: str


class KernelObservationEvent(TypedDict):
    kind: Literal["kernel_observation"]
    turn: int
    observation_kind: str
    raw: dict[str, Any]


class SummaryUpgradedEvent(TypedDict):
    kind: Literal["summary_upgraded"]
    compressed_seq: int
    summary: str


class _GroupMemberJoinedRequired(TypedDict):
    kind: Literal["group_member_joined"]
    session_id: str


class GroupMemberJoinedEvent(_GroupMemberJoinedRequired, total=False):
    role: str
    member_kind: Literal["peer", "vehicle"]


class _GroupBudgetChargedRequired(TypedDict):
    kind: Literal["group_budget_charged"]
    tokens: int
    subagents: int


class GroupBudgetChargedEvent(_GroupBudgetChargedRequired, total=False):
    rounds: int


class RoundStartedEvent(TypedDict):
    kind: Literal["round_started"]
    # 1-based round number within the loop.
    round: int
    goal: str


class _RoundPacedRequired(TypedDict):
    kind: Literal["round_paced"]
    round: int
    action: Literal["continue", "sleep", "stop"]
    reason: str


class RoundPacedEvent(_RoundPacedRequired, total=False):
    delay_ms: int
    # Absolute wake time for sleep — lets a stateless host re-arm from the log alone.
    wake_at_ms: int
    coerced_from: str


SessionEvent = (
    RunStartedEvent
    | LlmCompletedEvent
    | PromptMeasuredEvent
    | ContextPreparedEvent
    | ProviderAttemptEvent
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
    | SemanticArchivePendingEvent
    | SemanticArchiveCompletedEvent
    | SemanticArchiveFailedEvent
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
    | MemoryWriteFailedEvent
    | MemoryCandidatePendingEvent
    | MemoryCandidateRejectedEvent
    | MemoryQueryFailedEvent
    | MemoryRetrievalResultEvent
    | WorkflowNodeCompletedEvent
    | WorkflowNodesSubmittedEvent
    | WorkflowBatchSpawnedEvent
    | WorkflowCompletedEvent
    | KernelObservationEvent
    | RunTerminalEvent
    | SummaryUpgradedEvent
    | GroupMemberJoinedEvent
    | GroupBudgetChargedEvent
    | RoundStartedEvent
    | RoundPacedEvent
)


def _session_event_union_fields() -> dict[str, dict[str, tuple[str, ...]]]:
    from typing import get_args

    fields: dict[str, dict[str, tuple[str, ...]]] = {}
    for member in get_args(SessionEvent):
        # `from __future__ import annotations` leaves `kind` as a ForwardRef to 'Literal["..."]'.
        annotation = member.__annotations__["kind"]
        (kind,) = re.findall(r"[\"'](\w+)[\"']", getattr(annotation, "__forward_arg__", str(annotation)))
        fields[kind] = {
            "required": tuple(sorted(member.__required_keys__ - {"kind"})),
            "optional": tuple(sorted(member.__optional_keys__)),
        }
    return fields


# The registered session-event vocabulary comes from contracts/session-events.json (generated
# into `_session_event_registry`). The union must carry exactly those kinds, each with exactly the
# registered required/optional top-level fields, so a drift fails at import rather than on replay.
assert _session_event_union_fields() == SESSION_EVENT_REGISTRY, (
    "SessionEvent TypedDicts must match contracts/session-events.json exactly"
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
        raise ValueError("llm_completed has removed provider_replay field; use wire_evidence.replay_state")
    if event.get("kind") == "provider_attempt":
        _validate_provider_attempt_event(event)
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


def _validate_provider_attempt_event(event: dict[str, Any]) -> None:
    """P4 §3 teeth (node G2 parity): a persisted provider_attempt must carry the linkage the
    C6 cross-checks verify — the kernel-minted effect id and the request fingerprint binding it
    to a prompt_measured record. Nested objects keep the host-native spelling, so `route` is
    only required to be an object (the chain validator reads both spellings)."""
    effect_id = event.get("effect_id")
    if not isinstance(effect_id, str) or not effect_id:
        raise ValueError("provider_attempt effect_id is required")
    attempt_seq = event.get("attempt_seq")
    if isinstance(attempt_seq, bool) or not isinstance(attempt_seq, int) or attempt_seq < 1:
        raise ValueError("provider_attempt attempt_seq must be a positive integer")
    if not isinstance(event.get("route"), dict):
        raise ValueError("provider_attempt route must be an object")
    fingerprint = event.get("request_fingerprint")
    if not isinstance(fingerprint, str) or not fingerprint:
        raise ValueError("provider_attempt request_fingerprint is required")
    if event.get("status") not in ("success", "transport_exhausted", "aborted", "rejected"):
        raise ValueError("provider_attempt status must be success|transport_exhausted|aborted|rejected")
    rungs = event.get("transport_rungs")
    if isinstance(rungs, bool) or not isinstance(rungs, int) or rungs < 0:
        raise ValueError("provider_attempt transport_rungs must be a non-negative integer")


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
      # P4 §3 evidence fields — passthrough, never read by recovery.
      **({"effect_id": raw["effect_id"]} if raw.get("effect_id") is not None else {}),
      **({"invocation_id": raw["invocation_id"]} if raw.get("invocation_id") is not None else {}),
      **({"wire_evidence": raw["wire_evidence"]} if raw.get("wire_evidence") is not None else {}),
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
        # _event_to_json writes these; dropping them here lost them on read.
        "is_fatal": r.get("is_fatal", False),
        "error_kind": r.get("error_kind"),
        "content": r["content"],
      })
    return {
      "kind": "tool_completed",
      "turn": raw["turn"],
      "results": results,
      **({"effect_id": raw["effect_id"]} if raw.get("effect_id") is not None else {}),
    }
  return raw  # type: ignore[return-value]
