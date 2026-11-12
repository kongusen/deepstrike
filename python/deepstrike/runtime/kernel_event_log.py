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
        "lifecycle_report",
        "context_lifecycle_reported",
        "memory_written",
        "memory_queried",
        "memory_validation_failed",
        "memory_candidate_pending",
        "memory_candidate_rejected",
    ):
        return "mm"
    if kind == "agent_process_changed":
        return "proc"
    if kind == "signal_delivery_disposed":
        return "ipc"
    return "sched"


KernelPrimitive = Literal["syscall", "sched", "mm"]


def primitive_for_category(category: KernelEventCategory) -> KernelPrimitive:
    if category == "syscall":
        return "syscall"
    if category == "mm":
        return "mm"
    return "sched"


def primitive_for_kind(kind: str) -> KernelPrimitive:
    return primitive_for_category(category_for_kind(kind))


def kernel_observation_to_session_event(
    obs: dict[str, Any],
    turn: int,
    *,
    next_archive_start: int = 0,
    latest_seq: int | None = None,
    preserved_refs: list[str] | None = None,
    compression_action: Callable[[str | None], str | None] | None = None,
) -> dict[str, Any] | None:
    """Project a kernel observation into its durable session event (cross-SDK fixture
    ``kernel-observation-projection.json``). Absent optional fields are omitted, never ``None``."""
    event = _project_kernel_observation(
        obs,
        turn,
        next_archive_start=next_archive_start,
        latest_seq=latest_seq,
        preserved_refs=preserved_refs,
        compression_action=compression_action,
    )
    if event is None:
        return None
    return {key: value for key, value in event.items() if value is not None}


_COMPRESSION_ACTIONS = frozenset({"snip_compact", "micro_compact", "context_collapse", "auto_compact"})


def canonical_compression_action(action: str | None) -> str | None:
    return action if action in _COMPRESSION_ACTIONS else None


def _get(obs: dict[str, Any], key: str, default: Any) -> Any:
    # `??` semantics: only an absent / null value falls back (0, "" and False are kept).
    value = obs.get(key)
    return default if value is None else value


def _text(obs: dict[str, Any], key: str) -> str:
    value = obs.get(key)
    return value if isinstance(value, str) else ""


def _project_kernel_observation(
    obs: dict[str, Any],
    turn: int,
    *,
    next_archive_start: int,
    latest_seq: int | None,
    preserved_refs: list[str] | None,
    compression_action: Callable[[str | None], str | None] | None,
) -> dict[str, Any] | None:
    t = _get(obs, "turn", turn)
    to_action = compression_action or canonical_compression_action

    kind = obs.get("kind")
    if kind == "compressed":
        latest = latest_seq if latest_seq is not None else -1
        if latest < next_archive_start:
            return None
        summary = obs.get("summary")
        return {
            "kind": "compressed",
            "turn": t,
            "archived_seq_range": (next_archive_start, latest),
            "action": to_action(obs.get("action")),
            "summary": summary,
            "summary_tokens": max(1, -(-len(summary) // 4)) if summary else None,
            "preserved_refs": preserved_refs if preserved_refs is not None else [],
        }
    if kind == "renewed":
        return {
            "kind": "context_renewed",
            "turn": t,
            "sprint": _get(obs, "sprint", 0),
            "handoff_ref": "",
        }
    if kind == "lifecycle_report":
        event = {
            "kind": "context_lifecycle_reported",
            "turn": t,
            "knowledge_entries": _get(obs, "knowledge_entries", 0),
            "knowledge_referenced": _get(obs, "knowledge_referenced", 0),
            "regret": _get(obs, "regret", 0),
            "waste_tokens": _get(obs, "waste_tokens", 0),
            "cache_loss_tokens": _get(obs, "cache_loss_tokens", 0),
            "recalled": _get(obs, "recalled", 0),
            "recall_expanded": _get(obs, "recall_expanded", 0),
        }
        for field in ("items", "history", "history_items", "edges", "waste_items", "external_edges",
                      "parent_items_used", "prefix_breaks"):
            rows = _get(obs, field, None)
            if rows:
                event[field] = list(rows)
        for field in ("history_items_omitted", "edges_omitted", "prefix_breaks_omitted",
                      "cache_tail_tokens", "notes_written", "notes_active"):
            if _get(obs, field, 0):
                event[field] = obs[field]
        return event
    if kind == "rollbacked":
        return {
            "kind": "rollbacked",
            "turn": t,
            "checkpoint_history_len": _get(obs, "checkpoint_history_len", 0),
            "reason": obs.get("reason"),
        }
    if kind == "capability_changed":
        ev: dict[str, Any] = {
            "kind": "capability_changed",
            "turn": t,
            "added": _get(obs, "added", []),
            "removed": _get(obs, "removed", []),
        }
        for key in ("change_kind", "capability_id", "version", "mounted_by", "mount_reason"):
            if obs.get(key) is not None:
                ev[key] = obs[key]
        return ev
    if kind == "milestone_advanced":
        return {
            "kind": "milestone_advanced",
            "turn": t,
            "phase_id": _get(obs, "phase_id", ""),
            "capabilities_unlocked": _get(obs, "capabilities_unlocked", []),
        }
    if kind == "milestone_blocked":
        return {
            "kind": "milestone_blocked",
            "turn": t,
            "phase_id": _get(obs, "phase_id", ""),
            "reason": _text(obs, "reason"),
        }
    if kind == "checkpoint_taken":
        return {
            "kind": "checkpoint_taken",
            "turn": t,
            "history_len": _get(obs, "history_len", 0),
        }
    if kind == "entropy_sample":
        return {
            "kind": "entropy_sample",
            "turn": t,
            "score": _get(obs, "score", 0.0),
            "rho": _get(obs, "rho", 0.0),
            "repeat_pressure": _get(obs, "repeat_pressure", 0.0),
            "failure_rate": _get(obs, "failure_rate", 0.0),
            "rollbacks_in_window": _get(obs, "rollbacks_in_window", 0),
            "window_turns": _get(obs, "window_turns", 0),
        }
    if kind == "entropy_alert":
        return {
            "kind": "entropy_alert",
            "turn": t,
            "score": _get(obs, "score", 0.0),
            "threshold": _get(obs, "threshold", 0.0),
        }
    if kind == "agent_process_changed":
        ev = {
            "kind": "agent_process_changed",
            "turn": t,
            "agent_id": _get(obs, "agent_id", ""),
            "parent_task_id": _get(obs, "parent_task_id", ""),
            "role": _get(obs, "role", ""),
            "isolation": _get(obs, "isolation", ""),
            "context_inheritance": _get(obs, "context_inheritance", ""),
            "state": _get(obs, "state", "running"),
            "permitted_capability_ids": _get(obs, "permitted_capability_ids", []),
        }
        if obs.get("result_termination"):
            ev["result_termination"] = obs["result_termination"]
        return ev
    if kind == "tool_gated":
        return {
            "kind": "tool_gated",
            "turn": t,
            "call_id": _get(obs, "call_id", ""),
            "tool": _get(obs, "tool", ""),
            "reason": _text(obs, "reason"),
        }
    if kind == "signal_delivery_disposed":
        return {
            "kind": "signal_delivery_disposed",
            "turn": t,
            "operation_id": _get(obs, "operation_id", ""),
            "delivery_id": _get(obs, "delivery_id", ""),
            "attempt": _get(obs, "attempt", 0),
            "signal_id": _get(obs, "signal_id", ""),
            "disposition": _get(obs, "disposition", ""),
            "queue_depth": _get(obs, "queue_depth", 0),
        }
    if kind == "budget_exceeded":
        return {
            "kind": "budget_exceeded",
            "turn": t,
            "operation_id": _get(obs, "operation_id", ""),
            "reservation_id": obs.get("reservation_id") or None,
            "budget": _get(obs, "budget", ""),
        }
    if kind == "budget_usage_reported":
        return {
            "kind": "budget_usage_reported",
            "turn": t,
            "operation_id": _get(obs, "operation_id", ""),
            "reservation_id": _get(obs, "reservation_id", ""),
            "tokens": _get(obs, "tokens", 0),
            "subagents": _get(obs, "subagents", 0),
            "rounds": _get(obs, "rounds", 0),
        }
    if kind == "operation_cancelled":
        return {
            "kind": "operation_cancelled",
            "turn": t,
            "operation_id": _get(obs, "operation_id", ""),
            "reason": _get(obs, "reason", "user"),
            "pending_call_ids": _get(obs, "pending_call_ids", []),
        }
    if kind == "suspended":
        return {
            "kind": "suspended",
            "turn": t,
            "reason": _text(obs, "reason"),
            "pending_calls": _get(obs, "pending_calls", []),
        }
    if kind == "resumed":
        return {
            "kind": "resumed",
            "turn": t,
            "approved": _get(obs, "approved", []),
            "denied": _get(obs, "denied", []),
        }
    if kind == "page_in_requested":
        return None
    if kind == "page_out_archived":
        return {
            "kind": "page_out",
            "turn": t,
            "action": to_action(obs.get("action")),
            "summary": obs.get("summary"),
            "tier_hint": obs.get("tier"),
            "message_count": _get(obs, "message_count", 0),
            "archive_ref": obs.get("archive_ref"),
        }
    if kind == "memory_written":
        return {
            "kind": "memory_written",
            "turn": t,
            "record_id": _get(obs, "record_id", ""),
            "scope": _get(obs, "scope", {"tenant_id": "", "namespace": ""}),
            "memory_kind": _get(obs, "memory_kind", ""),
            "name": _get(obs, "name", ""),
            "size_bytes": _get(obs, "size_bytes", 0),
        }
    if kind == "memory_queried":
        return {
            "kind": "memory_queried",
            "turn": t,
            "scope": _get(obs, "scope", {"tenant_id": "", "namespace": ""}),
            "query": _get(obs, "query", ""),
            "requested_k": _get(obs, "requested_k", 0),
            "requires_async_response": _get(obs, "requires_async_response", False),
        }
    if kind == "memory_candidate_pending":
        # M2 · the kernel's PersistMemory effect settled as a candidate (the child-request path).
        return {
            "kind": "memory_candidate_pending",
            "turn": t,
            "candidate_id": str(_get(obs, "candidate_id", "")),
            "source": "child_request",
            "name": _get(obs, "name", ""),
        }
    if kind == "memory_validation_failed":
        return {
            "kind": "memory_validation_failed",
            "turn": t,
            "record_id": _get(obs, "record_id", ""),
            "error": _get(obs, "error", ""),
        }
    if kind == "memory_write_failed":
        return {
            "kind": "memory_write_failed",
            "turn": t,
            "record_id": _get(obs, "record_id", ""),
            "error": _get(obs, "error", ""),
        }
    if kind == "memory_query_failed":
        return {
            "kind": "memory_query_failed",
            "turn": t,
            "scope": _get(obs, "scope", {"tenant_id": "", "namespace": ""}),
            "query": _get(obs, "query", ""),
            "error": _get(obs, "error", ""),
        }
    if kind == "workflow_batch_spawned":
        nodes = list(_get(obs, "nodes", []))
        return {
            "kind": "workflow_batch_spawned",
            "turn": t,
            "node_count": len(nodes),
            "node_ids": [str(node.get("agent_id") or "") for node in nodes if isinstance(node, dict)],
        }
    if kind == "workflow_completed":
        node_outcomes = list(_get(obs, "node_outcomes", []))
        return {
            "kind": "workflow_completed",
            "turn": t,
            "node_outcomes": node_outcomes,
            "total_nodes": len(node_outcomes),
        }
    # Unknown / control-plane observations stay durable as opaque kernel_observation envelopes
    # (mirrors Node) so audit consumers can see control_request_rejected and similar facts.
    return {
        "kind": "kernel_observation",
        "turn": t,
        "observation_kind": kind,
        "raw": dict(obs),
    }
