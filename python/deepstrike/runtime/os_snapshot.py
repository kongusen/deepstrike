from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from deepstrike.runtime.kernel_event_log import (
    KernelEventCategory,
    category_for_kind,
    primitive_for_kind,
)

_KERNEL_KINDS = frozenset({
    "compressed",
    "page_out",
    "page_in",
    "large_result_spooled",
    "capability_changed",
    "context_renewed",
    "suspended",
    "resumed",
    "tool_gated",
    "signal_delivery_disposed",
    "budget_exceeded",
    "budget_usage_reported",
    "checkpoint_taken",
    "rollbacked",
    "agent_process_changed",
    "milestone_advanced",
    "milestone_blocked",
    "memory_written",
    "memory_queried",
    "memory_validation_failed",
})


@dataclass
class OsSnapshot:
    last_suspend: dict[str, Any] | None = None
    last_resumed_turn: int | None = None
    process_by_agent: list[dict[str, Any]] = field(default_factory=list)
    budget_exceeded: list[dict[str, Any]] = field(default_factory=list)
    budget_usage_reported: list[dict[str, Any]] = field(default_factory=list)
    signals: list[dict[str, Any]] = field(default_factory=list)
    page_out_count: int = 0
    page_in_count: int = 0
    spool_count: int = 0
    tool_gated_count: int = 0
    memory_written_count: int = 0
    memory_queried_count: int = 0
    memory_validation_failed_count: int = 0
    memory_retrieval_result_count: int = 0


def rebuild_os_snapshot_from_session_events(events: list[dict[str, Any]]) -> OsSnapshot:
    snap = OsSnapshot()
    index: dict[str, int] = {}
    for event in events:
        kind = event.get("kind")
        if kind == "memory_retrieval_result":
            snap.memory_retrieval_result_count += 1
            continue
        if kind not in _KERNEL_KINDS and kind not in ("suspended", "resumed"):
            continue
        if kind == "suspended":
            snap.last_suspend = {
                "turn": event.get("turn"),
                "reason": event.get("reason"),
                "pending_calls": event.get("pending_calls") or [],
            }
        elif kind == "resumed":
            snap.last_resumed_turn = event.get("turn")
        elif kind == "tool_gated":
            snap.tool_gated_count += 1
        elif kind == "agent_process_changed":
            record = {
                "turn": event.get("turn"),
                "agent_id": event.get("agent_id"),
                "parent_session_id": event.get("parent_session_id"),
                "state": event.get("state") or "running",
            }
            agent_id = event.get("agent_id") or ""
            idx = index.get(agent_id)
            if idx is not None:
                snap.process_by_agent[idx] = record
            else:
                index[agent_id] = len(snap.process_by_agent)
                snap.process_by_agent.append(record)
        elif kind == "budget_exceeded":
            snap.budget_exceeded.append({
                "turn": event.get("turn"),
                "operation_id": event.get("operation_id"),
                "reservation_id": event.get("reservation_id"),
                "budget": event.get("budget"),
            })
        elif kind == "budget_usage_reported":
            snap.budget_usage_reported.append({
                "turn": event.get("turn"),
                "operation_id": event.get("operation_id"),
                "reservation_id": event.get("reservation_id"),
                "tokens": event.get("tokens") or 0,
                "subagents": event.get("subagents") or 0,
                "rounds": event.get("rounds") or 0,
            })
        elif kind == "signal_delivery_disposed":
            snap.signals.append({
                "turn": event.get("turn"),
                "operation_id": event.get("operation_id"),
                "delivery_id": event.get("delivery_id"),
                "attempt": event.get("attempt"),
                "signal_id": event.get("signal_id"),
                "disposition": event.get("disposition"),
                "queue_depth": event.get("queue_depth"),
            })
        elif kind == "page_out":
            snap.page_out_count += 1
        elif kind == "page_in":
            snap.page_in_count += 1
        elif kind == "large_result_spooled":
            snap.spool_count += 1
        elif kind == "memory_written":
            snap.memory_written_count += 1
        elif kind == "memory_queried":
            snap.memory_queried_count += 1
        elif kind == "memory_validation_failed":
            snap.memory_validation_failed_count += 1
    return snap
