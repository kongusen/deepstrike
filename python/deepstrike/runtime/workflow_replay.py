"""Durable workflow replay projections.

The Kernel persists workflow lifecycle events in the session log.  This module
turns those events into a small, provider independent inspection object that
hosts can use to rebuild workflow state or present an audit trail.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Iterable


@dataclass(frozen=True)
class WorkflowReplay:
    """Ordered lifecycle events and the last known outcome for each node."""

    events: tuple[dict[str, Any], ...]
    node_outcomes: tuple[dict[str, Any], ...]
    completed: bool

    @classmethod
    def from_entries(cls, entries: Iterable[Any]) -> "WorkflowReplay":
        kinds = {
            "workflow_batch_spawned",
            "workflow_node_completed",
            "workflow_nodes_submitted",
            "workflow_completed",
        }
        events: list[dict[str, Any]] = []
        outcomes: dict[str, dict[str, Any]] = {}
        completed = False
        for entry in entries:
            event = entry.get("event") if isinstance(entry, dict) and "event" in entry else getattr(entry, "event", entry)
            if not isinstance(event, dict) or event.get("kind") not in kinds:
                continue
            event = dict(event)
            events.append(event)
            if event.get("kind") == "workflow_node_completed":
                node_id = event.get("agent_id") or event.get("node_id")
                if node_id is not None:
                    outcomes[str(node_id)] = event
            elif event.get("kind") == "workflow_completed":
                completed = True
                for outcome in event.get("node_outcomes", []):
                    if isinstance(outcome, dict):
                        node_id = outcome.get("agent_id") or outcome.get("node_id")
                        if node_id is not None:
                            outcomes[str(node_id)] = dict(outcome)
        return cls(tuple(events), tuple(outcomes.values()), completed)

    def node(self, node_id: str) -> dict[str, Any] | None:
        return next((item for item in self.node_outcomes if str(item.get("agent_id") or item.get("node_id")) == node_id), None)
