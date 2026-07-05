from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from deepstrike._kernel import ToolCall
from deepstrike.runtime.replay_sanitize import sanitize_replay_text


def _estimate_token_count(text: str) -> int:
  return max(1, len(text) // 4)


def normalize_llm_completed(event: dict[str, Any], max_bytes: int | None = None) -> dict[str, Any]:
  """Normalize a persisted llm_completed event for recovery.

  Content is sanitized and token_count backfilled, but the stored
  ``provider_replay`` envelope is passed through verbatim. This layer is
  provider-neutral and never synthesizes protocol-specific replay shapes
  (e.g. Anthropic ``native_blocks``); legacy reconstruction for a given
  protocol is the target provider's ``seed_provider_replay`` responsibility.
  """
  content = sanitize_replay_text(event.get("content", ""), max_bytes)
  tool_calls = list(event.get("tool_calls") or [])
  provider_replay = event.get("provider_replay")
  out: dict[str, Any] = {
    "kind": "llm_completed",
    "turn": event["turn"],
    "content": content,
    "tool_calls": tool_calls,
    "token_count": event.get("token_count") or _estimate_token_count(content),
  }
  if provider_replay:
    out["provider_replay"] = provider_replay
  return out


def repair_events_for_recovery(events: list[Any], max_bytes: int | None = None) -> list[Any]:
  from deepstrike.runtime.session_log import SessionEntry

  repaired: list[Any] = []
  for entry in events:
    event = entry.event if hasattr(entry, "event") else entry
    if event.get("kind") != "llm_completed":
      repaired.append(entry)
      continue
    normalized = normalize_llm_completed(event, max_bytes)
    if hasattr(entry, "seq"):
      repaired.append(SessionEntry(seq=entry.seq, event=normalized))
    else:
      repaired.append(normalized)
  return repaired


def build_llm_completed_event(
  *,
  turn: int,
  content: str,
  tool_calls: list[ToolCall],
  token_count: int | None = None,
  provider_replay: dict[str, Any] | None = None,
) -> dict[str, Any]:
  return normalize_llm_completed({
    "kind": "llm_completed",
    "turn": turn,
    "content": content,
    "tool_calls": tool_calls,
    "token_count": token_count,
    "provider_replay": provider_replay,
  })


def build_run_terminal_event(*, reason: str, turns_used: int, total_tokens: int) -> dict[str, Any]:
  return {
    "kind": "run_terminal",
    "reason": reason,
    "turns_used": max(0, turns_used),
    "total_tokens": max(0, total_tokens),
  }


def build_workflow_node_completed_event(
  *,
  turn: int,
  agent_id: str,
  termination: str,
  classify_branch: str | None = None,
  tournament_winner: str | None = None,
  loop_continue: bool | None = None,
  output: str | None = None,
) -> dict[str, Any]:
  """Build a workflow_node_completed event for persistence after a node finishes. W-1: carries the
  result-borne control signals + output so resume replays control flow and re-seeds outputs."""
  event: dict[str, Any] = {
    "kind": "workflow_node_completed",
    "turn": turn,
    "agent_id": agent_id,
    "termination": termination,
  }
  if classify_branch is not None:
    event["classify_branch"] = classify_branch
  if tournament_winner is not None:
    event["tournament_winner"] = tournament_winner
  if loop_continue is not None:
    event["loop_continue"] = loop_continue
  if output:
    event["output"] = output
  return event


@dataclass
class RecoveredNodeCompletion:
  """One recovered node completion: the agent id plus its persisted control signals and output."""

  agent_id: str
  classify_branch: str | None = None
  tournament_winner: str | None = None
  loop_continue: bool | None = None
  output: str | None = None


def recover_completed_workflow_nodes(events: list[Any]) -> list[RecoveredNodeCompletion]:
  """Recover completed workflow node records from a session event stream. Scans for
  workflow_node_completed events with termination "completed" and returns them WITH their
  result-borne control signals (W-1) — resume_workflow lowers these to the kernel's
  ``resumed_results`` so a classifier re-prunes and a loop stop is honored, and re-seeds the
  driver's outputs map from the persisted output text."""
  completed: list[RecoveredNodeCompletion] = []
  for entry in events:
    event = entry.event if hasattr(entry, "event") else entry
    if event.get("kind") == "workflow_node_completed" and event.get("termination") == "completed":
      agent_id = event.get("agent_id")
      if agent_id:
        completed.append(RecoveredNodeCompletion(
          agent_id=agent_id,
          classify_branch=event.get("classify_branch"),
          tournament_winner=event.get("tournament_winner"),
          loop_continue=event.get("loop_continue"),
          output=event.get("output"),
        ))
  return completed


def build_workflow_nodes_submitted_event(
  *, turn: int, nodes: list, base_index: int | None = None, submitter_agent_id: str | None = None,
) -> dict[str, Any]:
  """R3-1: build a workflow_nodes_submitted event for persistence after a runtime submission, so
  resume can re-apply it. ``nodes`` is the kernel-shape (snake_case) submitted node array;
  ``base_index`` is the kernel-reported graph position (WorkflowNodesSubmitted observation).
  W-N3: ``submitter_agent_id`` is the submitting node's agent id (absent = host/bootstrap) —
  resume DROPS batches whose submitter re-runs (it will re-submit) instead of duplicating."""
  event: dict[str, Any] = {"kind": "workflow_nodes_submitted", "turn": turn, "nodes": nodes}
  if base_index is not None:
    event["base_index"] = base_index
  if submitter_agent_id is not None:
    event["submitter_agent_id"] = submitter_agent_id
  return event


def recover_submitted_workflow_nodes(events: list[Any]) -> tuple[list, list[int], list[str | None]]:
  """R3-1: recover the runtime submission batches (in order) plus their recorded base indices.
  A mixed log (some records without ``base_index``) degrades to order-only replay (empty bases).
  W-N3: ``submitters`` is parallel to ``submissions`` (None = host/bootstrap submission)."""
  submissions: list = []
  bases: list[int] = []
  submitters: list[str | None] = []
  for entry in events:
    event = entry.event if hasattr(entry, "event") else entry
    if event.get("kind") == "workflow_nodes_submitted":
      submissions.append(event.get("nodes") or [])
      submitters.append(event.get("submitter_agent_id"))
      if event.get("base_index") is not None:
        bases.append(int(event["base_index"]))
  return submissions, (bases if len(bases) == len(submissions) else []), submitters
