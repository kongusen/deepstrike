"""Python SDK → canonical kernel boundary regressions (P1 audit, 0.2.74).

Mirrors ``node/tests/boundary-p1-regressions.test.ts`` against the real native kernel.

P1-7:  the host re-applied the model's ``update_plan`` and wrote its own "Executed tools" progress.
P1-8:  skill content was pinned before (and regardless of) the kernel's admission.
P1-9:  the host rewrote the kernel-rendered tool surface and knowledge (governance prefilter), and
       lowered governance constraints with field names the kernel rejects.
P1-10: launch / preemption acknowledgements echoed "the last action" as started; child usage and
       attempt ids were invented.
P1-11: signal deadlines were dropped, a host clock was stamped, notes rode an id prefix.
P1-12: ``pending_call_ids`` carried provider effect ids.
P1-13: argument text that was not a JSON object ran the tool with ``{}``.
P1-14: the kernel's live control plane had no SDK surface.
P1-15: memory had two authorities — the host re-implemented write validation, skipped the write
       quota, and computed ``recall_count + 1`` and promotion itself; model recalls were never
       counted.
"""

import json

import pytest

from deepstrike import (
    GovernancePolicy, InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner,
    governance_policy_patch,
)
from deepstrike._kernel import ToolSchema
from deepstrike.kernel.canonical import CanonicalKernel
from deepstrike.providers.base import normalize_tool_call
from deepstrike.providers.stream import ErrorEvent, TextDelta, ToolCallEvent, ToolResultEvent
from deepstrike.runtime.canonical_kernel_step import CanonicalRunnerRuntime
from deepstrike.runtime.kernel_journal import InMemoryKernelJournal
from deepstrike.runtime.runner import _InboundSignalDelivery, _signal_to_kernel_event
from deepstrike.signals.types import RuntimeSignal
from deepstrike.tools.registry import RegisteredTool


class Scripted:
  """Plays one scripted turn per provider call, then answers "done"."""

  def __init__(self, turns):
    self.turns = turns
    self.contexts = []
    self.tool_names = []

  async def stream(self, context, tools, extensions=None, state=None):
    self.contexts.append(context)
    self.tool_names.append([getattr(t, "name", None) for t in (tools or [])])
    turn = self.turns[len(self.contexts) - 1] if len(self.contexts) <= len(self.turns) else [TextDelta(delta="done")]
    for event in turn:
      yield event


def _tool(name: str, fn, parameters: str = '{"type":"object"}') -> RegisteredTool:
  return RegisteredTool(fn, ToolSchema(name=name, description=name, parameters=parameters))


def _text(context) -> str:
  return json.dumps(context, default=lambda o: getattr(o, "__dict__", str(o)))


async def _drain(runner, session_id, goal="go"):
  return [event async for event in runner.run(goal=goal, session_id=session_id)]


def test_p1_13_normalize_keeps_non_object_text_verbatim():
  assert normalize_tool_call("c1", "write", '{"path": "/tm').arguments == '{"path": "/tm'
  assert normalize_tool_call("c1", "write", "[1]").arguments == "[1]"
  assert normalize_tool_call("c1", "write", "").arguments == "{}"
  assert normalize_tool_call("c1", "write", '{ "a" : 1 }').arguments == '{"a": 1}'


@pytest.mark.asyncio
async def test_p1_13_a_truncated_call_never_executes():
  executed = []

  async def write(**kwargs) -> str:
    executed.append(kwargs)
    return "wrote"

  provider = Scripted([[ToolCallEvent(id="c1", name="write", arguments={}, raw_arguments='{"path": "/tm')]])
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider, session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane().register(_tool("write", write)),
    max_tokens=8000, max_turns=4, baseline_tool_ids=["write"],
  ))
  events = await _drain(runner, "p1-13")
  assert executed == []
  result = next(e for e in events if isinstance(e, ToolResultEvent))
  assert result.is_error and "invalid arguments" in result.content
  assert "invalid arguments" in _text(provider.contexts[1])


@pytest.mark.asyncio
async def test_a_failed_tool_result_reaches_the_provider_marked_as_a_failure():
  async def fetch(**_kwargs) -> str:
    raise RuntimeError("upstream timeout")

  provider = Scripted([[ToolCallEvent(id="c-fail", name="fetch", arguments={})]])
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider, session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane().register(_tool("fetch", fetch)),
    max_tokens=8000, max_turns=4, baseline_tool_ids=["fetch"],
  ))
  await _drain(runner, "is-error")
  parts = [part for turn in provider.contexts[1].turns for part in (turn.content_parts or [])]
  result = next(p for p in parts if p.type == "tool_result" and p.call_id == "c-fail")
  assert result.is_error is True


@pytest.mark.asyncio
async def test_p1_7_the_host_never_writes_the_models_task_state():
  async def noop(**_kwargs) -> str:
    return "ok"

  provider = Scripted([
    [ToolCallEvent(id="plan", name="update_plan", arguments={"progress": "drafting section two"})],
    [ToolCallEvent(id="noop", name="noop", arguments={})],
  ])
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider, session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane().register(_tool("noop", noop)),
    max_tokens=8000, max_turns=5, baseline_tool_ids=["noop"], enable_plan_tool=True,
  ))
  events = await _drain(runner, "p1-7")
  assert not [e for e in events if isinstance(e, ErrorEvent)], [e.message for e in events if isinstance(e, ErrorEvent)]
  third = _text(provider.contexts[2])
  assert "drafting section two" in third
  assert "Executed tools" not in third


@pytest.mark.asyncio
async def test_p1_12_cancellation_names_logical_calls_only():
  runner: RuntimeRunner | None = None

  async def stop_me(**_kwargs) -> str:
    assert runner is not None
    runner.interrupt("user")
    return "ok"

  provider = Scripted([[ToolCallEvent(id="c1", name="stop_me", arguments={})]])
  session_log = InMemorySessionLog()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider, session_log=session_log,
    execution_plane=LocalExecutionPlane().register(_tool("stop_me", stop_me)),
    max_tokens=8000, max_turns=4, baseline_tool_ids=["stop_me"],
  ))
  await _drain(runner, "p1-12")
  entries = await session_log.read("p1-12")
  cancelled = next(e.event for e in entries if e.event.get("kind") == "operation_cancelled")
  assert cancelled["pending_call_ids"] == []


@pytest.mark.asyncio
async def test_p1_9_the_kernel_withholds_vetoed_tools_and_accepts_constraints():
  async def rm(**_kwargs) -> str:
    return "gone"

  async def ls(**_kwargs) -> str:
    return "files"

  async def first_request(surface: bool):
    provider = Scripted([])
    runner = RuntimeRunner(RuntimeOptions(
      provider=provider, session_log=InMemorySessionLog(),
      execution_plane=LocalExecutionPlane().register(_tool("rm", rm), _tool("ls", ls)),
      max_tokens=8000, max_turns=2, baseline_tool_ids=["rm", "ls"],
      governance_policy=GovernancePolicy(
        vetoes=["rm"], surface_denied_in_system=surface,
        constraints=[{"kind": "range", "tool": "ls", "path": "n", "min": 0, "max": 2.5}],
      ),
    ))
    events = await _drain(runner, f"p1-9-{surface}")
    assert not [e for e in events if isinstance(e, ErrorEvent)], [e.message for e in events if isinstance(e, ErrorEvent)]
    return provider.tool_names[0], provider.contexts[0].system_knowledge or ""

  tools, knowledge = await first_request(True)
  assert "ls" in tools and "rm" not in tools
  assert knowledge.count("[governance]") == 1

  tools, knowledge = await first_request(False)
  assert "rm" in tools
  assert "[governance]" not in knowledge


@pytest.mark.asyncio
async def test_p1_8_skill_content_follows_the_kernels_admission(tmp_path):
  (tmp_path / "debug.md").write_text("---\nname: debug\ndescription: d\n---\nDEBUG-GUIDANCE\n")
  (tmp_path / "ghost.md").write_text("---\nname: ghost\ndescription: g\n---\nGHOST-GUIDANCE\n")

  for name, admitted in (("debug", True), ("ghost", False)):
    provider = Scripted([[ToolCallEvent(id="s1", name="skill", arguments={"name": name})]])
    runner = RuntimeRunner(RuntimeOptions(
      provider=provider, session_log=InMemorySessionLog(), execution_plane=LocalExecutionPlane(),
      max_tokens=8000, max_turns=3, skill_dir=str(tmp_path),
      # The kernel catalog admits only `debug`.
      skill_filter=["debug"],
    ))
    # Widen the host's own filter once the catalog is sent, so the host stages `ghost` content the
    # kernel never declared: only the kernel's refusal can keep it from the model.
    staged: list[str] = []
    real_stream = provider.stream

    async def widening_stream(context, tools, extensions=None, state=None):
      runner._opts.skill_filter = None
      async for event in real_stream(context, tools, extensions, state):
        yield event
    provider.stream = widening_stream  # type: ignore[method-assign]
    original = runner.__class__.__dict__  # keep linters quiet about unused names
    del original

    from deepstrike.runtime import runner as runner_module
    real_apply = runner_module.apply_host

    async def recording_apply(runtime, pending, event):
      if event.get("kind") == "add_knowledge_message" and str(event.get("key", "")).startswith("skill:"):
        staged.append(event["key"])
      return await real_apply(runtime, pending, event)
    runner_module.apply_host = recording_apply
    try:
      events = await _drain(runner, f"p1-8-{name}")
    finally:
      runner_module.apply_host = real_apply
    assert f"skill:{name}" in staged, "the host staged the content before the kernel adjudicated"
    assert not [e for e in events if isinstance(e, ErrorEvent)]
    second = _text(provider.contexts[1])
    if admitted:
      assert "DEBUG-GUIDANCE" in second
    else:
      assert "GHOST-GUIDANCE" not in second


@pytest.mark.asyncio
async def test_p1_10_launch_outcomes_are_the_hosts_report():
  rt = CanonicalRunnerRuntime(CanonicalKernel(), InMemoryKernelJournal(), "op-spawn", max_context_tokens=100_000)
  await rt.apply_host_event({"kind": "configure_run", "config": {"resource_quota": {"max_concurrent_subagents": 4}}})
  spawn = await rt.start_workflow({"nodes": [
    {"node_id": "a", "task": "a", "role": "implement"}, {"node_id": "b", "task": "b", "role": "implement"},
  ]})
  assert spawn is not None and spawn.kind == "spawn_workflow"
  rt.drain_host_observations()
  await rt.apply_host_event({
    "kind": "workflow_spawn_result", "effect_id": spawn.effect_id,
    "started_agent_ids": ["wf-node0"],
    "failures": [{"agent_id": "wf-node1", "error": "no slot", "kind": "resource_exhausted"}],
  })
  batch = next(o for o in rt.drain_host_observations() if o.get("kind") == "workflow_batch_spawned")
  assert [n["agent_id"] for n in batch["nodes"]] == ["wf-node0"]
  with pytest.raises(RuntimeError, match="not a pending spawn_tasks effect"):
    await rt.apply_host_event({"kind": "workflow_spawn_result", "effect_id": "nope"})


@pytest.mark.asyncio
async def test_p1_10_a_completion_without_a_kernel_attempt_id_is_refused():
  journal = InMemoryKernelJournal()
  rt = CanonicalRunnerRuntime(CanonicalKernel(), journal, "op-attempt", max_context_tokens=100_000)
  await rt.apply_host_event({"kind": "configure_run", "config": {"resource_quota": {"max_concurrent_subagents": 4}}})
  spawn = await rt.start_workflow({"nodes": [{"node_id": "a", "task": "a", "role": "implement"}]})
  await rt.apply_host_event({"kind": "workflow_spawn_result", "effect_id": spawn.effect_id})
  restored = CanonicalRunnerRuntime(CanonicalKernel(), journal, "op-attempt", max_context_tokens=100_000)
  await restored.restore()
  completion = {"kind": "sub_agent_completed", "result": {
    "agent_id": "wf-node0", "result": {"termination": "completed", "turns_used": 1, "total_tokens_used": 5}}}
  with pytest.raises(RuntimeError, match="no kernel-minted attempt id"):
    await restored.apply_host_event(completion)
  await restored.apply_host_event({**completion, "attempt_id": spawn.nodes[0]["attempt_id"]})


def test_p1_11_signals_speak_the_logical_signal_vocabulary():
  async def _ok() -> bool:
    return True

  note = _InboundSignalDelivery(
    "s1", "any-delivery-id", 1,
    RuntimeSignal(source="custom", signal_type="event", urgency="normal", payload={"goal": "x"}),
    _ok, _ok, "check the build",
  )
  assert _signal_to_kernel_event(note)["signal"]["payload"] == "check the build"


@pytest.mark.asyncio
async def test_p1_11_the_kernel_admits_deadline_signals_and_notes():
  async def _ok() -> bool:
    return True

  rt = CanonicalRunnerRuntime(CanonicalKernel(), InMemoryKernelJournal(), "op-signal", max_context_tokens=100_000)
  await rt.start_agent({"goal": "wait"})
  for delivery in (
    _InboundSignalDelivery("s1", "d1", 1, RuntimeSignal(
      source="cron", signal_type="job", urgency="low", payload={"job": "nightly"}, deadline_ms=2_000), _ok, _ok),
    _InboundSignalDelivery("s2", "d2", 1, RuntimeSignal(
      source="custom", signal_type="event", urgency="normal"), _ok, _ok, "deploy finished"),
  ):
    await rt.apply_host_event(_signal_to_kernel_event(delivery, now_ms=1_000))
  disposed = [o for o in rt.drain_host_observations() if o.get("kind") == "signal_delivery_disposed"]
  assert len(disposed) == 2


@pytest.mark.asyncio
async def test_p1_14_live_policy_patches_reach_the_kernel_under_revision_control():
  runner: RuntimeRunner | None = None
  outcomes: dict[str, object] = {}

  async def poke(**_kwargs) -> str:
    import asyncio

    assert runner is not None
    if not outcomes:
      outcomes["ok"] = asyncio.ensure_future(runner.apply_policy_patch(governance_policy_patch(GovernancePolicy(vetoes=["poke"]))))
      outcomes["stale"] = asyncio.ensure_future(runner.apply_policy_patch(
        governance_policy_patch(GovernancePolicy(vetoes=["x"])), expected_revision=9))
      outcomes["deadline"] = asyncio.ensure_future(runner.update_deadline(None))
      outcomes["compact"] = asyncio.ensure_future(runner.force_compact())
    return "poked"

  provider = Scripted([[ToolCallEvent(id=f"p{i}", name="poke", arguments={"i": i})] for i in range(3)])
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider, session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane().register(_tool("poke", poke)),
    max_tokens=8000, max_turns=6, baseline_tool_ids=["poke"], repeat_fuse=False,
  ))
  await _drain(runner, "p1-14")
  await outcomes["ok"]
  await outcomes["deadline"]
  await outcomes["compact"]
  with pytest.raises(Exception, match="revision mismatch"):
    await outcomes["stale"]
  assert "[governance]" in (provider.contexts[-1].system_knowledge or "")
  with pytest.raises(RuntimeError, match="requires an active run"):
    await runner.force_compact()


# ---------------------------------------------------------------------------------------------
# P1-15 · memory has one authority — the kernel
# ---------------------------------------------------------------------------------------------

from deepstrike.memory.protocols import (  # noqa: E402
    MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord, MemoryScope,
)
from deepstrike.runtime.runner import MemoryPolicy, MemoryWriteRateLimit, ResourceQuota  # noqa: E402

_SCOPE = MemoryScope(tenant_id="p1-15", namespace="memory")


def _stored(record_id: str, recall_count: int = 0, pinned: bool = False) -> MemoryRecord:
  return MemoryRecord(
    record_id=record_id, scope=_SCOPE, name=record_id, kind="reference", content=f"{record_id} body",
    description="fixture", provenance=MemoryProvenance(author="host", trust="host_verified"),
    created_at=1, updated_at=1, recall_count=recall_count, pinned=pinned,
  )


class _Store:
  def __init__(self, hits=()):
    self.hits = list(hits)
    self.puts: list[str] = []
    self.recalls: list[list] = []

  async def put(self, _agent_id, record):
    self.puts.append(record.record_id)

  async def get(self, _agent_id, _record_id):
    return None

  async def delete(self, _agent_id, _record_id):
    return None

  async def save_session(self, _session):
    return None

  async def search(self, _agent_id, _query):
    return [MemoryRecall(record=record, score=0.9, why="fixture") for record in self.hits]

  async def record_recall(self, _agent_id, recalls):
    self.recalls.append(list(recalls))


@pytest.mark.asyncio
async def test_p1_15_a_model_memory_query_mirrors_the_kernel_derived_count():
  store = _Store([_stored("crossing", 2)])
  promotions = []
  provider = Scripted([[ToolCallEvent(id="m1", name="memory", arguments={"query": "prefs"})]])
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider, session_log=InMemorySessionLog(), execution_plane=LocalExecutionPlane(),
    max_tokens=8000, max_turns=4, agent_id="p1-15", memory_scope=_SCOPE, memory_store=store,
    pre_query_memory=lambda goal: [], memory_policy=MemoryPolicy(promotion_recall_threshold=3),
    on_promotion_suggested=lambda **promotion: promotions.append(promotion),
  ))
  await _drain(runner, "p1-15-query")
  assert [(r.record_id, r.recall_count) for batch in store.recalls for r in batch] == [("crossing", 3)]
  assert promotions == [{"record_id": "crossing", "recall_count": 3}]


@pytest.mark.asyncio
async def test_p1_15_a_host_write_in_a_live_run_answers_to_the_kernel_write_quota():
  store = _Store()
  verdicts = []
  holder = {}

  async def remember_twice(**_kwargs) -> str:
    for record_id in ("first", "second"):
      verdicts.append(await holder["runner"].write_memory(_stored(record_id)))
    return "ok"

  provider = Scripted([[ToolCallEvent(id="w1", name="remember_twice", arguments={})]])
  holder["runner"] = runner = RuntimeRunner(RuntimeOptions(
    provider=provider, session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane().register(_tool("remember_twice", remember_twice)),
    max_tokens=8000, max_turns=4, baseline_tool_ids=["remember_twice"],
    agent_id="p1-15", memory_scope=_SCOPE, memory_store=store, pre_query_memory=lambda goal: [],
    resource_quota=ResourceQuota(memory_writes_per_window=MemoryWriteRateLimit(max_writes=1, window_ms=60_000)),
  ))
  await _drain(runner, "p1-15-quota")
  assert verdicts == [True, False]
  assert store.puts == ["first"]


@pytest.mark.asyncio
async def test_p1_15_a_host_write_with_no_live_run_is_judged_by_the_kernel_rule():
  def make(policy=None):
    return RuntimeRunner(RuntimeOptions(
      provider=Scripted([]), session_log=InMemorySessionLog(), agent_id="p1-15", memory_store=_Store(),
      **({"memory_policy": policy} if policy is not None else {}),
    ))

  wide = _stored("wide")
  wide.name = "记" * 100
  assert await make().write_memory(wide) is True, "the name limit counts characters, not bytes"
  wide.name = "记" * 101
  assert await make().write_memory(wide) is False
  big = _stored("big")
  big.content = "12345"
  assert await make(MemoryPolicy(max_content_bytes=4)).write_memory(big) is False
  assert await make(MemoryPolicy(max_content_bytes=4, validation_enabled=False)).write_memory(big) is True


@pytest.mark.asyncio
async def test_p1_15_a_host_recall_derives_its_counts_in_the_kernel():
  store = _Store([_stored("a", 1), _stored("a", 1), _stored("pinned", 1, pinned=True)])
  promotions = []
  runner = RuntimeRunner(RuntimeOptions(
    provider=Scripted([]), session_log=InMemorySessionLog(), agent_id="p1-15", memory_store=store,
    memory_policy=MemoryPolicy(promotion_recall_threshold=2),
    on_promotion_suggested=lambda **promotion: promotions.append(promotion),
  ))
  await runner.query_memory(MemoryQuery(scope=_SCOPE, query="a"))
  assert [(r.record_id, r.recall_count) for batch in store.recalls for r in batch] == [("a", 2), ("pinned", 2)]
  assert promotions == [{"record_id": "a", "recall_count": 2}]
