from __future__ import annotations

import warnings
from dataclasses import dataclass, replace
from typing import TYPE_CHECKING, Any

from deepstrike._kernel import KernelRuntime, LoopPolicy
from deepstrike.providers.stream import DoneEvent, TextDelta, WorkflowNodesSubmittedEvent
from deepstrike.runtime.filtered_plane import FilteredExecutionPlane
from deepstrike.runtime.kernel_step import kernel_apply
from deepstrike.runtime.runner import RuntimeOptions, RuntimeRunner, SubAgentHarnessConfig
from deepstrike.runtime.session_log import SessionLog
from deepstrike.types.agent import (
  AgentRunSpec,
  AgentProcessChangedObservation,
  LoopResult,
  SubAgentResult,
  agent_run_spec_to_kernel as spec_to_kernel,
)

@dataclass
class SubAgentRunContext:
  parent_opts: RuntimeOptions
  parent_session_id: str
  spec: AgentRunSpec
  manifest: AgentProcessChangedObservation
  session_log: SessionLog
  harness: SubAgentHarnessConfig | None = None
  # M5 v2.1: set when this child is a workflow node — propagated so a nested ``start_workflow``
  # FLATTENS to the parent kernel rather than auto-pivoting into its own bootstrap.
  is_workflow_node: bool = False
  # W-N1 tool exposure. The kernel omits an EMPTY ``permitted_capability_ids`` on the wire, so a
  # grant-less workflow node is indistinguishable from a zero-cap spawn at the manifest — the
  # caller states intent instead: "inherit" = run on the parent's execution plane with its
  # meta-tool availability (trusted workflow nodes — they carried no grant list by design, and
  # filtering on the missing list ran every DAG node TOOL-LESS); "filtered" (default) = filter
  # to the manifest grants, empty ⇒ deny-all (spawn path, quarantined nodes).
  tool_access: str = "filtered"  # "inherit" | "filtered"
  # AttemptLoop carry material; delivered through the child's signal input.
  context_input: str | None = None


def _termination_from_status(status: str) -> str:
  normalized = status.lower()
  known = {
    "completed", "max_turns", "token_budget", "timeout",
    "user_abort", "error", "milestone_exceeded", "context_overflow", "no_progress",
  }
  return normalized if normalized in known else status


def _manifest_from_obs(obs: dict, parent_session_id: str, spec: AgentRunSpec) -> AgentProcessChangedObservation:
  return AgentProcessChangedObservation(
    agent_id=str(obs.get("agent_id") or spec.identity.agent_id),
    parent_session_id=str(obs.get("parent_session_id") or parent_session_id),
    role=str(obs.get("role") or spec.role),
    isolation=str(obs.get("isolation") or spec.isolation),
    context_inheritance=str(obs.get("context_inheritance") or "none"),
    permitted_capability_ids=list(obs.get("permitted_capability_ids") or []),
    turn=obs.get("turn"),
    state=str(obs.get("state") or "running"),
    result_termination=obs.get("result_termination"),
  )


def _find_spawn_obs(observations: list[dict]) -> dict | None:
  for o in observations:
    kind = o.get("kind")
    if kind in ("agent_process_changed", "agent_spawned") and o.get("agent_id"):
      return o
  return None


async def _log_agent_process_changed(session_log: SessionLog, parent_session_id: str, obs: dict) -> None:
  turn = obs.get("turn") or 0
  entry: dict = {
    "kind": "agent_process_changed",
    "turn": turn,
    "agent_id": obs.get("agent_id") or "",
    "parent_session_id": obs.get("parent_session_id") or parent_session_id,
    "role": obs.get("role") or "",
    "isolation": obs.get("isolation") or "",
    "context_inheritance": obs.get("context_inheritance") or "",
    "state": obs.get("state") or "running",
    "permitted_capability_ids": obs.get("permitted_capability_ids") or [],
  }
  if obs.get("result_termination"):
    entry["result_termination"] = obs["result_termination"]
  await session_log.append(parent_session_id, entry)


def _harness_criteria(spec: AgentRunSpec) -> list:
  from deepstrike.harness.harness import Criterion

  phases = spec.milestones.phases if spec.milestones else []
  return [
    Criterion(text=text, required=True)
    for phase in phases
    for text in phase.criteria
    if isinstance(text, str)
  ]


def _derive_meta_tools(permitted: set[str], opts: RuntimeOptions) -> frozenset[str]:
  """Derive which meta-tools a child runner should expose based on permitted IDs and available sources."""
  meta: set[str] = set()
  if "skill" in permitted and opts.skill_dir:
    meta.add("skill")
  if "memory" in permitted and opts.dream_store:
    meta.add("memory")
  if "knowledge" in permitted and opts.knowledge_source:
    meta.add("knowledge")
  if "update_plan" in permitted and opts.enable_plan_tool:
    meta.add("update_plan")
  return frozenset(meta)


def _available_meta_tools(opts: RuntimeOptions) -> frozenset[str]:
  """W-N1: meta-tools by source availability alone — a ``tool_access="inherit"`` child gets the
  same meta surface a top-level run of these options would."""
  meta: set[str] = set()
  if opts.skill_dir:
    meta.add("skill")
  if opts.dream_store:
    meta.add("memory")
  if opts.knowledge_source:
    meta.add("knowledge")
  if opts.enable_plan_tool:
    meta.add("update_plan")
  return frozenset(meta)


def _resolve_tool_grants(ctx: SubAgentRunContext) -> tuple[Any, frozenset[str]]:
  """W-N1 (+W-N4: the ONE grant-resolution seam for the direct and harness paths): "inherit" runs
  the child on the parent's plane with availability-derived meta-tools (trusted workflow nodes);
  "filtered" (default) filters to the manifest grants, empty ⇒ deny-all (spawn path, quarantined
  nodes)."""
  from deepstrike.runtime.execution_plane import LocalExecutionPlane

  plane = ctx.parent_opts.execution_plane or LocalExecutionPlane()
  if ctx.tool_access == "inherit":
    return plane, _available_meta_tools(ctx.parent_opts)
  permitted = set(ctx.manifest.permitted_capability_ids)
  meta_tools = _derive_meta_tools(permitted, ctx.parent_opts)
  # A "filtered" spawn with no capability grants and no meta-tools resolves to a deny-all plane —
  # the child model sees zero tools and reports "no tools available". Warn the host (visible, not
  # fatal) with the fix, mirroring execution_plane's failure-shaped-chunk warning. Exempt workflow
  # nodes: not-inherit + workflow-node ⇒ quarantined ⇒ intentional deny-all, not a misconfiguration.
  if not ctx.is_workflow_node and not permitted and not meta_tools:
    warnings.warn(
      f'spawned sub-agent "{ctx.spec.identity.agent_id}" resolved to zero tools (deny-all filter). '
      "Mount tools as capabilities and grant via spec.capability_filter, or pass "
      "spec.tool_access='inherit' to run on the parent's plane. If a tool-less child is intentional, "
      "ignore this.",
      RuntimeWarning,
      stacklevel=2,
    )
  return FilteredExecutionPlane(plane, permitted, meta_tools), meta_tools


def _resolve_provider(opts: RuntimeOptions, model_hint: str | None):
  """M1/G3 intelligence routing: resolve the provider for a sub-agent from its spec's ``model_hint``.

  Falls back to the parent provider when there is no hint or no ``provider_for`` hook resolves it."""
  if model_hint and opts.provider_for is not None:
    routed = opts.provider_for(model_hint)
    if routed is not None:
      return routed
  return opts.provider


def _wrap_worktree(ctx: SubAgentRunContext, plane):
  """M3/G4: if this is an ``isolation: "worktree"`` node and a worktree manager is configured, wrap
  its plane in a ``WorktreeExecutionPlane`` (creates a git worktree, injects it as ``cwd``, removes
  it on cleanup). Returns ``(plane, cleanup)``; a non-worktree node gets a pass-through + no-op."""
  if getattr(ctx.manifest, "isolation", None) == "worktree" and ctx.parent_opts.worktree_manager is not None:
    from deepstrike.runtime.worktree_plane import WorktreeExecutionPlane

    wt = WorktreeExecutionPlane(plane, ctx.parent_opts.worktree_manager, ctx.spec.identity.agent_id)
    return wt, wt.cleanup

  async def _noop() -> None:
    return None

  return plane, _noop


def _build_child_opts(
  ctx: SubAgentRunContext,
  *,
  system_prompt: str | None,
  filtered_plane,
  meta_tools: frozenset[str],
) -> RuntimeOptions:
  # Inherit-everything like node's `{...ctx.parentOpts, ...overrides}` spread, so the child stays in
  # the parent's governance domain (`run_group`) and sees `reducers` / `worktree_manager` / every
  # future option without this list drifting. Only the per-child contract fields are overridden.
  return replace(
    ctx.parent_opts,
    # M1/G3: route to the node's hinted model (falls back to the parent provider).
    provider=_resolve_provider(ctx.parent_opts, getattr(ctx.spec, "model_hint", None)),
    # M4/G5: cap the child run at the node's token budget (falls back to the inherited cap).
    max_total_tokens=getattr(ctx.spec, "token_budget", None) or ctx.parent_opts.max_total_tokens,
    session_log=ctx.session_log,
    execution_plane=filtered_plane,
    # O3: per-child turn / wall-clock caps (fall back to the inherited limits).
    max_turns=getattr(ctx.spec, "max_turns", None) or ctx.parent_opts.max_turns,
    timeout_ms=getattr(ctx.spec, "max_wall_ms", None) or ctx.parent_opts.timeout_ms,
    agent_id=ctx.spec.identity.agent_id,
    system_prompt=system_prompt,
    skill_dir=ctx.parent_opts.skill_dir if "skill" in meta_tools else None,
    dream_store=ctx.parent_opts.dream_store if "memory" in meta_tools else None,
    knowledge_source=ctx.parent_opts.knowledge_source if "knowledge" in meta_tools else None,
    enable_plan_tool=ctx.parent_opts.enable_plan_tool if "update_plan" in meta_tools else None,
    # M5 v2.1: a workflow node's `start_workflow` flattens to the parent kernel (no nested pivot).
    is_workflow_node=ctx.is_workflow_node,
    # Nested vehicle: the child joins the inherited run_group for lineage/settlement only — it
    # must NOT re-reserve budget axes the parent already holds (that double-reserve squeezed the
    # child's grant to 0 and the kernel stripped its first-turn tools).
    nested_group_vehicle=True,
    # The child runs under ITS OWN spec, never the parent's: the replace() above would otherwise
    # leak the parent's ``run_spec`` (identity, capability filter — and a LoopDriver's armed
    # ``loop_round``, giving every child a phantom pace tool). A loop-node iteration carries its
    # own minimal spec to arm the pacing trap (DW-3); everything else runs spec-less as before.
    run_spec=(
      AgentRunSpec(
        identity=ctx.spec.identity,
        role=ctx.spec.role,
        goal=ctx.spec.goal,
        loop_round=getattr(ctx.spec, "loop_round", None),
      )
      if getattr(ctx.spec, "loop_round", None)
      else None
    ),
  )


async def _resolve_inheritance(ctx: SubAgentRunContext) -> tuple[str | None, list | None]:
  system_prompt = ctx.parent_opts.system_prompt
  inherit_events = None

  if ctx.manifest.context_inheritance == "full":
    inherit_events = await ctx.session_log.read(ctx.parent_session_id)
  elif ctx.manifest.context_inheritance == "system_only":
    parent_events = await ctx.session_log.read(ctx.parent_session_id)
    for entry in parent_events:
      ev = entry.event
      if ev.get("kind") == "run_started" and ev.get("system_prompt"):
        system_prompt = ev["system_prompt"]
        break

  return system_prompt, inherit_events


class SubAgentOrchestrator:
  async def run(self, ctx: SubAgentRunContext) -> SubAgentResult:
    if ctx.harness:
      return await self._run_with_harness(ctx)
    return await self._run_direct(ctx)

  async def _run_with_harness(self, ctx: SubAgentRunContext) -> SubAgentResult:
    from deepstrike.harness.harness import (
      AttemptLoop,
      AttemptRequest,
      RuntimeAttemptBody,
      StopPolicy,
    )
    from deepstrike.harness.judge import LlmEvalJudge

    # W-N1: same grant resolution as the direct path (inherit vs filtered).
    base_plane, meta_tools = _resolve_tool_grants(ctx)
    # M3/G4: worktree isolation for a worktree node (cleaned up in `finally` below).
    exec_plane, cleanup_worktree = _wrap_worktree(ctx, base_plane)
    system_prompt, inherit_events = await _resolve_inheritance(ctx)
    child_runner = RuntimeRunner(_build_child_opts(
      ctx,
      system_prompt=system_prompt,
      filtered_plane=exec_plane,
      meta_tools=meta_tools,
    ))
    if ctx.context_input:
      child_runner.inject_note(ctx.context_input)
    loop = AttemptLoop(
      body=RuntimeAttemptBody(child_runner),
      judge=LlmEvalJudge(ctx.harness.eval_provider),
      stop=StopPolicy(max_attempts=ctx.harness.max_attempts),
    )
    try:
      outcome = await loop.run(AttemptRequest(
        session_id=ctx.spec.identity.session_id,
        goal=ctx.spec.goal,
        criteria=_harness_criteria(ctx.spec),
        inherit_events=inherit_events,
      ))
    finally:
      await cleanup_worktree()

    from deepstrike._kernel import Message

    final_message = None
    if outcome.result:
      final_message = Message(role="assistant", content=outcome.result)

    run_termination = _termination_from_status(outcome.run_status)
    termination = (
      "error"
      if outcome.outcome == "exhausted"
      or (
        outcome.outcome == "run_error"
        and run_termination not in {"error", "user_abort"}
      )
      else run_termination
    )
    loop_result = LoopResult(
      termination=termination,
      turns_used=outcome.turns,
      total_tokens_used=outcome.total_tokens,
      final_message=final_message,
      attempt={
        "outcome": outcome.outcome,
        "run_status": outcome.run_status,
        "attempts": outcome.attempts,
        "verdict": outcome.verdict,
      },
    )
    # R3-1: surface nodes the agent submitted under the harness so run_workflow appends them.
    return SubAgentResult(
      agent_id=ctx.spec.identity.agent_id,
      result=loop_result,
      submitted_nodes=list(outcome.submitted_nodes),
    )

  async def _run_direct(self, ctx: SubAgentRunContext) -> SubAgentResult:
    # W-N1: "inherit" runs the child on the parent's plane (trusted workflow nodes); "filtered"
    # (default) filters to the manifest grants, empty ⇒ deny-all.
    base_plane, meta_tools = _resolve_tool_grants(ctx)
    system_prompt, inherit_events = await _resolve_inheritance(ctx)

    # M3/G4: a worktree node runs inside its own git worktree (created here, removed in `finally`).
    exec_plane, cleanup_worktree = _wrap_worktree(ctx, base_plane)
    child_runner = RuntimeRunner(_build_child_opts(
      ctx,
      system_prompt=system_prompt,
      filtered_plane=exec_plane,
      meta_tools=meta_tools,
    ))
    if ctx.context_input:
      child_runner.inject_note(ctx.context_input)

    done: DoneEvent | None = None
    final_text = ""
    # R3-1: collect any nodes this node's agent submitted via the `submit_workflow_nodes` tool (the
    # runner surfaces them as `WorkflowNodesSubmittedEvent` because the workflow lives in the parent
    # kernel, not this child's). `run_workflow` sends them to the parent kernel.
    submitted_nodes: list = []
    try:
      async for evt in child_runner.run(
        session_id=ctx.spec.identity.session_id,
        goal=ctx.spec.goal,
        inherit_events=inherit_events,
      ):
        if isinstance(evt, TextDelta):
          final_text += evt.delta
        if isinstance(evt, DoneEvent):
          done = evt
        if isinstance(evt, WorkflowNodesSubmittedEvent):
          submitted_nodes.extend(evt.nodes)
    finally:
      await cleanup_worktree()

    from deepstrike._kernel import Message

    final_message = Message(role="assistant", content=final_text) if final_text else None
    loop_result = LoopResult(
      termination=_termination_from_status(done.status if done else "error"),
      turns_used=done.iterations if done else 0,
      total_tokens_used=done.total_tokens if done else 0,
      final_message=final_message,
      # DW-3: surface the kernel-adjudicated pace decision (loop-node iterations consume it as the
      # continuation vocabulary). SDK-internal; stripped by ``sub_agent_result_to_kernel``.
      pace_decision=getattr(done, "pace_decision", None) if done else None,
    )
    return SubAgentResult(
      agent_id=ctx.spec.identity.agent_id,
      result=loop_result,
      submitted_nodes=submitted_nodes,
    )


default_sub_agent_orchestrator = SubAgentOrchestrator()


async def spawn_standalone(
  parent_opts: RuntimeOptions,
  parent_session_id: str,
  spec: AgentRunSpec,
  *,
  orchestrator: SubAgentOrchestrator | None = None,
  context_input: str | None = None,
) -> SubAgentResult:
  """Kernel spawn path without an active parent run loop (harness / coordinator use)."""
  policy = LoopPolicy(max_tokens=parent_opts.max_tokens, max_turns=parent_opts.max_turns)
  runtime = KernelRuntime(policy)
  pending: list[dict] = []

  kernel_apply(runtime, pending, {"kind": "start_run", "task": {"goal": "coordinator", "criteria": []}})
  observations = kernel_apply(runtime, pending, {
    "kind": "spawn_sub_agent",
    "spec": spec_to_kernel(spec),
    "parent_session_id": parent_session_id,
  })

  spawned_obs = _find_spawn_obs(observations)
  if spawned_obs is None:
    raise RuntimeError("spawn_sub_agent did not emit agent_process_changed")

  await _log_agent_process_changed(parent_opts.session_log, parent_session_id, spawned_obs)
  manifest = _manifest_from_obs(spawned_obs, parent_session_id, spec)

  orch = orchestrator or default_sub_agent_orchestrator
  return await orch.run(SubAgentRunContext(
    parent_opts=parent_opts,
    parent_session_id=parent_session_id,
    spec=spec,
    manifest=manifest,
    session_log=parent_opts.session_log,
    harness=parent_opts.sub_agent_harness,
    context_input=context_input,
  ))
