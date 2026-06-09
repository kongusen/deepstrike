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


# ─── W0-ABI: declarative workflow specs ───


@dataclass
class WorkflowNodeSpec:
  """One node in a declarative workflow DAG (host shape)."""

  task: str | dict[str, Any]  # goal string, or {"goal", "criteria"?, "lane"?}
  role: KernelAgentRole
  isolation: AgentIsolation = "shared"
  context_inheritance: ContextInheritance = "none"
  model_hint: str | None = None
  depends_on: list[int] = field(default_factory=list)


@dataclass
class WorkflowSpec:
  """A declarative workflow DAG the kernel runs node-by-node, gating each spawn."""

  nodes: list[WorkflowNodeSpec]


@dataclass
class WorkflowSpawnInfo:
  """Per-node spawn descriptor carried in the ``workflow_batch_spawned`` observation."""

  agent_id: str
  goal: str
  role: str
  isolation: str
  context_inheritance: str
  model_hint: str | None = None


def workflow_spec_to_kernel(spec: WorkflowSpec) -> dict[str, Any]:
  """Map a host ``WorkflowSpec`` to the snake_case kernel JSON (``load_workflow.spec``)."""
  nodes: list[dict[str, Any]] = []
  for n in spec.nodes:
    task = {"goal": n.task} if isinstance(n.task, str) else dict(n.task)
    kernel_task: dict[str, Any] = {
      "goal": task["goal"],
      # `criteria` is required by the kernel's RuntimeTask serde (no default).
      "criteria": task.get("criteria", []),
    }
    if task.get("lane"):
      kernel_task["lane"] = task["lane"]
    node: dict[str, Any] = {
      "task": kernel_task,
      "role": n.role,
      "isolation": n.isolation,
      "context_inheritance": n.context_inheritance,
    }
    if n.model_hint:
      node["model_hint"] = n.model_hint
    if n.depends_on:
      node["depends_on"] = list(n.depends_on)
    nodes.append(node)
  return {"nodes": nodes}


def workflow_node_to_spec(node: WorkflowSpawnInfo, parent_session_id: str) -> AgentRunSpec:
  """Build a sub-agent run spec for a kernel-generated workflow node."""
  return AgentRunSpec(
    identity=AgentIdentity(
      agent_id=node.agent_id,
      session_id=f"{parent_session_id}-{node.agent_id}",
      is_sub_agent=True,
      parent_session_id=parent_session_id,
    ),
    role=node.role,  # type: ignore[arg-type]
    goal=node.goal,
    isolation=node.isolation,  # type: ignore[arg-type]
  )


def workflow_node_to_manifest(
  node: WorkflowSpawnInfo, parent_session_id: str
) -> AgentProcessChangedObservation:
  """Build the host manifest for a kernel-generated workflow node."""
  return AgentProcessChangedObservation(
    agent_id=node.agent_id,
    parent_session_id=parent_session_id,
    role=node.role,
    isolation=node.isolation,
    context_inheritance=node.context_inheritance,
    permitted_capability_ids=[],
  )
