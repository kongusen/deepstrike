from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Literal

KernelAgentRole = Literal["explore", "plan", "implement", "verify", "custom"]
AgentIsolation = Literal["shared", "read_only", "worktree", "remote"]
ContextInheritance = Literal["none", "system_only", "full"]
TerminationReason = Literal[
  "completed", "max_turns", "token_budget", "timeout", "user_abort", "error", "milestone_exceeded",
]
MilestonePolicy = Literal["require_verifier", "terminate", "auto_pass"]


@dataclass
class AgentIdentity:
  agent_id: str
  session_id: str
  is_sub_agent: bool = False
  parent_session_id: str | None = None


@dataclass
class AgentCapabilityFilter:
  allowed_kinds: list[str] = field(default_factory=list)
  allowed_ids: list[str] = field(default_factory=list)


@dataclass
class MilestonePhase:
  id: str
  criteria: list[str] = field(default_factory=list)
  unlocks: list[dict[str, Any]] = field(default_factory=list)
  verifier: dict[str, Any] | None = None
  required_evidence: list[str] = field(default_factory=list)


@dataclass
class MilestoneContract:
  phases: list[MilestonePhase]


@dataclass
class AgentRunSpec:
  identity: AgentIdentity
  role: KernelAgentRole
  goal: str
  isolation: AgentIsolation = "shared"
  verification_contract_id: str | None = None
  capability_filter: AgentCapabilityFilter | None = None
  milestones: MilestoneContract | None = None
  metadata: dict[str, Any] | None = None


@dataclass
class AgentProcessChangedObservation:
  agent_id: str
  parent_session_id: str
  role: str
  isolation: str
  context_inheritance: str
  permitted_capability_ids: list[str]
  turn: int | None = None
  state: str = "running"
  result_termination: str | None = None
  kind: str = "agent_process_changed"


@dataclass
class LoopResult:
  termination: str
  turns_used: int
  total_tokens_used: int
  final_message: Any | None = None


@dataclass
class SubAgentResult:
  agent_id: str
  result: LoopResult


@dataclass
class MilestoneCheckResult:
  phase_id: str
  passed: bool
  reason: str | None = None


def agent_identity_sub(agent_id: str, session_id: str, parent_session_id: str | None = None) -> AgentIdentity:
  return AgentIdentity(
    agent_id=agent_id,
    session_id=session_id,
    is_sub_agent=True,
    parent_session_id=parent_session_id,
  )


def agent_run_spec_to_kernel(spec: AgentRunSpec) -> dict[str, Any]:
  cap = spec.capability_filter or AgentCapabilityFilter()
  out: dict[str, Any] = {
    "identity": {
      "agent_id": spec.identity.agent_id,
      "session_id": spec.identity.session_id,
      "is_sub_agent": spec.identity.is_sub_agent,
      **({"parent_session_id": spec.identity.parent_session_id} if spec.identity.parent_session_id else {}),
    },
    "role": spec.role,
    "isolation": spec.isolation,
    "goal": spec.goal,
    "capability_filter": {
      "allowed_kinds": cap.allowed_kinds,
      "allowed_ids": cap.allowed_ids,
    },
    "metadata": spec.metadata if spec.metadata is not None else None,
  }
  if spec.verification_contract_id:
    out["verification_contract_id"] = spec.verification_contract_id
  if spec.milestones:
    out["milestones"] = {
      "phases": [
        {
          "id": p.id,
          "criteria": p.criteria,
          "unlocks": p.unlocks,
          "required_evidence": p.required_evidence,
          **({"verifier": p.verifier} if p.verifier else {}),
        }
        for p in spec.milestones.phases
      ],
    }
  return out


def milestone_check_result_to_kernel(result: MilestoneCheckResult) -> dict[str, Any]:
  out: dict[str, Any] = {"phase_id": result.phase_id, "passed": result.passed}
  if result.reason:
    out["reason"] = result.reason
  return out


def milestone_check_pass(phase_id: str) -> MilestoneCheckResult:
  return MilestoneCheckResult(phase_id=phase_id, passed=True)


def milestone_check_fail(phase_id: str, reason: str) -> MilestoneCheckResult:
  return MilestoneCheckResult(phase_id=phase_id, passed=False, reason=reason)


def sub_agent_result_to_kernel(result: SubAgentResult) -> dict[str, Any]:
  final = result.result.final_message
  final_kernel = None
  if final is not None:
    tool_calls = getattr(final, "tool_calls", None) or []
    final_kernel = {
      "role": getattr(final, "role", "assistant"),
      "content": getattr(final, "content", ""),
      "tool_calls": [
        {"id": c.id, "name": c.name, "arguments": json.loads(c.arguments or "{}")}
        for c in tool_calls
      ],
    }
    token_count = getattr(final, "token_count", None)
    if token_count is not None:
      final_kernel["token_count"] = token_count
  return {
    "agent_id": result.agent_id,
    "result": {
      "termination": result.result.termination,
      "final_message": final_kernel,
      "turns_used": result.result.turns_used,
      "total_tokens_used": result.result.total_tokens_used,
    },
  }
