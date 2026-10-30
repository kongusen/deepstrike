from dataclasses import asdict
from itertools import product

import pytest

from deepstrike import MemoryRuntimePolicy, resolve_memory_runtime_policy
from deepstrike.runtime.canonical_kernel_step import CanonicalRunnerRuntime
from deepstrike.runtime.kernel_journal import InMemoryKernelJournal

OFF = MemoryRuntimePolicy("disabled", "disabled")


def test_no_policy_preserves_legacy_behavior():
    assert resolve_memory_runtime_policy(None) is None


@pytest.mark.parametrize("retrieval,writes", list(product(("disabled", "host"), ("disabled", "candidate_sink"))))
def test_switches_project_independently(retrieval, writes):
    policy = MemoryRuntimePolicy(retrieval, writes)
    gates = resolve_memory_runtime_policy(policy)
    assert gates == resolve_memory_runtime_policy(asdict(policy))
    assert gates.retrieval_enabled == (retrieval == "host")
    assert gates.model_writes_enabled == (writes != "disabled")
    assert gates.model_writes_to_candidate_sink == (writes == "candidate_sink")


@pytest.mark.parametrize("field,value", [("retrieval", "candidate_sink"), ("model_writes", "host"), ("storage", "disk")])
def test_invalid_policy_rejected(field, value):
    with pytest.raises(ValueError, match="memory runtime policy"):
        resolve_memory_runtime_policy({**asdict(OFF), field: value})


def test_unknown_removed_policy_fields_are_rejected():
    with pytest.raises(ValueError, match="unknown memory runtime policy field"):
        resolve_memory_runtime_policy({"retrieval": "host", "model_writes": "disabled", "session_extraction": "disabled"})


def test_partial_policy_rejected():
    with pytest.raises(ValueError, match="model_writes"):
        resolve_memory_runtime_policy({"retrieval": "host"})


@pytest.mark.asyncio
@pytest.mark.parametrize("read,write", list(product((False, True), repeat=2)))
async def test_canonical_binding_keeps_independent_capabilities(read, write):
    runtime = CanonicalRunnerRuntime(object(), InMemoryKernelJournal(), "op-policy", max_context_tokens=10000)
    await runtime.apply_host_event({"kind": "set_memory_enabled", "enabled": True, "read": read, "write": write})
    assert runtime._config["memory_access"]["capabilities"] == {"read": read, "write": write}


@pytest.mark.asyncio
async def test_canonical_binding_legacy_defaults():
    runtime = CanonicalRunnerRuntime(object(), InMemoryKernelJournal(), "op-legacy", max_context_tokens=10000)
    await runtime.apply_host_event({"kind": "set_memory_enabled", "enabled": True})
    assert runtime._config["memory_access"]["capabilities"] == {"read": True, "write": True}
