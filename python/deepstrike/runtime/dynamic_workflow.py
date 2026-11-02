"""Plan task 6.3 · dynamic workflows: a host program decides, step by step, which agents to run.

The program is ordinary async Python. Each ``agent`` (or ``parallel_agents`` batch) becomes one
submission the kernel admits onto the operation's dynamic workflow root — the kernel still owns the
DAG, its budgets and every spawn — and the program awaits the typed result before deciding what
comes next. The vocabulary matches the Node SDK's ``runDynamicWorkflow``; replay of a saved run and
the sandboxed script executors are Node-only for now.

    async def program(ctx):
        notes = await ctx.parallel_agents([agent_task(f"survey {area}") for area in areas])
        await ctx.blackboard.put("criteria", "primary sources only")
        async with ctx.phase("synthesis"):
            return await ctx.agent("synthesize: " + "\\n".join(n.text for n in notes if n))

    run = await runner.run_dynamic_workflow(program)
"""

from __future__ import annotations

import asyncio
import hashlib
import json
from contextlib import asynccontextmanager
from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Awaitable, Callable, Literal

from deepstrike.types.agent import WorkflowNodeSpec, WorkflowOutcome, WorkflowSpec


@dataclass(frozen=True)
class DynamicWorkflowLimits:
  max_agents_per_run: int = 64
  max_concurrent_agents: int = 8
  max_batch_size: int = 16


@dataclass(frozen=True)
class DynamicWorkflowAgentOptions:
  label: str | None = None
  role: str = "implement"
  model_hint: str | None = None
  trust: str | None = None
  isolation: str | None = None
  output_schema: dict[str, Any] | None = None
  token_budget: int | None = None
  max_turns: int | None = None
  max_wall_ms: int | None = None


@dataclass(frozen=True)
class DynamicWorkflowAgentRequest:
  prompt: str
  options: DynamicWorkflowAgentOptions = field(default_factory=DynamicWorkflowAgentOptions)


def agent_task(prompt: str, options: DynamicWorkflowAgentOptions | None = None) -> DynamicWorkflowAgentRequest:
  if not prompt.strip():
    raise ValueError("dynamic workflow agent prompt must not be empty")
  return DynamicWorkflowAgentRequest(prompt, options or DynamicWorkflowAgentOptions())


@dataclass(frozen=True)
class DynamicWorkflowAgentResult:
  value: Any
  text: str
  node_id: str
  status: str
  termination: str | None = None


@dataclass
class DynamicWorkflowPhaseProgress:
  name: str
  status: Literal["running", "completed", "failed"] = "running"
  agents_started: int = 0
  agents_completed: int = 0


@dataclass
class DynamicWorkflowProgress:
  run_id: str
  agents_started: int = 0
  agents_completed: int = 0
  active_agents: int = 0
  phase: str | None = None
  phases: list[DynamicWorkflowPhaseProgress] = field(default_factory=list)
  logs: list[dict[str, Any]] = field(default_factory=list)


@dataclass
class DynamicWorkflowRun:
  run_id: str
  value: Any
  progress: DynamicWorkflowProgress


class DynamicWorkflowLimitError(RuntimeError):
  pass


class DynamicWorkflowHost:
  """What the context needs from the runner: admit and drive one submission, write the blackboard."""

  async def run_workflow(self, spec: WorkflowSpec, plan: dict[str, Any]) -> WorkflowOutcome:  # pragma: no cover
    raise NotImplementedError

  async def write_blackboard(self, key: str, content: str | None) -> None:  # pragma: no cover
    raise NotImplementedError


def _fingerprint(prompt: str, options: DynamicWorkflowAgentOptions) -> str:
  material = json.dumps({"prompt": prompt, "options": options.__dict__}, sort_keys=True, default=str)
  return "sha256:" + hashlib.sha256(material.encode()).hexdigest()


class DynamicWorkflowBlackboard:
  """Plan task 6.2 · state the program shares with the agents it runs. Each entry is a kernel item
  (``blackboard:<key>``) every agent spawned after the write starts with, whatever its context
  inheritance; a phase-scoped entry leaves when its phase ends. Non-string values are written as
  JSON."""

  def __init__(self, context: "DynamicWorkflowContext") -> None:
    self._context = context
    self._entries: dict[str, tuple[Any, DynamicWorkflowPhaseProgress | None]] = {}

  async def put(self, key: str, value: Any, *, scope: Literal["workflow", "phase"] = "workflow") -> None:
    if not key or len(key) > 128 or not all(c.isalnum() or c in "_.:-" for c in key):
      raise ValueError("dynamic workflow blackboard keys are 1-128 characters of letters, digits, _ . : -")
    if scope not in ("workflow", "phase"):
      raise ValueError(f"unknown blackboard scope {scope!r}")
    phase = self._context._phase_stack[-1] if scope == "phase" and self._context._phase_stack else None
    if scope == "phase" and phase is None:
      raise ValueError("a phase-scoped blackboard entry needs an enclosing phase")
    content = value if isinstance(value, str) else json.dumps(value, sort_keys=True)
    self._entries[key] = (value, phase)
    await self._context._host.write_blackboard(key, content)

  def get(self, key: str) -> Any:
    entry = self._entries.get(key)
    return entry[0] if entry else None

  async def delete(self, key: str) -> None:
    if self._entries.pop(key, None) is not None:
      await self._context._host.write_blackboard(key, None)

  async def _end_phase(self, phase: DynamicWorkflowPhaseProgress) -> None:
    for key in [key for key, (_, owner) in self._entries.items() if owner is phase]:
      await self.delete(key)


class DynamicWorkflowContext:
  """The program's view of its run: agents, batches, phases, a log and the blackboard."""

  def __init__(self, host: DynamicWorkflowHost, run_id: str, args: dict[str, Any], limits: DynamicWorkflowLimits) -> None:
    self._host = host
    self._limits = limits
    self.args = dict(args)
    self.progress = DynamicWorkflowProgress(run_id=run_id)
    self.blackboard = DynamicWorkflowBlackboard(self)
    self._phase_stack: list[DynamicWorkflowPhaseProgress] = []
    self._next_node = 0
    self._sequence = 0
    self._slots = asyncio.Semaphore(limits.max_concurrent_agents)

  async def agent(self, prompt: str, options: DynamicWorkflowAgentOptions | None = None) -> DynamicWorkflowAgentResult | None:
    return (await self._run_batch([agent_task(prompt, options)]))[0]

  async def parallel_agents(self, requests: list[DynamicWorkflowAgentRequest]) -> list[DynamicWorkflowAgentResult | None]:
    if len(requests) > self._limits.max_batch_size:
      raise DynamicWorkflowLimitError(f"parallel_agents exceeded max_batch_size ({self._limits.max_batch_size})")
    return await self._run_batch(requests)

  async def parallel(self, items: list[Any], worker: Callable[[Any, int], Awaitable[Any]]) -> list[Any]:
    if len(items) > self._limits.max_batch_size:
      raise DynamicWorkflowLimitError(f"parallel exceeded max_batch_size ({self._limits.max_batch_size})")
    return list(await asyncio.gather(*(worker(item, index) for index, item in enumerate(items))))

  async def pipeline(self, items: list[Any], worker: Callable[[Any, int], Awaitable[Any]]) -> list[Any]:
    return [await worker(item, index) for index, item in enumerate(items)]

  @asynccontextmanager
  async def phase(self, name: str) -> AsyncIterator[DynamicWorkflowPhaseProgress]:
    if not name.strip():
      raise ValueError("dynamic workflow phase name must not be empty")
    phase = DynamicWorkflowPhaseProgress(name=name)
    self.progress.phases.append(phase)
    self._phase_stack.append(phase)
    self.progress.phase = name
    try:
      yield phase
      phase.status = "completed"
    except BaseException:
      phase.status = "failed"
      raise
    finally:
      self._phase_stack.pop()
      self.progress.phase = self._phase_stack[-1].name if self._phase_stack else None
      await self.blackboard._end_phase(phase)

  def log(self, message: str, **fields: Any) -> None:
    self.progress.logs.append({"message": message, **({"fields": fields} if fields else {})})

  async def _run_batch(self, requests: list[DynamicWorkflowAgentRequest]) -> list[DynamicWorkflowAgentResult | None]:
    if not requests:
      return []
    if self.progress.agents_started + len(requests) > self._limits.max_agents_per_run:
      raise DynamicWorkflowLimitError(
        f"dynamic workflow exceeded max_agents_per_run ({self._limits.max_agents_per_run})")
    sequence = self._sequence
    self._sequence += 1
    entries = []
    for request in requests:
      options = request.options
      node_id = (options.label or "").strip() or f"dynamic-agent-{self._next_node}"
      self._next_node += 1
      entries.append((node_id, request, _fingerprint(request.prompt, options)))
    spec = WorkflowSpec(nodes=[
      WorkflowNodeSpec(
        node_id=node_id,
        task=request.prompt,
        role=request.options.role,  # type: ignore[arg-type]
        **{name: value for name, value in (
          ("model_hint", request.options.model_hint),
          ("trust", request.options.trust),
          ("isolation", request.options.isolation),
          ("output_schema", request.options.output_schema),
          ("token_budget", request.options.token_budget),
          ("max_turns", request.options.max_turns),
          ("max_wall_ms", request.options.max_wall_ms),
        ) if value is not None},
      )
      for node_id, request, _ in entries
    ])
    plan = {
      "run_id": self.progress.run_id,
      "sequence": sequence,
      "nodes": [
        {"node_id": node_id, "depends_on": [], "prompt_fingerprint": fingerprint, "replay": "executed"}
        for node_id, _, fingerprint in entries
      ],
    }
    phase = self._phase_stack[-1] if self._phase_stack else None
    self.progress.agents_started += len(entries)
    self.progress.active_agents += len(entries)
    if phase:
      phase.agents_started += len(entries)
    async with self._slots:
      try:
        outcome = await self._host.run_workflow(spec, plan)
      finally:
        self.progress.active_agents -= len(entries)
    if outcome.rejection is not None:
      raise RuntimeError(f"dynamic workflow submission rejected: {outcome.rejection.reason}")
    by_node = {o.node_id: o for o in outcome.node_outcomes}
    results: list[DynamicWorkflowAgentResult | None] = []
    for node_id, request, _ in entries:
      node = by_node.get(node_id)
      text = outcome.outputs.get(node_id)
      self.progress.agents_completed += 1
      if phase:
        phase.agents_completed += 1
      if text is None and (node is None or node.status != "completed"):
        results.append(None)
        continue
      text = text or ""
      value: Any = text
      if request.options.output_schema is not None:
        try:
          value = json.loads(text)
        except ValueError:
          value = None
      results.append(DynamicWorkflowAgentResult(
        value=value,
        text=text,
        node_id=node_id,
        status=node.status if node else "completed",
        termination=node.termination if node else None,
      ))
    return results
