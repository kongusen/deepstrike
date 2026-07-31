"""Regression: nested group-vehicle spawn via ``spawn_sub_agent`` is unavailable under canonical ABI v3.

The nested-vehicle budget fix remains covered by orchestrator/unit tests; the public host spawn bypass
that this regression originally exercised is fail-closed. Mirrors Node posture for direct host spawn.
"""
from __future__ import annotations

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
)
from deepstrike.runtime.run_group import (
    GroupBudgetScope,
    GroupMember,
    InMemoryGroupBudgetStore,
    RunGroup,
)
from deepstrike.tools import tool
from deepstrike.types.agent import AgentCapabilityFilter, AgentIdentity, AgentRunSpec


def _noop() -> str:
    """Do nothing."""
    return "ok"


@pytest.mark.asyncio
async def test_nested_vehicle_spawn_sub_agent_unavailable_under_canonical_abi_v3():
    store = InMemoryGroupBudgetStore()
    group = RunGroup(id="nested", budget_store=store)

    parent_scope = await GroupBudgetScope.open(
        group,
        GroupMember("parent"),
        limits={"tokens": 100_000},
        requested={"tokens": 100_000},
    )
    assert parent_scope.granted.tokens == 100_000

    plane = LocalExecutionPlane()
    plane.register(tool(_noop))
    runner = RuntimeRunner(RuntimeOptions(
        provider=None,
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=4096,
        max_total_tokens=100_000,
        run_group=group,
        agent_id="parent",
    ))

    spec = AgentRunSpec(
        identity=AgentIdentity(agent_id="worker", session_id="worker-child", is_sub_agent=True),
        role="implement",
        isolation="shared",
        goal="do the work",
        capability_filter=AgentCapabilityFilter(allowed_ids=["_noop"]),
    )
    with pytest.raises(RuntimeError, match=r"canonical ABI v3"):
        async for _event in runner.spawn_sub_agent(spec):
            pass
