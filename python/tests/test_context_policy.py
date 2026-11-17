import pytest

from deepstrike.runtime.context_policy import (
    context_policy,
    normalize_context_policy,
    ratio_to_ppm,
)


def test_normalizes_context_ratios_to_cross_sdk_integer_ppm_wire():
    wire = normalize_context_policy(context_policy())
    assert wire == {
        "pressure_thresholds_ppm": {
            "micro": 700_000,
            "collapse": 900_000,
            "auto": 950_000,
            "renewal": 980_000,
        },
        "target_after_compress_ppm": 650_000,
        "preserve_recent_turns": 2,
        "renewal_carryover_ppm": 50_000,
    }
    assert ratio_to_ppm(0.1234565) == 123_457


def test_context_policy_is_atomically_validated():
    with pytest.raises(ValueError, match="micro < collapse"):
        context_policy({"pressure_thresholds": {"micro": 0.91}})
    with pytest.raises(ValueError, match="lower than the micro"):
        context_policy({"target_after_compress": 0.70})
    with pytest.raises(ValueError, match="between 0 and 1"):
        context_policy({"renewal_carryover": 1.1})
