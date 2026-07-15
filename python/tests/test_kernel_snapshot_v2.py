from deepstrike.kernel import KernelRuntime, LoopPolicy
from deepstrike.runtime.kernel_step import (
    kernel_action,
    restore_kernel_runtime,
    snapshot_kernel_runtime,
)


def test_snapshot_v2_restores_wire_and_effect_identity():
    original = KernelRuntime(LoopPolicy(max_tokens=4096))
    call = kernel_action(original, [], {
        "kind": "start_run",
        "task": {"goal": "checkpoint", "criteria": []},
    })
    assert call.kind == "call_provider"

    snapshot = snapshot_kernel_runtime(original)
    assert snapshot["initial_policy"]["max_total_tokens"] == "1000000"
    restored = KernelRuntime(LoopPolicy(max_tokens=1))
    restore_kernel_runtime(restored, snapshot)
    assert snapshot_kernel_runtime(restored) == snapshot

    event = {
        "kind": "provider_result",
        "effect_id": call.effect_id,
        "message": {"role": "assistant", "content": "done", "tool_calls": []},
        "observed_input_tokens": 4,
        "observed_output_tokens": 1,
        "now_ms": 10,
    }
    restored_done = kernel_action(restored, [], event)
    original_done = kernel_action(original, [], event)
    assert restored_done.kind == original_done.kind == "done"
    assert restored_done.effect_id == original_done.effect_id
    assert restored_done.result.termination == original_done.result.termination
