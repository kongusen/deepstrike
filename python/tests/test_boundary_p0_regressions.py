"""Python SDK → canonical kernel boundary regressions (P0 audit, 0.2.74).

Mirrors ``node/tests/boundary-p0-regressions.test.ts`` and
``wasm/tests/boundary-p0-regressions.node.mjs`` against the real native kernel.

P0-1: preloaded history kept tool results but dropped the assistant tool calls they answer.
P0-2: a model-authored workflow the canonical DAG cannot express must come back as a syscall
      rejection instead of failing the run.
P0-3: caller node ids were discarded and every append restarted at ``wf-node0``.
P0-4: milestone criteria / required evidence never reached ``on_milestone_evaluate``.
"""

import pytest

from deepstrike import (
    InMemorySessionLog, LocalExecutionPlane, MilestoneContract, MilestonePhase, ModelMessage,
    RuntimeOptions, RuntimeRunner,
)
from deepstrike._kernel import ContentPartObj, ToolCall
from deepstrike.kernel.canonical import CanonicalKernel
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime.canonical_kernel_step import CanonicalRunnerRuntime
from deepstrike.runtime.kernel_journal import InMemoryKernelJournal
from deepstrike.runtime.kernel_step import message_to_kernel
from deepstrike.types.agent import start_workflow_tool

TOOLS = [{"name": "search", "description": "s", "parameters": {"type": "object", "properties": {}}}]


def _runtime(name: str) -> CanonicalRunnerRuntime:
  return CanonicalRunnerRuntime(
    CanonicalKernel(), InMemoryKernelJournal(), f"op-{name}", max_context_tokens=100_000,
  )


@pytest.mark.asyncio
async def test_p0_1_preloaded_history_keeps_tool_call_pairing():
  rt = _runtime("history")
  await rt.apply_host_event({"kind": "set_tools", "tools": TOOLS})
  history = [
    ModelMessage(role="user", content="find x"),
    ModelMessage(role="assistant", content="", tool_calls=[ToolCall(id="call_1", name="search", arguments='{"q":"x"}')]),
    ModelMessage(role="tool", content="", content_parts=[
      ContentPartObj(type="tool_result", call_id="call_1", output="result-x", is_error=False),
    ]),
  ]
  await rt.apply_host_event({"kind": "preload_history", "messages": [message_to_kernel(m) for m in history]})
  action = await rt.start_agent({"goal": "continue"})
  assert action is not None and action.kind == "call_provider"

  turns = action.context.turns
  call = next(i for i, t in enumerate(turns) if any(c.id == "call_1" for c in (t.tool_calls or [])))
  result = next(i for i, t in enumerate(turns)
                if any(p.type == "tool_result" and p.call_id == "call_1" for p in (t.content_parts or [])))
  assert turns[call].tool_calls[0].name == "search"
  assert result > call
  assert turns[result].content == "result-x"


@pytest.mark.asyncio
async def test_p0_2_inexpressible_model_workflow_is_a_kernel_rejection():
  assert all(f'"{key}"' not in start_workflow_tool["parameters"]
             for key in ("loop", "classify", "tournament", "reducer", "dep_policy", "quarantined"))

  rt = _runtime("loop")
  await rt.apply_host_event({"kind": "set_tools", "tools": [
    *TOOLS, {"name": "start_workflow", "description": "w", "parameters": {"type": "object"}},
  ]})
  action = await rt.start_agent({"goal": "g"}, {"goal": "g", "exposure_baseline": ["start_workflow"]})
  following = await rt.apply_host_event({
    "kind": "provider_result",
    "effect_id": action.effect_id,
    "message": {"role": "assistant", "content": "", "tool_calls": [{
      "id": "c1", "name": "start_workflow",
      "arguments": {"spec": {"nodes": [{"task": "t", "role": "implement", "loop": {"max_iters": 2}}]}},
    }]},
  })
  # The run continues: the kernel answered the malformed syscall with a model-visible rejection.
  assert following is not None and following.kind == "call_provider"


@pytest.mark.asyncio
async def test_p0_3_caller_ids_survive_and_appended_anonymous_ids_are_unique():
  rt = _runtime("ids")
  await rt.apply_host_event({"kind": "configure_run", "config": {"resource_quota": {"max_concurrent_subagents": 4}}})
  start = await rt.start_workflow({"nodes": [{"node_id": "alpha", "task": "a", "role": "implement"}]})
  assert start is not None and start.kind == "spawn_workflow"
  assert [(n["task_id"], n["node_id"]) for n in start.nodes] == [("wf-node0", "alpha")]
  await rt.apply_host_event({"kind": "workflow_spawn_result", "effect_id": start.effect_id})

  grown = await rt.apply_host_event({"kind": "sub_agent_completed", "result": {
    "agent_id": "wf-node0",
    "result": {"termination": "completed", "final_message": {"content": "done"}, "turns_used": 1, "total_tokens_used": 1},
    "submitted_nodes": [{"task": "anonymous", "role": "implement"}],
  }})
  assert grown is not None and grown.kind == "spawn_workflow"
  assert [(n["task_id"], n["node_id"]) for n in grown.nodes] == [("wf-node1", "wf-node1")]


@pytest.mark.asyncio
async def test_p0_4_milestone_evaluation_receives_host_requirements():
  class Provider:
    async def stream(self, context, tools, extensions=None, state=None):
      yield TextDelta(delta="done")

  seen: list[dict] = []

  def evaluate(ctx):
    seen.append(ctx)
    from deepstrike.types.agent import MilestoneCheckResult
    return MilestoneCheckResult(phase_id=ctx["phaseId"], passed=True)

  runner = RuntimeRunner(RuntimeOptions(
    provider=Provider(),
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=4000,
    max_turns=4,
    milestone_contract=MilestoneContract(phases=[
      MilestonePhase(id="phase1", criteria=["tests pass"], required_evidence=["test log"]),
    ]),
    on_milestone_evaluate=evaluate,
  ))
  async for _event in runner.run(goal="ship it", session_id="p0-4"):
    pass

  assert seen, "the milestone phase was evaluated"
  assert seen[0] == {"phaseId": "phase1", "criteria": ["tests pass"], "requiredEvidence": ["test log"]}


@pytest.mark.asyncio
async def test_p0_5_on_tool_result_redaction_covers_every_consumer():
  import json

  from deepstrike._kernel import ToolSchema
  from deepstrike.providers.stream import ToolCallEvent, ToolResultEvent

  class SecretPlane:
    def register(self, *tools):
      return self

    def unregister(self, name):
      return self

    def schemas(self):
      return [ToolSchema(name="fetch", description="fetch", parameters='{"type":"object"}')]

    async def execute_all(self, calls, ctx):
      for call in calls:
        yield ToolResultEvent(
          call_id=call.id, name=call.name, content="SECRET-TOKEN",
          content_parts=[{"type": "text", "text": "SECRET-TOKEN"}],
        )

  class FetchThenStop:
    def __init__(self):
      self.contexts = []

    async def stream(self, context, tools, extensions=None, state=None):
      self.contexts.append(context)
      if len(self.contexts) == 1:
        yield ToolCallEvent(id="call_fetch", name="fetch", arguments={})
      else:
        yield TextDelta(delta="done")

  provider = FetchThenStop()
  session_log = InMemorySessionLog()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    execution_plane=SecretPlane(),
    max_tokens=8000,
    max_turns=4,
    baseline_tool_ids=["fetch"],
    on_tool_result=lambda _result: {"replace_output": "[redacted]"},
  ))
  async for _event in runner.run(goal="fetch it", session_id="p0-5"):
    pass

  assert len(provider.contexts) >= 2
  second = provider.contexts[1]
  rendered = repr(second.turns) + repr(getattr(second, "state_turn", None))
  assert "SECRET-TOKEN" not in rendered
  assert "[redacted]" in rendered
  logged = json.dumps([entry for entry in await session_log.read("p0-5")], default=repr)
  assert "SECRET-TOKEN" not in logged


@pytest.mark.asyncio
async def test_model_memory_query_is_resolved_by_the_main_loop():
  """The model's ``memory`` syscall publishes ``query_memory``; the main loop must resolve it with
  real store hits (it used to fall through to the unhandled-effect backstop and fail the run)."""
  from deepstrike.memory.protocols import MemoryProvenance, MemoryRecall, MemoryRecord, MemoryScope
  from deepstrike.providers.stream import ErrorEvent, ToolCallEvent

  scope = MemoryScope("tenant", "agent-memory")
  record = MemoryRecord(
    record_id="record-tests", scope=scope, name="tests", kind="feedback",
    content="Use small focused tests.", description="testing preference",
    provenance=MemoryProvenance(author="host", trust="host_verified"),
    created_at=1, updated_at=1, confidence=0.9,
  )

  class Store:
    async def put(self, agent_id, rec): pass
    async def get(self, agent_id, record_id): return None
    async def delete(self, agent_id, record_id): pass
    async def save_session(self, data): pass
    async def search(self, agent_id, query):
      return [MemoryRecall(record=record, score=0.9, why="fixture")]

  class RecallThenStop:
    def __init__(self):
      self.contexts = []

    async def stream(self, context, tools, extensions=None, state=None):
      self.contexts.append(context)
      if len(self.contexts) == 1:
        yield ToolCallEvent(id="call_mem", name="memory", arguments={"query": "tests"})
      else:
        yield TextDelta(delta="done")

  provider = RecallThenStop()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=8000,
    max_turns=4,
    agent_id="agent-memory",
    memory_store=Store(),
    memory_scope=scope,
  ))
  events = [event async for event in runner.run(goal="recall preferences", session_id="mem-q")]

  assert not [e for e in events if isinstance(e, ErrorEvent)], [e.message for e in events if isinstance(e, ErrorEvent)]
  assert len(provider.contexts) >= 2
  assert "Use small focused tests." in repr(provider.contexts[1].turns) + str(provider.contexts[1].system_knowledge)


async def _pressured(name: str):
  """Drive tool turns under a small budget until context pressure publishes a page-out."""
  rt = CanonicalRunnerRuntime(CanonicalKernel(), InMemoryKernelJournal(), f"op-{name}", max_context_tokens=4_000)
  await rt.apply_host_event({"kind": "set_tools", "tools": [
    {"name": "ping", "description": "ping", "parameters": {"type": "object"}},
  ]})
  action = await rt.start_agent({"goal": "keep pinging"}, {"goal": "keep pinging", "exposure_baseline": ["ping"]})
  for turn in range(40):
    if action.kind == "archive_page_out":
      return rt, action
    if action.kind == "call_provider":
      action = await rt.apply_host_event({
        "kind": "provider_result", "effect_id": action.effect_id,
        "message": {"role": "assistant", "content": "", "tool_calls": [
          {"id": f"call_{turn}", "name": "ping", "arguments": {"n": turn}},
        ]},
        "observed_input_tokens": 3_900, "stop_reason": "tool_use",
      })
    elif action.kind == "execute_tool":
      action = await rt.apply_host_event({
        "kind": "tool_results", "effect_id": action.effect_id,
        "results": [{"call_id": action.calls[0].id,
                     "output": f"pong {turn}: " + "a long tool body worth compacting " * 20, "is_error": False}],
      })
    else:
      raise AssertionError(f"unexpected effect while building pressure: {action.kind}")
  raise AssertionError("context pressure never published an archive_page_out")


@pytest.mark.asyncio
async def test_p0_6_page_out_without_a_stored_ref_fails_instead_of_minting_one():
  rt, action = await _pressured("page-out-missing")
  assert action.archive_payload and action.archive_payload.get("content")
  await rt.apply_host_event({"kind": "page_out_archive_result", "effect_id": action.effect_id})
  kinds = [obs.get("kind") for obs in rt.drain_host_observations()]
  assert "page_out_archive_failed" in kinds
  assert "payload_residency_changed" not in kinds

  rt, action = await _pressured("page-out-stored")
  await rt.apply_host_event({"kind": "page_out_archive_result", "effect_id": action.effect_id, "payload_ref": "payload:stored"})
  residency = [obs for obs in rt.drain_host_observations() if obs.get("kind") == "payload_residency_changed"]
  assert residency and residency[0].get("payload_ref") == "payload:stored"


@pytest.mark.asyncio
async def test_p0_6_the_runner_stores_the_archive_body_it_reports():
  """Without a compression store the runner must still persist the opaque body under the ref it
  reports, so a later ``load_payload`` finds it."""
  from deepstrike.runtime.payload_store import PayloadStore
  from deepstrike.providers.stream import ErrorEvent, ToolCallEvent
  from deepstrike._kernel import ToolSchema as KernelToolSchema
  from deepstrike.tools.registry import RegisteredTool

  class Provider:
    def __init__(self):
      self.turn = 0

    async def stream(self, context, tools, extensions=None, state=None):
      self.turn += 1
      if self.turn < 30:
        yield ToolCallEvent(id=f"call_{self.turn}", name="ping", arguments={"n": self.turn})
      else:
        yield TextDelta(delta="done")

  async def ping(**_kwargs) -> str:
    return "a long tool body worth compacting " * 20

  store = PayloadStore()
  persisted: list[str] = []
  original = store.persist_payload

  async def recording(session_id, payload_ref, content):
    persisted.append(payload_ref)
    await original(session_id, payload_ref, content)
  store.persist_payload = recording  # type: ignore[method-assign]

  runner = RuntimeRunner(RuntimeOptions(
    provider=Provider(),
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane().register(RegisteredTool(ping, KernelToolSchema(
      name="ping", description="ping", parameters='{"type":"object"}'))),
    max_tokens=4_000,
    max_turns=40,
    baseline_tool_ids=["ping"],
    payload_store=store,
  ))
  events = [event async for event in runner.run(goal="keep pinging", session_id="p0-6-runner")]
  assert not [e for e in events if isinstance(e, ErrorEvent)], [e.message for e in events if isinstance(e, ErrorEvent)]
  assert persisted, "the archive body was written to the payload store"
  assert all(ref.startswith("payload:") for ref in persisted)
