import json

from deepstrike import start_workflow_tool, submit_workflow_nodes_tool


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
