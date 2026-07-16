"""M5 v2 / G1: an agent authors a top-level workflow.

``bootstrap_workflow`` routes a host ``WorkflowSpec`` through the agent-reachable
``Syscall::LoadWorkflow`` (the ``submit_workflow`` kernel event): with no workflow active the kernel
BOOTSTRAPS the DAG in this same kernel (unified governance — one kernel, one quota), then the shared
driver runs it to completion. Exercises the real native ABI end-to-end via KernelRuntime.
"""

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    LoopResult,
    RuntimeOptions,
    RuntimeRunner,
    SubAgentResult,
    WorkflowNodeSpec,
    WorkflowSpec,
    submit_workflow_to_kernel,
)
from deepstrike._kernel import KernelRuntime, LoopPolicy, Message
from deepstrike.runtime.kernel_step import kernel_action, kernel_apply


def test_submit_workflow_to_kernel_lowers_spec_with_parent_session():
    ev = submit_workflow_to_kernel(WorkflowSpec(nodes=[WorkflowNodeSpec(task="x", role="implement")]), "sess-1")
    assert ev["kind"] == "submit_workflow"
    assert ev["parent_session_id"] == "sess-1"
    assert len(ev["spec"]["nodes"]) == 1
    # submitter id only when a quarantined author needs trust coercion (flatten case).
    assert "submitter_agent_id" not in ev
    assert submit_workflow_to_kernel(WorkflowSpec(nodes=[]), "s", "wf-node3")["submitter_agent_id"] == "wf-node3"


class _Stub:
    def __init__(self) -> None:
        self.ran: list[str] = []

    async def run(self, ctx):
        self.ran.append(ctx.spec.identity.agent_id)
        return SubAgentResult(
            agent_id=ctx.spec.identity.agent_id,
            result=LoopResult(
                termination="completed",
                turns_used=1,
                total_tokens_used=1,
                final_message=Message(role="assistant", content="ok"),
            ),
        )


def _runner(orch, *, quota: dict | None = None):
    r = RuntimeRunner(RuntimeOptions(
        provider=None,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))
    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    kernel_action(rt, [], {"kind": "start_run", "task": {"goal": "parent", "criteria": []}})
    if quota is not None:
        kernel_apply(rt, [], {"kind": "set_resource_quota", "quota": quota})
    r._active_kernel = rt
    r._current_session_id = "sess"
    return r


@pytest.mark.asyncio
async def test_bootstrap_workflow_bootstraps_and_runs_authored_nodes():
    orch = _Stub()
    runner = _runner(orch)
    # No load_workflow first — the agent itself authors the spec; the kernel bootstraps it.
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="explore A", role="implement"),
        WorkflowNodeSpec(task="explore B", role="implement"),
    ])
    outcome = await runner.bootstrap_workflow(spec)
    assert sorted(orch.ran) == ["wf-node0", "wf-node1"]
    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1"]
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == []


@pytest.mark.asyncio
async def test_bootstrap_workflow_denied_past_workflow_node_quota():
    orch = _Stub()
    runner = _runner(orch, quota={"max_workflow_nodes": 2})
    # 3 nodes > max(2) → the kernel denies the bootstrap; nothing runs.
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="a", role="implement"),
        WorkflowNodeSpec(task="b", role="implement"),
        WorkflowNodeSpec(task="c", role="implement"),
    ])
    outcome = await runner.bootstrap_workflow(spec)
    assert orch.ran == []
    assert [n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")] == []
    assert outcome.rejection is not None
    assert outcome.rejection.operation == "start_workflow"
    assert "would grow workflow" in outcome.rejection.reason
