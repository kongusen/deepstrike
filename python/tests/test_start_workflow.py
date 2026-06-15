import json

from deepstrike import start_workflow_tool, submit_workflow_nodes_tool
from deepstrike.runtime.runner import _parse_start_workflow_args


def test_start_workflow_tool_shares_submit_node_schema():
    assert start_workflow_tool["name"] == "start_workflow"
    p = json.loads(start_workflow_tool["parameters"])
    assert p["required"] == ["spec"]
    items = p["properties"]["spec"]["properties"]["nodes"]["items"]
    for key in ("task", "role", "loop", "classify", "tournament", "reducer", "token_budget", "depends_on"):
        assert key in items["properties"]
    assert items["required"] == ["task", "role"]
    # Same node-item schema as submit_workflow_nodes — they must never drift.
    submit_items = json.loads(submit_workflow_nodes_tool["parameters"])["properties"]["nodes"]["items"]
    assert items == submit_items


def test_parse_start_workflow_args_flattens_spec_nodes():
    args = json.dumps({"spec": {"nodes": [
        {"task": "a", "role": "explore"},
        {"task": "pick", "role": "plan", "tournament": {"entrants": ["x", "y"]}, "depends_on": [0]},
    ]}})
    nodes = _parse_start_workflow_args(args)
    assert len(nodes) == 2
    assert nodes[0].task == "a" and nodes[0].role == "explore"
    assert nodes[1].tournament == {"entrants": ["x", "y"]}
    assert nodes[1].depends_on == [0]
    # malformed / missing spec → no nodes
    assert _parse_start_workflow_args("{}") == []
    assert _parse_start_workflow_args("not json") == []
