from deepstrike import InMemorySessionLog
from deepstrike.runtime.runner import RuntimeOptions
from deepstrike.runtime.sub_agent_orchestrator import _resolve_provider
from deepstrike.types.agent import (
    WorkflowNodeSpec,
    WorkflowSpawnInfo,
    workflow_node_spec_to_kernel,
    workflow_node_to_spec,
)


def _node(**over) -> WorkflowSpawnInfo:
    base = dict(agent_id="wf-node0", goal="g", role="plan", isolation="shared", context_inheritance="none")
    base.update(over)
    return WorkflowSpawnInfo(**base)


def test_workflow_node_to_spec_carries_model_hint():
    assert workflow_node_to_spec(_node(model_hint="opus"), "sess").model_hint == "opus"
    assert workflow_node_to_spec(_node(), "sess").model_hint is None


def test_token_budget_maps_and_carries():
    # M4/G5: tokenBudget lowers to kernel token_budget and carries to the child spec.
    assert workflow_node_spec_to_kernel(WorkflowNodeSpec(task="x", role="plan", token_budget=10000))["token_budget"] == 10000
    assert "token_budget" not in workflow_node_spec_to_kernel(WorkflowNodeSpec(task="x", role="plan"))
    assert workflow_node_to_spec(_node(token_budget=10000), "sess").token_budget == 10000
    assert workflow_node_to_spec(_node(), "sess").token_budget is None


def test_dependency_policy_maps_with_strict_default():
    assert workflow_node_spec_to_kernel(
        WorkflowNodeSpec(task="x", role="plan", dep_policy="accept_partial")
    )["dep_policy"] == "accept_partial"
    assert workflow_node_spec_to_kernel(
        WorkflowNodeSpec(task="x", role="plan")
    )["dep_policy"] == "all_success"


def test_resolve_provider_routes_and_falls_back():
    base = object()
    opus = object()
    seen: list[str] = []

    def hook(h):
        seen.append(h)
        return opus if h == "opus" else None

    opts = RuntimeOptions(provider=base, session_log=InMemorySessionLog(), provider_for=hook)
    assert _resolve_provider(opts, "opus") is opus  # hook resolves → routed
    assert seen == ["opus"]  # hook called with the hint
    assert _resolve_provider(opts, "unknown") is base  # hook returns None → fallback
    assert _resolve_provider(opts, None) is base  # no hint → fallback

    opts_no_hook = RuntimeOptions(provider=base, session_log=InMemorySessionLog())
    assert _resolve_provider(opts_no_hook, "opus") is base  # no hook → fallback
