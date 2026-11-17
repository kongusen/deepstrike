from __future__ import annotations

import math
from copy import deepcopy
from typing import Any, TypedDict


PPM_SCALE = 1_000_000


class ContextPressureThresholds(TypedDict):
    micro: float
    collapse: float
    auto: float
    renewal: float


class DelegatedContextTiers(TypedDict, total=False):
    tool_result_clearing: bool
    compaction: bool


class ContextPolicy(TypedDict, total=False):
    pressure_thresholds: ContextPressureThresholds
    target_after_compress: float
    preserve_recent_turns: int
    renewal_carryover: float
    delegated_tiers: DelegatedContextTiers
    #: Plan task 9.1 · tokens the model's notes may hold in the state turn (kernel default 600, on
    #: since 0.2.82); 0 turns the ``note`` tool off.
    notes_budget_tokens: int


class ContextPolicyWire(TypedDict, total=False):
    pressure_thresholds_ppm: dict[str, int]
    target_after_compress_ppm: int
    preserve_recent_turns: int
    renewal_carryover_ppm: int
    delegated_tiers: DelegatedContextTiers
    notes_budget_tokens: int


DEFAULT_CONTEXT_POLICY: ContextPolicy = {
    "pressure_thresholds": {
        "micro": 0.70,
        "collapse": 0.90,
        "auto": 0.95,
        "renewal": 0.98,
    },
    "target_after_compress": 0.65,
    "preserve_recent_turns": 2,
    "renewal_carryover": 0.05,
}


def context_policy(overrides: dict[str, Any] | None = None) -> ContextPolicy:
    """Resolve ergonomic partial SDK options into a complete, validated policy."""
    policy = deepcopy(DEFAULT_CONTEXT_POLICY)
    overrides = overrides or {}
    thresholds = overrides.get("pressure_thresholds")
    policy.update({key: value for key, value in overrides.items() if key != "pressure_thresholds"})  # type: ignore[typeddict-item]
    if thresholds is not None:
        policy["pressure_thresholds"].update(thresholds)
    normalize_context_policy(policy)
    return policy


def normalize_context_policy(policy: ContextPolicy) -> ContextPolicyWire:
    thresholds = policy["pressure_thresholds"]
    pressure_ppm = {
        name: ratio_to_ppm(thresholds[name], f"pressure_thresholds.{name}")
        for name in ("micro", "collapse", "auto", "renewal")
    }
    ordered = list(pressure_ppm.values())
    if any(left >= right for left, right in zip(ordered, ordered[1:])):
        raise ValueError(
            "context pressure thresholds must satisfy micro < collapse < auto < renewal"
        )
    target_ppm = ratio_to_ppm(policy["target_after_compress"], "target_after_compress")
    if target_ppm >= pressure_ppm["micro"]:
        raise ValueError("target_after_compress must be lower than the micro threshold")
    _integer_at_least(policy["preserve_recent_turns"], 1, "preserve_recent_turns")

    wire: ContextPolicyWire = {
        "pressure_thresholds_ppm": pressure_ppm,
        "target_after_compress_ppm": target_ppm,
        "preserve_recent_turns": policy["preserve_recent_turns"],
        "renewal_carryover_ppm": ratio_to_ppm(policy["renewal_carryover"], "renewal_carryover"),
    }
    if "notes_budget_tokens" in policy:
        _integer_at_least(policy["notes_budget_tokens"], 0, "notes_budget_tokens")
        wire["notes_budget_tokens"] = policy["notes_budget_tokens"]
    delegated = policy.get("delegated_tiers")
    if delegated and any(delegated.values()):
        wire["delegated_tiers"] = {
            key: bool(value) for key, value in delegated.items() if value
        }
    return wire


def ratio_to_ppm(value: float, field: str = "ratio") -> int:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0 or value > 1:
        raise ValueError(f"{field} must be a finite number between 0 and 1")
    return math.floor(value * PPM_SCALE + 0.5)


def _integer_at_least(value: int, minimum: int, field: str) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ValueError(f"{field} must be an integer >= {minimum}")
