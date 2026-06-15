"""#2-B-ii (python, asyncio idiom): a Critical InterruptNow during a running workflow node preempts it
mid-flight. The drive loop's concurrent monitor polls the signal source during the batch → routes it to
the kernel (root in SubAgentAwait → preempt → AgentPreempted + workflow torn down) → cancels the matching
node's asyncio task → CancelledError aborts its in-flight LLM call. Real native kernel.
"""

import asyncio
import json
from types import SimpleNamespace

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    LoopResult,
    Message,
    RuntimeOptions,
    RuntimeRunner,
    SubAgentResult,
)
from deepstrike._kernel import KernelRuntime, LoopPolicy


class _Stub:
    def __init__(self) -> None:
        self.cancelled = False

    async def run(self, ctx):
        # The node "runs" long; a preempt cancels its task → CancelledError here.
        try:
            await asyncio.sleep(5)
        except asyncio.CancelledError:
            self.cancelled = True
            raise
        return SubAgentResult(
            agent_id=ctx.spec.identity.agent_id,
            result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1,
                              final_message=Message(role="assistant", content="ok")),
        )


class _SigSource:
    def __init__(self, sigs):
        self._sigs = list(sigs)

    async def next_signal(self):
        return self._sigs.pop(0) if self._sigs else None


@pytest.mark.asyncio
async def test_critical_signal_preempts_running_workflow_node():
    orch = _Stub()
    crit = SimpleNamespace(source="gateway", signal_type="alert", urgency="critical",
                           payload={"goal": "STOP NOW"}, kind="alert", dedupe_key=None)
    runner = RuntimeRunner(RuntimeOptions(
        provider=None,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        signal_source=_SigSource([crit]),
        max_tokens=1000,
    ))
    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    rt.step(json.dumps({"version": 1, "event": {"kind": "start_run", "task": {"goal": "parent", "criteria": []}}}))
    runner._active_kernel = rt
    runner._current_session_id = "sess"
    runner._pending_observations = []

    from deepstrike import WorkflowSpec, WorkflowNodeSpec
    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="a long-running node", role="implement")])
    outcome = await runner.run_workflow(spec)

    # The running node's task was cancelled mid-flight and the workflow torn down.
    assert orch.cancelled is True
    assert "wf-node0" in outcome["failed"]
    assert any(o.get("kind") == "agent_preempted" for o in runner._pending_observations)
