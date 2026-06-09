import json

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    SubAgentResult,
    LoopResult,
    WorkflowSpec,
    WorkflowNodeSpec,
    workflow_spec_to_kernel,
)
from deepstrike._kernel import KernelRuntime, LoopPolicy


class _StubProvider:
    async def stream(self, context, tools, extensions=None, state=None):  # pragma: no cover
        from deepstrike.providers.stream import TextDelta

        yield TextDelta(delta="x")


class _StubOrchestrator:
    """Records goals it was asked to run; reports each node as completed."""

    def __init__(self) -> None:
        self.goals: list[str] = []

    async def run(self, ctx) -> SubAgentResult:
        self.goals.append(ctx.spec.goal)
        return SubAgentResult(
            agent_id=ctx.spec.identity.agent_id,
            result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1),
        )


def test_workflow_spec_to_kernel_shape():
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore", isolation="read_only", context_inheritance="system_only"),
        WorkflowNodeSpec(task={"goal": "synth", "criteria": ["merge"]}, role="plan", depends_on=[0]),
    ])
    k = workflow_spec_to_kernel(spec)
    assert k["nodes"][0] == {
        "task": {"goal": "w0", "criteria": []},
        "role": "explore",
        "isolation": "read_only",
        "context_inheritance": "system_only",
    }
    assert k["nodes"][1]["task"] == {"goal": "synth", "criteria": ["merge"]}
    assert k["nodes"][1]["depends_on"] == [0]


@pytest.mark.asyncio
async def test_run_workflow_drives_fanout_to_completion():
    orch = _StubOrchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))

    # Establish an active parent run on a kernel (run_workflow runs mid-run).
    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    rt.step(json.dumps({"version": 1, "event": {"kind": "start_run", "task": {"goal": "parent", "criteria": []}}}))
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0, 1]),
    ])
    outcome = await runner.run_workflow(spec)

    assert sorted(outcome["completed"]) == ["wf-node0", "wf-node1", "wf-node2"]
    assert outcome["failed"] == []
    # Workers ran first (parallel), then synth — all goals were dispatched.
    assert sorted(orch.goals) == ["synth", "w0", "w1"]
    assert orch.goals[-1] == "synth"  # synth only after both workers
