import pytest

from deepstrike.runtime.context_policy import (
    context_policy_v1,
    normalize_context_policy_v1,
    ratio_to_ppm,
)
from deepstrike.runtime.kernel_transaction_log import kernel_record_digest


def test_normalizes_context_ratios_to_cross_sdk_integer_ppm_wire():
    wire = normalize_context_policy_v1(context_policy_v1())
    assert wire == {
        "version": 1,
        "pressure_thresholds_ppm": {
            "snip": 700_000,
            "micro": 800_000,
            "collapse": 900_000,
            "auto": 950_000,
            "renewal": 980_000,
        },
        "target_after_compress_ppm": 650_000,
        "preserve_recent_turns": 2,
        "renewal_carryover_ppm": 50_000,
        "collapse_old_assistant_narration": True,
        "idle_micro_compact_minutes": 60,
    }
    assert kernel_record_digest(wire) == "a8ea8875b056cb07c15b7832b5a90aa809041e91aeaf58462c402bce2312351b"
    assert ratio_to_ppm(0.1234565) == 123_457


def test_context_policy_is_atomically_validated():
    with pytest.raises(ValueError, match="snip < micro"):
        context_policy_v1({"pressure_thresholds": {"micro": 0.69}})
    with pytest.raises(ValueError, match="lower than the snip"):
        context_policy_v1({"target_after_compress": 0.70})
    with pytest.raises(ValueError, match="between 0 and 1"):
        context_policy_v1({"renewal_carryover": 1.1})
