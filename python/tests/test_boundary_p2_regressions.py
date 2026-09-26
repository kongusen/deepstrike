"""Python SDK → canonical kernel boundary regressions (P2 audit, 0.2.74).

Mirrors ``node/tests/boundary-p2-regressions.test.ts`` against the real native kernel.

P2-1: the runner told a malformed input apart by searching error text for "invalidarg".
P2-2: a provider result / tool batch advanced the host's view before the kernel committed it.
P2-3: two transitions of one operation could overlap on the journal's single outbound slot; the
      file journal listed the whole record directory on every append; and the observations of an
      envelope drained on restore were thrown away.
"""

import asyncio
import json
import time

import pytest

from deepstrike import GovernancePolicy, governance_policy_patch
from deepstrike.kernel.canonical import CanonicalKernel
from deepstrike.runtime.canonical_kernel_step import (
  CanonicalKernelRejectedError, CanonicalRunnerRuntime, is_invalid_input_error,
)
from deepstrike.runtime.kernel_journal import (
  FileKernelJournal, InMemoryKernelJournal, JournalRecordInput,
)


def _runtime(journal=None, operation_id="op-p2"):
  return CanonicalRunnerRuntime(
    CanonicalKernel(), journal or InMemoryKernelJournal(), operation_id, max_context_tokens=100_000,
  )


def test_p2_1_a_run_failure_is_classified_by_fault_code_not_text():
  assert is_invalid_input_error(CanonicalKernelRejectedError("malformed_envelope", "x")) is True
  assert is_invalid_input_error(CanonicalKernelRejectedError("invalid_lifecycle", "x")) is False
  assert is_invalid_input_error(RuntimeError("tool said: invalid argument supplied")) is False


@pytest.mark.asyncio
async def test_p2_2_a_refused_provider_result_leaves_the_host_where_the_kernel_is():
  rt = _runtime()
  first = await rt.start_agent({"goal": "answer"})
  assert first is not None and first.kind == "call_provider"
  rt.drain_new_messages()
  turns = rt.turn()
  with pytest.raises(Exception):
    await rt.apply_host_event({
      "kind": "provider_result", "effect_id": "step:999:effect:0",
      "message": {"role": "assistant", "content": "stale answer", "tool_calls": []},
    })
  assert rt.turn() == turns
  assert rt.drain_new_messages() == []


@pytest.mark.asyncio
async def test_p2_3_two_transitions_of_one_operation_never_share_the_outbound_slot():
  journal = InMemoryKernelJournal()
  rt = _runtime(journal, "op-serial")
  await rt.start_agent({"goal": "answer"})
  events: list[str] = []
  stage, clear = journal.stage_outbound_envelope, journal.clear_outbound_envelope

  async def staged(operation_id, envelope):
    events.append("stage")
    await asyncio.sleep(0.005)
    await stage(operation_id, envelope)

  async def cleared(operation_id):
    events.append("clear")
    await clear(operation_id)

  journal.stage_outbound_envelope = staged
  journal.clear_outbound_envelope = cleared
  update = lambda progress: rt.host.transition(  # noqa: E731
    {"kind": "host_control", "command": {"kind": "update_task", "update": {"progress": progress}}})
  await asyncio.gather(update("a"), update("b"))
  assert events == ["stage", "clear", "stage", "clear"]


@pytest.mark.asyncio
async def test_p2_3_the_file_journal_finds_its_head_without_listing_every_append(tmp_path):
  journal = FileKernelJournal(tmp_path)
  listings = 0
  original = journal._record_seqs

  def counted(operation_id):
    nonlocal listings
    listings += 1
    return original(operation_id)

  journal._record_seqs = counted
  head = None
  for step in range(40):
    receipt = await journal.compare_and_append("op", head, JournalRecordInput(
      step_seq=step, record_digest=f"sha256:{step:064d}", record_bytes=bytes([step]),
    ))
    head = receipt.record_digest
  assert listings <= 1
  assert (await journal.head("op")).step_seq == 39
  other = FileKernelJournal(tmp_path)
  await other.compare_and_append("op", head, JournalRecordInput(
    step_seq=40, record_digest="sha256:" + "a" * 64, record_bytes=b"\x01",
  ))
  assert (await journal.head("op")).step_seq == 40, "another writer's append is still seen"


@pytest.mark.asyncio
async def test_p2_3_a_restore_keeps_the_observations_of_the_envelope_it_drains():
  journal = InMemoryKernelJournal()
  rt = _runtime(journal, "op-drain")
  await rt.start_agent({"goal": "answer"})
  await journal.stage_outbound_envelope("op-drain", json.dumps({
    "operation_id": "op-drain",
    "input_id": "crash-window",
    "observed_at_ms": str(int(time.time() * 1000) + 1_000),
    "input": {"kind": "host_control", "command": {
      "kind": "apply_policy_patch", "expected_revision": "0",
      "patch": governance_policy_patch(GovernancePolicy(vetoes=["x"])),
    }},
  }))
  woken = _runtime(journal, "op-drain")
  await woken.restore()
  assert "live_policy_changed" in [o.get("kind") for o in woken.drain_host_observations()]


@pytest.mark.asyncio
async def test_p2_4_a_dependent_of_the_fast_node_starts_while_its_slow_sibling_works():
  """Behind a round barrier the dependent could not start until the slow node finished — and the
  slow node here waits for exactly that dependent, so the barrier times it out."""
  from deepstrike import (
    InMemorySessionLog, LoopResult, ModelMessage, RuntimeOptions, RuntimeRunner, SubAgentResult,
  )
  from deepstrike.types.agent import WorkflowNodeSpec, WorkflowSpec

  dependent_started = asyncio.Event()

  class Orchestrator:
    async def run(self, ctx):
      goal = ctx.spec.goal
      if "slow" in goal:
        await asyncio.wait_for(dependent_started.wait(), timeout=2)
      if "after fast" in goal:
        dependent_started.set()
      agent_id = ctx.spec.identity.agent_id
      return SubAgentResult(agent_id=agent_id, result=LoopResult(
        termination="completed", turns_used=1, total_tokens_used=1,
        final_message=ModelMessage(role="assistant", content=agent_id),
      ))

  class Provider:
    async def stream(self, context, tools, extensions=None, state=None):
      if False:
        yield None

  runner = RuntimeRunner(RuntimeOptions(
    provider=Provider(), session_log=InMemorySessionLog(), max_tokens=8000,
    sub_agent_orchestrator=Orchestrator(),
  ))
  outcome = await runner.run_workflow(WorkflowSpec(nodes=[
    WorkflowNodeSpec(task="slow worker", role="explore"),
    WorkflowNodeSpec(task="fast worker", role="explore"),
    WorkflowNodeSpec(task="after fast", role="plan", depends_on=[1]),
  ]), session_id="p2-4")
  assert [node.status for node in outcome.node_outcomes] == ["completed", "completed", "completed"]
