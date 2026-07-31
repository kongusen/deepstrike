"""M5 v2 / G1: agent-authored workflow bootstrap.

Canonical host cutover (Task 20) fails closed on the agent-reachable ``submit_workflow`` bootstrap
path — same posture as Node Task 19. Host-driven ``run_workflow`` / ``load_workflow`` remains the
supported production entry.
"""

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    WorkflowNodeSpec,
    WorkflowSpec,
    submit_workflow_to_kernel,
)


def test_submit_workflow_to_kernel_lowers_spec_with_parent_session():
    ev = submit_workflow_to_kernel(WorkflowSpec(nodes=[WorkflowNodeSpec(task="x", role="implement")]), "sess-1")
    assert ev["kind"] == "submit_workflow"
    assert ev["parent_session_id"] == "sess-1"
    assert len(ev["spec"]["nodes"]) == 1
    assert "submitter_agent_id" not in ev
    assert submit_workflow_to_kernel(WorkflowSpec(nodes=[]), "s", "wf-node3")["submitter_agent_id"] == "wf-node3"


@pytest.mark.asyncio
async def test_bootstrap_workflow_unsupported_by_canonical_host():
    runner = RuntimeRunner(RuntimeOptions(
        provider=None,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=1000,
    ))
    with pytest.raises(RuntimeError, match="unsupported by the canonical host"):
        await runner.bootstrap_workflow(WorkflowSpec(nodes=[
            WorkflowNodeSpec(task="explore A", role="implement"),
            WorkflowNodeSpec(task="explore B", role="implement"),
        ]))


@pytest.mark.asyncio
async def test_bootstrap_workflow_quota_path_also_unsupported():
    runner = RuntimeRunner(RuntimeOptions(
        provider=None,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        resource_quota={"max_workflow_nodes": 2},
        max_tokens=1000,
    ))
    with pytest.raises(RuntimeError, match="unsupported by the canonical host"):
        await runner.bootstrap_workflow(WorkflowSpec(nodes=[
            WorkflowNodeSpec(task="a", role="implement"),
            WorkflowNodeSpec(task="b", role="implement"),
            WorkflowNodeSpec(task="c", role="implement"),
        ]))
