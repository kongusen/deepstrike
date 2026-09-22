import deepstrike


def test_root_all_is_semantic_and_advanced_runtime_is_explicit():
    assert "Agent" in deepstrike.__all__
    assert "create_agent" in deepstrike.__all__
    assert "RuntimeRunner" not in deepstrike.__all__
    assert "KernelJournal" not in deepstrike.__all__
    assert "EvolutionRuntime" not in deepstrike.__all__
    assert not hasattr(deepstrike, "RuntimeRunner")
    assert not hasattr(deepstrike, "KernelJournal")
