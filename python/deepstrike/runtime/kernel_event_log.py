from __future__ import annotations

from typing import Any, Callable, Literal

KernelEventCategory = Literal["syscall", "sched", "mm", "proc", "ipc"]


def category_for_kind(kind: str) -> KernelEventCategory:
    if kind in ("tool_gated", "capability_changed"):
        return "syscall"
    if kind in (
        "compressed",
        "page_out",
        "page_in",
        "page_in_requested",
        "renewed",
        "context_renewed",
        "large_result_spooled",
    ):
        return "mm"
    if kind == "agent_process_changed":
        return "proc"
    if kind == "signal_disposed":
        return "ipc"
    return "sched"


def with_category(event: dict[str, Any]) -> dict[str, Any]:
    return {**event, "category": category_for_kind(event["kind"])}


def kernel_observation_to_session_event(
    obs: dict[str, Any],
    turn: int,
    *,
    next_archive_start: int = 0,
    latest_seq: int | None = None,
    archive_ref: str | None = None,
    preserved_refs: list[str] | None = None,
    compression_action: Callable[[str | None], str | None] | None = None,
    spool_ref: str | None = None,
) -> dict[str, Any] | None:
    t = obs.get("turn") or turn
    to_action = compression_action or (lambda _a: None)

    kind = obs.get("kind")
    if kind == "page_out":
        archived = obs.get("archived")
        return with_category({
            "kind": "page_out",
            "turn": t,
            "action": to_action(obs.get("action")),
            "summary": obs.get("summary"),
            "tier_hint": obs.get("tier_hint") or "durable",
            "message_count": len(archived) if isinstance(archived, list) else 0,
        })
    if kind == "compressed":
        latest = latest_seq if latest_seq is not None else -1
        if latest < next_archive_start:
            return None
        summary = obs.get("summary")
        return with_category({
            "kind": "compressed",
            "turn": t,
            "archived_seq_range": (next_archive_start, latest),
            "action": to_action(obs.get("action")),
            "summary": summary,
            "summary_tokens": max(1, len(summary) // 4) if summary else None,
            "archive_ref": archive_ref,
            "preserved_refs": preserved_refs or [],
        })
    if kind == "renewed":
        return with_category({
            "kind": "context_renewed",
            "turn": t,
            "sprint": obs.get("sprint") or 0,
            "handoff_ref": "",
        })
    if kind == "rollbacked":
        return with_category({
            "kind": "rollbacked",
            "turn": t,
            "checkpoint_history_len": obs.get("checkpoint_history_len") or 0,
            "reason": obs.get("reason"),
        })
    if kind == "capability_changed":
        ev: dict[str, Any] = with_category({
            "kind": "capability_changed",
            "turn": t,
            "added": obs.get("added") or [],
            "removed": obs.get("removed") or [],
        })
        for key in ("change_kind", "capability_id", "version", "mounted_by", "mount_reason"):
            if obs.get(key) is not None:
                ev[key] = obs[key]
        return ev
    if kind == "milestone_advanced":
        return with_category({
            "kind": "milestone_advanced",
            "turn": t,
            "phase_id": obs.get("phase_id") or "",
            "capabilities_unlocked": obs.get("capabilities_unlocked") or [],
        })
    if kind == "milestone_blocked":
        return with_category({
            "kind": "milestone_blocked",
            "turn": t,
            "phase_id": obs.get("phase_id") or "",
            "reason": obs.get("reason") or "",
        })
    if kind == "milestone_evidence":
        return with_category({
            "kind": "milestone_evidence",
            "turn": t,
            "phase_id": obs.get("phase_id") or "",
            "evidence": obs.get("evidence") or [],
        })
    if kind == "checkpoint_taken":
        return with_category({
            "kind": "checkpoint_taken",
            "turn": t,
            "history_len": obs.get("history_len") or 0,
        })
    if kind == "agent_process_changed":
        ev = with_category({
            "kind": "agent_process_changed",
            "turn": t,
            "agent_id": obs.get("agent_id") or "",
            "parent_session_id": obs.get("parent_session_id") or "",
            "role": obs.get("role") or "",
            "isolation": obs.get("isolation") or "",
            "context_inheritance": obs.get("context_inheritance") or "",
            "state": obs.get("state") or "running",
            "permitted_capability_ids": obs.get("permitted_capability_ids") or [],
        })
        if obs.get("result_termination"):
            ev["result_termination"] = obs["result_termination"]
        return ev
    if kind == "tool_gated":
        return with_category({
            "kind": "tool_gated",
            "turn": t,
            "call_id": obs.get("call_id") or "",
            "tool": obs.get("tool") or "",
            "reason": obs.get("reason") or "",
        })
    if kind == "signal_disposed":
        return with_category({
            "kind": "signal_disposed",
            "turn": t,
            "signal_id": obs.get("signal_id") or "",
            "disposition": obs.get("disposition") or "",
            "queue_depth": obs.get("queue_depth") or 0,
        })
    if kind == "budget_exceeded":
        return with_category({
            "kind": "budget_exceeded",
            "turn": t,
            "budget": obs.get("budget") or "",
        })
    if kind == "suspended":
        return with_category({
            "kind": "suspended",
            "turn": t,
            "reason": obs.get("reason") or "",
            "pending_calls": obs.get("pending_calls") or [],
        })
    if kind == "resumed":
        return with_category({
            "kind": "resumed",
            "turn": t,
            "approved": obs.get("approved") or [],
            "denied": obs.get("denied") or [],
        })
    if kind == "page_in_requested":
        return None
    if kind == "large_result_spooled":
        return with_category({
            "kind": "large_result_spooled",
            "turn": t,
            "call_id": obs.get("call_id") or "",
            "tool": obs.get("tool") or "",
            "original_size": obs.get("original_size") or 0,
            "preview_size": obs.get("preview_size") or 0,
            "spool_ref": spool_ref,
        })
    return None
