from deepstrike.runtime.workflow_replay import WorkflowReplay


def test_workflow_replay_projects_wrapped_entries_and_outcomes():
    replay = WorkflowReplay.from_entries([
        {"seq": 0, "event": {"kind": "workflow_node_completed", "agent_id": "a", "status": "completed"}},
        {"seq": 1, "event": {"kind": "workflow_completed", "node_outcomes": [{"node_id": "b", "status": "failed"}]}},
    ])
    assert replay.completed is True
    assert len(replay.events) == 2
    assert replay.node("a")["status"] == "completed"
    assert replay.node("b")["status"] == "failed"
