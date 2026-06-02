from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from deepstrike._kernel import KernelRuntime, LoopPolicy
from deepstrike.providers.stream import DoneEvent, TextDelta
from deepstrike.runtime.filtered_plane import FilteredExecutionPlane
from deepstrike.runtime.kernel_step import kernel_apply
from deepstrike.runtime.runner import RuntimeOptions, RuntimeRunner, SubAgentHarnessConfig
from deepstrike.runtime.session_log import SessionLog
from deepstrike.types.agent import (
  AgentRunSpec,
  AgentSpawnedObservation,
  LoopResult,
  SubAgentResult,
  agent_run_spec_to_kernel as spec_to_kernel,
)

if TYPE_CHECKING:
  from deepstrike.harness.harness import HarnessLoop


@dataclass
class SubAgentRunContext:
  parent_opts: RuntimeOptions
  parent_session_id: str
  spec: AgentRunSpec
  manifest: AgentSpawnedObservation
  session_log: SessionLog
  harness: SubAgentHarnessConfig | None = None


def _termination_from_status(status: str) -> str:
  normalized = status.lower()
  known = {
    "completed", "max_turns", "token_budget", "timeout",
    "user_abort", "error", "milestone_exceeded",
  }
  return normalized if normalized in known else status


def _manifest_from_obs(obs: dict, parent_session_id: str, spec: AgentRunSpec) -> AgentSpawnedObservation:
  return AgentSpawnedObservation(
    agent_id=str(obs.get("agent_id") or spec.identity.agent_id),
    parent_session_id=str(obs.get("parent_session_id") or parent_session_id),
    role=str(obs.get("role") or spec.role),
    isolation=str(obs.get("isolation") or spec.isolation),
    context_inheritance=str(obs.get("context_inheritance") or "none"),
    permitted_capability_ids=list(obs.get("permitted_capability_ids") or []),
    turn=obs.get("turn"),
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


def _build_child_opts(
  ctx: SubAgentRunContext,
  *,
  system_prompt: str | None,
  filtered_plane,
) -> RuntimeOptions:
  return RuntimeOptions(
    provider=ctx.parent_opts.provider,
    session_log=ctx.session_log,
    execution_plane=filtered_plane,
    max_tokens=ctx.parent_opts.max_tokens,
    max_turns=ctx.parent_opts.max_turns,
    timeout_ms=ctx.parent_opts.timeout_ms,
    agent_id=ctx.spec.identity.agent_id,
    system_prompt=system_prompt,
    initial_memory=ctx.parent_opts.initial_memory,
    skill_dir=ctx.parent_opts.skill_dir,
    dream_store=ctx.parent_opts.dream_store,
    knowledge_source=ctx.parent_opts.knowledge_source,
    signal_source=ctx.parent_opts.signal_source,
    extensions=ctx.parent_opts.extensions,
    governance=ctx.parent_opts.governance,
    tokenizer=ctx.parent_opts.tokenizer,
    enable_plan_tool=ctx.parent_opts.enable_plan_tool,
    compression_store=ctx.parent_opts.compression_store,
    on_tool_suspend=ctx.parent_opts.on_tool_suspend,
    on_permission_request=ctx.parent_opts.on_permission_request,
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
    from deepstrike.harness.harness import HarnessLoop, HarnessRequest

    permitted = set(ctx.manifest.permitted_capability_ids)
    from deepstrike.runtime.execution_plane import LocalExecutionPlane

    plane = ctx.parent_opts.execution_plane or LocalExecutionPlane()
    filtered = FilteredExecutionPlane(plane, permitted)
    child_runner = RuntimeRunner(_build_child_opts(
      ctx,
      system_prompt=ctx.parent_opts.system_prompt,
      filtered_plane=filtered,
    ))
    loop = HarnessLoop(
      child_runner,
      ctx.harness.eval_provider,
      max_attempts=ctx.harness.max_attempts,
    )
    outcome = await loop.run(HarnessRequest(
      goal=ctx.spec.goal,
      criteria=_harness_criteria(ctx.spec),
    ))

    from deepstrike._kernel import Message

    final_message = None
    if outcome.result:
      final_message = Message(role="assistant", content=outcome.result)

    loop_result = LoopResult(
      termination="completed" if outcome.passed else "error",
      turns_used=outcome.iterations,
      total_tokens_used=outcome.total_tokens,
      final_message=final_message,
    )
    return SubAgentResult(agent_id=ctx.spec.identity.agent_id, result=loop_result)

  async def _run_direct(self, ctx: SubAgentRunContext) -> SubAgentResult:
    permitted = set(ctx.manifest.permitted_capability_ids)
    system_prompt, inherit_events = await _resolve_inheritance(ctx)

    from deepstrike.runtime.execution_plane import LocalExecutionPlane

    plane = ctx.parent_opts.execution_plane or LocalExecutionPlane()
    filtered = FilteredExecutionPlane(plane, permitted)
    child_runner = RuntimeRunner(_build_child_opts(
      ctx,
      system_prompt=system_prompt,
      filtered_plane=filtered,
    ))

    done: DoneEvent | None = None
    final_text = ""
    async for evt in child_runner.run(
      session_id=ctx.spec.identity.session_id,
      goal=ctx.spec.goal,
      inherit_events=inherit_events,
    ):
      if isinstance(evt, TextDelta):
        final_text += evt.delta
      if isinstance(evt, DoneEvent):
        done = evt

    from deepstrike._kernel import Message

    final_message = Message(role="assistant", content=final_text) if final_text else None
    loop_result = LoopResult(
      termination=_termination_from_status(done.status if done else "error"),
      turns_used=done.iterations if done else 0,
      total_tokens_used=done.total_tokens if done else 0,
      final_message=final_message,
    )
    return SubAgentResult(agent_id=ctx.spec.identity.agent_id, result=loop_result)


default_sub_agent_orchestrator = SubAgentOrchestrator()


async def spawn_standalone(
  parent_opts: RuntimeOptions,
  parent_session_id: str,
  spec: AgentRunSpec,
  *,
  orchestrator: SubAgentOrchestrator | None = None,
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
  ))
