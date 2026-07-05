from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Literal

KernelAgentRole = Literal["explore", "plan", "implement", "verify", "custom"]
AgentIsolation = Literal["shared", "read_only", "worktree", "remote"]
ContextInheritance = Literal["none", "system_only", "full"]
NodeTrust = Literal["trusted", "quarantined"]
TerminationReason = Literal[
  "completed", "max_turns", "token_budget", "timeout", "user_abort", "error", "milestone_exceeded",
  # v0.2.35 recovery ladder: compaction exhausted, prompt still exceeds the provider window.
  "context_overflow",
  # Repeat-fuse escalation: same tool call (name AND args) re-issued past terminate_after — a stall.
  "no_progress",
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
  # M1/G3: per-agent model preference; the host resolves it via ``RuntimeOptions.provider_for``.
  # Host-side routing only — not sent to the kernel.
  model_hint: str | None = None
  # M4/G5: cumulative token cap for this sub-agent's run (sets the child kernel's max_total_tokens).
  token_budget: int | None = None
  # O3: per-child turn cap (sets the child runner's max_turns; falls back to the parent's). A child
  # that exhausts it terminates "max_turns" — the parent reads the termination and decides retry/skip.
  max_turns: int | None = None
  # O3: per-child wall-clock cap in ms (sets the child runner's timeout_ms; falls back to the
  # parent's). A hung child terminates "timeout" instead of stalling the parent indefinitely.
  max_wall_ms: int | None = None
  # ③ loop-agent pacing: arms the kernel's after-round pacing trap (`pace` meta-tool). Snake_case
  # dict: {"max_rounds"?, "min_sleep_ms"?, "max_sleep_ms"?, "default_action"?}. Only set keys are
  # lowered to the kernel; None ⇒ no trap (a plain run).
  loop_round: dict[str, Any] | None = None


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
  # A#2 v2 loop stop signal: a loop iteration sets False to end the loop before `max_iters`. None
  # (every non-loop result) ⇒ no opinion → run to the cap. Sent only when set.
  loop_continue: bool | None = None
  # A#2 classify routing: a classifier node reports the chosen branch label; the kernel runs that
  # branch and prunes the rest. Sent only when set.
  classify_branch: str | None = None
  # A#2 tournament verdict: a judge reports the winning entrant's agent id. Sent only when set.
  tournament_winner: str | None = None
  # ③ loop-agent pacing: the kernel-adjudicated after-round decision, surfaced by the orchestrator
  # from the child's done event ({"action", "delay_ms"?, "reason", "coerced_from"?}). For a loop-node
  # iteration this is the PRIMARY continuation vocabulary (stop → loop_continue=False); the legacy
  # text-sniffed signal is the fallback. SDK-internal — stripped by ``sub_agent_result_to_kernel``.
  pace_decision: Any | None = None


@dataclass
class SubAgentResult:
  agent_id: str
  result: LoopResult
  # R3-1: nodes this node's agent asked to append to the parent workflow DAG (via the
  # `submit_workflow_nodes` tool). Surfaced by the orchestrator; `run_workflow` sends them to the
  # parent kernel before this node's completion. SDK-internal — not sent on the kernel SubAgentResult.
  submitted_nodes: list[WorkflowNodeSpec] = field(default_factory=list)


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
  # ③ loop-agent pacing: lower only the set knobs (kernel defaults fill the rest).
  if getattr(spec, "loop_round", None):
    lr = spec.loop_round or {}
    out["loop_round"] = {
      k: lr[k]
      for k in ("max_rounds", "min_sleep_ms", "max_sleep_ms", "default_action")
      if lr.get(k) is not None
    }
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


def _safe_parse_tool_args(raw: str | None) -> dict[str, Any]:
  """Tool-call ``arguments`` reach us as a raw model-authored string (e.g. the OpenAIChat-family
  non-streaming path passes it through verbatim). A malformed JSON string must degrade to empty
  args here, never raise — otherwise one bad tool-call on a sub-agent's final turn bricks the
  parent's result serialization. Mirrors the ``except -> {}`` guard every provider parse site uses."""
  try:
    return json.loads(raw or "{}")
  except (ValueError, TypeError):
    return {}


def sub_agent_result_to_kernel(result: SubAgentResult) -> dict[str, Any]:
  final = result.result.final_message
  final_kernel = None
  if final is not None:
    tool_calls = getattr(final, "tool_calls", None) or []
    final_kernel = {
      "role": getattr(final, "role", "assistant"),
      "content": getattr(final, "content", ""),
      "tool_calls": [
        {"id": c.id, "name": c.name, "arguments": _safe_parse_tool_args(c.arguments)}
        for c in tool_calls
      ],
    }
    token_count = getattr(final, "token_count", None)
    if token_count is not None:
      final_kernel["token_count"] = token_count
  res: dict[str, Any] = {
    "termination": result.result.termination,
    "final_message": final_kernel,
    "turns_used": result.result.turns_used,
    "total_tokens_used": result.result.total_tokens_used,
  }
  # A#2: control-flow signals — additive, omitted on the wire when unset so a plain spawn's result is
  # byte-identical to before. The kernel reads each only for the matching node kind.
  if getattr(result.result, "loop_continue", None) is not None:
    res["loop_continue"] = result.result.loop_continue
  if getattr(result.result, "classify_branch", None) is not None:
    res["classify_branch"] = result.result.classify_branch
  if getattr(result.result, "tournament_winner", None) is not None:
    res["tournament_winner"] = result.result.tournament_winner
  return {"agent_id": result.agent_id, "result": res}


# ─── W0-ABI: declarative workflow specs ───


@dataclass
class WorkflowNodeSpec:
  """One node in a declarative workflow DAG (host shape)."""

  task: str | dict[str, Any]  # goal string, or {"goal", "criteria"?, "lane"?}
  role: KernelAgentRole
  isolation: AgentIsolation = "shared"
  context_inheritance: ContextInheritance = "none"
  model_hint: str | None = None
  # W3: `quarantined` nodes read untrusted content and must run without privileges (read-only).
  trust: NodeTrust = "trusted"
  # G3: JSON Schema the node's output must conform to (validated + retried SDK-side).
  output_schema: dict[str, Any] | None = None
  # G2: make this a deterministic reduce node — runs no LLM agent; the runner routes it to the
  # registered reducer of this name over its ``depends_on`` nodes' outputs.
  reducer: str | None = None
  # A#2 v2: make this a *loop* node — re-run its agent up to ``loop["max_iters"]`` times. An iteration
  # may end the loop early by reporting ``loop_continue=False`` (the runner solicits this).
  loop: dict[str, Any] | None = None
  # A#2: make this a *classify* node — ``classify={"branches": [{"label", "nodes": [idx]}]}``. Its
  # agent picks exactly one branch label; that branch's nodes run and the others are pruned.
  classify: dict[str, Any] | None = None
  # A#2: make this a *tournament controller* — ``tournament={"entrants": [task, ...]}``. Generate each
  # entrant in parallel, then pairwise-judge to one winner (this node's goal is the criterion). ≥2.
  tournament: dict[str, Any] | None = None
  # M4/G5: cap this node's child run at ``token_budget`` cumulative tokens (the per-node "use N tokens").
  token_budget: int | None = None
  # O3: cap this node's child run at ``max_turns`` provider turns (falls back to the parent's).
  max_turns: int | None = None
  # O3: cap this node's child run at ``max_wall_ms`` wall-clock milliseconds.
  max_wall_ms: int | None = None
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
  # W3 trust level: "trusted" | "quarantined" (W-N1: quarantined nodes stay deny-all filtered).
  trust: str | None = None
  # G3: JSON Schema the node's output must conform to (carried verbatim from the spec).
  output_schema: dict[str, Any] | None = None
  # G2: for a reduce node, the registered reducer name.
  reducer: str | None = None
  # The dependency agent ids for EVERY dependent node (W-N2: a DAG edge carries data). A reduce
  # node's registered function consumes them; every other node gets its deps' outputs in context.
  input_agent_ids: list[str] = field(default_factory=list)
  # A#2: present only for a tournament *judge* spawn — the two entrant agent ids whose outputs this
  # judge compares (``{"left", "right"}``). The runner reports the winner as ``tournament_winner``.
  judge_match: dict[str, str] | None = None
  # A#2 v2: present only for a *loop* iteration spawn — the loop's ``max_iters``. Marks the spawn as a
  # loop iteration so the runner solicits + reports a ``loop_continue`` stop signal.
  loop_max_iters: int | None = None
  # A#2: present only for a *classify* spawn — the branch labels the classifier must choose among.
  classify_labels: list[str] = field(default_factory=list)
  # M4/G5: the node's per-node cumulative token cap, if set — the runner caps the child run here.
  token_budget: int | None = None
  # O3: per-node turn cap → the child run's ``max_turns``.
  max_turns: int | None = None
  # O3: per-node wall-clock cap (ms) → the child run's timeout.
  max_wall_ms: int | None = None


def workflow_budget_note(budget: dict[str, Any] | None) -> str:
  """G4: a concise budget note appended to a coordinator node's goal so its agent can size a
  ``submit_workflow_nodes`` batch to what is available. ``budget`` is the snake_case dict carried on
  the ``workflow_batch_spawned`` observation. Returns "" when nothing is bounded (no quota)."""
  if not budget:
    return ""
  parts: list[str] = []
  if budget.get("nodes_remaining") is not None and budget.get("nodes_max") is not None:
    parts.append(
      f"nodes {budget.get('nodes_used')}/{budget['nodes_max']} used, {budget['nodes_remaining']} remaining"
    )
  if budget.get("concurrency_remaining") is not None and budget.get("max_concurrent_subagents") is not None:
    parts.append(
      f"concurrency {budget.get('running_subagents')}/{budget['max_concurrent_subagents']} running, "
      f"{budget['concurrency_remaining']} free"
    )
  # M4/G5 token headroom: lets a coordinator scale a submission to "use N tokens".
  if budget.get("tokens_remaining") is not None and budget.get("tokens_max") is not None:
    parts.append(
      f"tokens {budget.get('tokens_used', 0)}/{budget['tokens_max']} used, {budget['tokens_remaining']} remaining"
    )
  if not parts:
    return ""
  return (
    "[workflow budget] " + " · ".join(parts) + ". "
    "If you submit more workflow nodes, keep the batch within the remaining node and token budget."
  )


def _workflow_task_to_kernel(t: str | dict[str, Any]) -> dict[str, Any]:
  """Normalize a workflow task (goal string or dict) to the kernel's RuntimeTask JSON."""
  task = {"goal": t} if isinstance(t, str) else dict(t)
  kernel_task: dict[str, Any] = {
    "goal": task["goal"],
    # `criteria` is required by the kernel's RuntimeTask serde (no default).
    "criteria": task.get("criteria", []),
  }
  if task.get("lane"):
    kernel_task["lane"] = task["lane"]
  return kernel_task


def _node_kind_to_kernel(n: WorkflowNodeSpec) -> dict[str, Any] | None:
  """Lower a node's control-flow kind to the kernel's serde-tagged NodeKind JSON, or None for a plain
  spawn. ``reducer`` / ``loop`` / ``classify`` / ``tournament`` are mutually exclusive."""
  declared = sum(
    1 for v in (n.reducer, n.loop, n.classify, n.tournament) if v is not None
  )
  if declared > 1:
    raise ValueError("a workflow node may declare at most one of: reducer, loop, classify, tournament")
  if n.reducer is not None:
    return {"type": "reduce", "reducer": n.reducer}
  if n.loop is not None:
    return {"type": "loop", "max_iters": n.loop["max_iters"]}
  if n.classify is not None:
    return {
      "type": "classify",
      "branches": [{"label": b["label"], "nodes": list(b["nodes"])} for b in n.classify["branches"]],
    }
  if n.tournament is not None:
    return {"type": "tournament", "entrants": [_workflow_task_to_kernel(e) for e in n.tournament["entrants"]]}
  return None


def workflow_node_spec_to_kernel(n: WorkflowNodeSpec) -> dict[str, Any]:
  """Map one host ``WorkflowNodeSpec`` to its snake_case kernel JSON. Shared by ``load_workflow`` and
  ``submit_workflow_nodes`` (R3-1) so the two encodings never drift."""
  node: dict[str, Any] = {
    "task": _workflow_task_to_kernel(n.task),
    "role": n.role,
    "isolation": n.isolation,
    "context_inheritance": n.context_inheritance,
  }
  if n.model_hint:
    node["model_hint"] = n.model_hint
  if getattr(n, "trust", "trusted") and n.trust != "trusted":
    node["trust"] = n.trust
  if getattr(n, "output_schema", None):
    node["output_schema"] = n.output_schema
  # A#2/G2: loop / classify / tournament / reduce lower to a serde-tagged NodeKind; spawn omits it.
  kind = _node_kind_to_kernel(n)
  if kind is not None:
    node["kind"] = kind
  # M4/G5: per-node token cap (additive; omitted when unset).
  if getattr(n, "token_budget", None) is not None:
    node["token_budget"] = n.token_budget
  # O3: per-node turn / wall-clock caps (additive; omitted when unset).
  if getattr(n, "max_turns", None) is not None:
    node["max_turns"] = n.max_turns
  if getattr(n, "max_wall_ms", None) is not None:
    node["max_wall_ms"] = n.max_wall_ms
  if n.depends_on:
    node["depends_on"] = list(n.depends_on)
  return node


def workflow_spec_to_kernel(spec: WorkflowSpec) -> dict[str, Any]:
  """Map a host ``WorkflowSpec`` to the snake_case kernel JSON (``load_workflow.spec``)."""
  return {"nodes": [workflow_node_spec_to_kernel(n) for n in spec.nodes]}


def submit_workflow_nodes_to_kernel(
  nodes: list[WorkflowNodeSpec], submitter_agent_id: str | None = None
) -> dict[str, Any]:
  """R3-1: map a batch of host nodes to the ``submit_workflow_nodes`` kernel event body.

  G1: ``submitter_agent_id`` (the node that requested the append) lets the kernel enforce
  no-privilege-escalation — a quarantined submitter's nodes are coerced to quarantined. Omitted ⇒
  no coercion.
  """
  body: dict[str, Any] = {
    "kind": "submit_workflow_nodes",
    "nodes": [workflow_node_spec_to_kernel(n) for n in nodes],
  }
  if submitter_agent_id:
    body["submitter_agent_id"] = submitter_agent_id
  return body


def submit_workflow_to_kernel(
  spec: WorkflowSpec, parent_session_id: str, submitter_agent_id: str | None = None
) -> dict[str, Any]:
  """M5/G1: map an agent-authored spec to the ``submit_workflow`` kernel event body.

  The agent-reachable ``Syscall::LoadWorkflow``: the kernel bootstraps the DAG when none is active,
  else flattens the spec's nodes onto the running one. ``parent_session_id`` seeds child session ids
  on bootstrap; ``submitter_agent_id`` carries G1 trust coercion on the flatten case.
  """
  body: dict[str, Any] = {
    "kind": "submit_workflow",
    "spec": workflow_spec_to_kernel(spec),
    "parent_session_id": parent_session_id,
  }
  if submitter_agent_id:
    body["submitter_agent_id"] = submitter_agent_id
  return body


# R3-1: the tool a workflow-coordinator node's agent calls to append work to the running DAG (true
# loop-until-done / dynamic fan-out). The runner intercepts the call and routes the nodes to the
# parent kernel (the child's own kernel holds no workflow).
# Shared JSON-Schema for a workflow-node batch (a DAG). Used by both ``submit_workflow_nodes``
# (append) and ``start_workflow`` (M5 v1: author a sub-workflow), so the two tools never drift.
_workflow_nodes_array_schema: dict[str, Any] = {
  "type": "array",
  "description": "Workflow nodes (a DAG); each runs as a gated sub-agent.",
  "items": {
    "type": "object",
    "properties": {
      "task": {"description": "The node's goal: a string, or {goal, criteria?, lane?}."},
      "role": {"type": "string", "enum": ["explore", "plan", "implement", "verify", "custom"]},
      "isolation": {"type": "string", "enum": ["shared", "read_only", "worktree", "remote"]},
      "context_inheritance": {"type": "string", "enum": ["none", "system_only", "full"]},
      "trust": {"type": "string", "enum": ["trusted", "quarantined"]},
      "output_schema": {"type": "object", "description": "Optional JSON Schema the node's output must conform to."},
      "model_hint": {"type": "string", "description": "Preferred model for this node (e.g. \"opus\"/\"sonnet\"); the host routes it."},
      "reducer": {"type": "string", "description": "Make this a deterministic reduce node (no LLM); names a registered reducer."},
      "loop": {
        "type": "object",
        "description": "Make this a loop node: re-run up to max_iters times, ending early when the agent reports done.",
        "properties": {"max_iters": {"type": "integer", "description": "Hard iteration cap."}},
        "required": ["max_iters"],
      },
      "classify": {
        "type": "object",
        "description": "Make this a classify node: pick one branch label; that branch's nodes run, the rest are pruned.",
        "properties": {
          "branches": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "label": {"type": "string"},
                "nodes": {"type": "array", "items": {"type": "integer"}, "description": "Batch-relative node indices for this branch."},
              },
              "required": ["label", "nodes"],
            },
          },
        },
        "required": ["branches"],
      },
      "tournament": {
        "type": "object",
        "description": "Make this a tournament controller: generate each entrant, then pairwise-judge to one winner.",
        "properties": {
          "entrants": {
            "type": "array",
            "description": "≥2 candidate tasks to generate and judge.",
            "items": {
              "oneOf": [
                {"type": "string"},
                {"type": "object", "properties": {"goal": {"type": "string"}, "criteria": {"type": "array", "items": {"type": "string"}}}, "required": ["goal"]},
              ],
            },
          },
        },
        "required": ["entrants"],
      },
      "token_budget": {"type": "integer", "description": "Cap this node's child run at this many cumulative tokens."},
      "depends_on": {"type": "array", "items": {"type": "integer"}},
    },
    "required": ["task", "role"],
  },
}

submit_workflow_nodes_tool: dict[str, Any] = {
  "name": "submit_workflow_nodes",
  "description": (
    "Append new nodes to the running workflow DAG (dynamic fan-out / loop-until-done). Each node "
    "spawns as a gated sub-agent. Use when you discover more work that should run as its own node. "
    "A node may declare ONE control-flow kind — `loop` / `classify` / `tournament` / `reducer` — "
    "otherwise it is a plain spawn. Within a submission, `depends_on` and `classify.branches[].nodes` "
    "are batch-relative (index 0 = this batch's first node)."
  ),
  "parameters": json.dumps({
    "type": "object",
    "properties": {"nodes": _workflow_nodes_array_schema},
    "required": ["nodes"],
  }),
}

# M5 v1 (flatten): the tool an agent calls to author a sub-workflow — a cohesive DAG of nodes composed
# onto the running workflow. Lowers to the same append path as ``submit_workflow_nodes`` (a
# ``WorkflowSpec`` is a node batch). v2 adds top-level bootstrap (the ``LoadWorkflow`` syscall).
start_workflow_tool: dict[str, Any] = {
  "name": "start_workflow",
  "description": (
    "Author and run a sub-workflow: a DAG of nodes (fan-out / classify / tournament / loop / reduce) "
    "composed onto the current run. Use to structure a multi-step task as its own harness. The nodes "
    "spawn as gated sub-agents; `depends_on` / `classify.branches[].nodes` are spec-relative."
  ),
  "parameters": json.dumps({
    "type": "object",
    "properties": {
      "spec": {
        "type": "object",
        "description": "The workflow specification.",
        "properties": {"nodes": _workflow_nodes_array_schema},
        "required": ["nodes"],
      },
    },
    "required": ["spec"],
  }),
}


def workflow_node_to_spec(node: WorkflowSpawnInfo, parent_session_id: str) -> AgentRunSpec:
  """Build a sub-agent run spec for a kernel-generated workflow node."""
  import re

  # W-N6 transcript-as-carry: a loop node's iterations share ONE stable session id (the ``-i{k}``
  # suffix names the spawn, not the session), so iteration k replays the transcript of 0..k-1 —
  # "do the next increment" actually sees the previous increments. The agent_id keeps the
  # per-iteration suffix (kernel completion routing).
  is_loop_iteration = getattr(node, "loop_max_iters", None) is not None
  session_node_id = re.sub(r"-i\d+$", "", node.agent_id) if is_loop_iteration else node.agent_id
  return AgentRunSpec(
    identity=AgentIdentity(
      agent_id=node.agent_id,
      session_id=f"{parent_session_id}-{session_node_id}",
      is_sub_agent=True,
      parent_session_id=parent_session_id,
    ),
    role=node.role,  # type: ignore[arg-type]
    goal=node.goal,
    isolation=node.isolation,  # type: ignore[arg-type]
    # M1/G3: carry the node's model preference so the orchestrator can route to a provider.
    model_hint=getattr(node, "model_hint", None),
    # M4/G5: carry the node's token cap so the orchestrator can bound the child run.
    token_budget=getattr(node, "token_budget", None),
    # O3: carry the node's turn / wall-clock caps (the orchestrator already honors these).
    max_turns=getattr(node, "max_turns", None),
    max_wall_ms=getattr(node, "max_wall_ms", None),
    # DW-3 one continuation vocabulary: a loop ITERATION runs with the pacing trap armed, so the
    # agent signals continue/stop through the kernel-adjudicated `pace` meta-tool instead of a
    # text-sniffed JSON blob. One iteration = one round; the DAG (not max_rounds) caps iterations,
    # and default stop means "ended without pacing" = done — the CC silence-is-completion contract.
    loop_round={"default_action": "stop"} if is_loop_iteration else None,
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


# ─── W1/W2 workflow templates (the six patterns as one-liners) ───
# Roles carry the kernel's role_defaults isolation/inheritance so host-built specs match the core
# orchestration::workflow constructors (e.g. verifiers stay bias-resistant).


def _as_task(t: "str | dict[str, Any]") -> "str | dict[str, Any]":
  return t


def fanout_synthesize(workers: list, synthesize) -> WorkflowSpec:
  """N parallel read-only Explore workers feeding a single Plan synthesizer (barrier)."""
  nodes = [
    WorkflowNodeSpec(task=_as_task(w), role="explore", isolation="read_only", context_inheritance="system_only")
    for w in workers
  ]
  nodes.append(WorkflowNodeSpec(
    task=_as_task(synthesize), role="plan", isolation="shared", context_inheritance="full",
    depends_on=list(range(len(workers))),
  ))
  return WorkflowSpec(nodes=nodes)


def generate_and_filter(generators: list, filter) -> WorkflowSpec:  # noqa: A002
  """N parallel Implement generators feeding a single Verify filter/dedupe step (barrier)."""
  nodes = [
    WorkflowNodeSpec(task=_as_task(g), role="implement", isolation="worktree", context_inheritance="full")
    for g in generators
  ]
  nodes.append(WorkflowNodeSpec(
    task=_as_task(filter), role="verify", isolation="read_only", context_inheritance="none",
    depends_on=list(range(len(generators))),
  ))
  return WorkflowSpec(nodes=nodes)


def verify_rules(rules: list, skeptic=None) -> WorkflowSpec:
  """One fresh-context verifier per rule (parallel) + optional skeptic depending on all.

  Verifiers run read-only with no inherited author context (bias-resistant).
  """
  nodes = [
    WorkflowNodeSpec(task=_as_task(r), role="verify", isolation="read_only", context_inheritance="none")
    for r in rules
  ]
  if skeptic is not None:
    nodes.append(WorkflowNodeSpec(
      task=_as_task(skeptic), role="verify", isolation="read_only", context_inheritance="none",
      depends_on=list(range(len(rules))),
    ))
  return WorkflowSpec(nodes=nodes)


def gen_eval(worker, evaluate, max_iters: int = 3, extract_skill_on_pass: bool = True) -> WorkflowSpec:
  """Generate→evaluate quality gate (the EvalPipeline successor, #6): a ``loop`` worker node (re-run
  up to ``max_iters``, stopping early on a ``loop_continue=False`` self-signal) + a bias-resistant
  ``verify`` eval node gated on it, carrying the kernel's verdict ``output_schema``. Mirrors the
  kernel ``gen_eval`` template. For the iterative retry-with-feedback variant, drive it with
  ``HarnessLoop``."""
  from deepstrike._kernel import verdict_output_schema
  schema = json.loads(verdict_output_schema(extract_skill_on_pass))
  return WorkflowSpec(nodes=[
    WorkflowNodeSpec(
      task=_as_task(worker), role="implement", isolation="worktree", context_inheritance="full",
      loop={"max_iters": max(1, max_iters)},
    ),
    WorkflowNodeSpec(
      task=_as_task(evaluate), role="verify", isolation="read_only", context_inheritance="none",
      depends_on=[0], output_schema=schema,
    ),
  ])
