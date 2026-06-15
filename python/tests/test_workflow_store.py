import pytest

from deepstrike import FileWorkflowStore, WorkflowNodeSpec, WorkflowSpec


def test_file_workflow_store_roundtrips_lists_and_rejects_unsafe(tmp_path):
    store = FileWorkflowStore(tmp_path)
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="explore", role="explore", isolation="read_only"),
        WorkflowNodeSpec(
            task="judge", role="plan", depends_on=[0],
            tournament={"entrants": ["x", "y"]}, token_budget=10000,
        ),
    ])
    path = store.save("my-flow", spec)
    assert "my-flow.json" in path
    assert store.list() == ["my-flow"]
    # Pure data ⇒ exact round-trip (dataclass equality across all fields).
    assert store.load("my-flow") == spec

    with pytest.raises(ValueError):
        store.save("../evil", spec)
    with pytest.raises(FileNotFoundError):
        store.load("does-not-exist")


def test_file_workflow_store_lists_empty_when_missing(tmp_path):
    store = FileWorkflowStore(tmp_path / "nope")
    assert store.list() == []
