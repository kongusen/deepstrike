from __future__ import annotations

import math
from copy import deepcopy
from typing import Any, TypedDict


CONTEXT_POLICY_VERSION = 1
PPM_SCALE = 1_000_000


class ContextPressureThresholdsV1(TypedDict):
    snip: float
    micro: float
    collapse: float
    auto: float
    renewal: float


class ContextPolicyV1(TypedDict):
    pressure_thresholds: ContextPressureThresholdsV1
    target_after_compress: float
    preserve_recent_turns: int
    renewal_carryover: float
    collapse_old_assistant_narration: bool
    idle_micro_compact_minutes: int


class ContextPolicyWireV1(TypedDict):
    version: int
    pressure_thresholds_ppm: dict[str, int]
    target_after_compress_ppm: int
    preserve_recent_turns: int
    renewal_carryover_ppm: int
    collapse_old_assistant_narration: bool
    idle_micro_compact_minutes: int


DEFAULT_CONTEXT_POLICY_V1: ContextPolicyV1 = {
    "pressure_thresholds": {
        "snip": 0.70,
        "micro": 0.80,
        "collapse": 0.90,
        "auto": 0.95,
        "renewal": 0.98,
    },
    "target_after_compress": 0.65,
    "preserve_recent_turns": 2,
    "renewal_carryover": 0.05,
    "collapse_old_assistant_narration": True,
    "idle_micro_compact_minutes": 60,
}


def context_policy_v1(overrides: dict[str, Any] | None = None) -> ContextPolicyV1:
    """Resolve ergonomic partial SDK options into a complete, validated policy."""
    policy = deepcopy(DEFAULT_CONTEXT_POLICY_V1)
    overrides = overrides or {}
    thresholds = overrides.get("pressure_thresholds")
    policy.update({key: value for key, value in overrides.items() if key != "pressure_thresholds"})  # type: ignore[typeddict-item]
    if thresholds is not None:
        policy["pressure_thresholds"].update(thresholds)
    normalize_context_policy_v1(policy)
    return policy


def normalize_context_policy_v1(policy: ContextPolicyV1) -> ContextPolicyWireV1:
    thresholds = policy["pressure_thresholds"]
    pressure_ppm = {
        name: ratio_to_ppm(thresholds[name], f"pressure_thresholds.{name}")
        for name in ("snip", "micro", "collapse", "auto", "renewal")
    }
    ordered = list(pressure_ppm.values())
    if any(left >= right for left, right in zip(ordered, ordered[1:])):
        raise ValueError(
            "context pressure thresholds must satisfy snip < micro < collapse < auto < renewal"
        )
    target_ppm = ratio_to_ppm(policy["target_after_compress"], "target_after_compress")
    if target_ppm >= pressure_ppm["snip"]:
        raise ValueError("target_after_compress must be lower than the snip threshold")
    _integer_at_least(policy["preserve_recent_turns"], 1, "preserve_recent_turns")
    _integer_at_least(policy["idle_micro_compact_minutes"], 0, "idle_micro_compact_minutes")
    if not isinstance(policy["collapse_old_assistant_narration"], bool):
        raise TypeError("collapse_old_assistant_narration must be boolean")

    return {
        "version": CONTEXT_POLICY_VERSION,
        "pressure_thresholds_ppm": pressure_ppm,
        "target_after_compress_ppm": target_ppm,
        "preserve_recent_turns": policy["preserve_recent_turns"],
        "renewal_carryover_ppm": ratio_to_ppm(policy["renewal_carryover"], "renewal_carryover"),
        "collapse_old_assistant_narration": policy["collapse_old_assistant_narration"],
        "idle_micro_compact_minutes": policy["idle_micro_compact_minutes"],
    }


def ratio_to_ppm(value: float, field: str = "ratio") -> int:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0 or value > 1:
        raise ValueError(f"{field} must be a finite number between 0 and 1")
    return math.floor(value * PPM_SCALE + 0.5)


def _integer_at_least(value: int, minimum: int, field: str) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ValueError(f"{field} must be an integer >= {minimum}")
