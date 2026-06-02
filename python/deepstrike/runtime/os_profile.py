from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from deepstrike.governance import Governance, GovernancePolicy, GovernancePolicyRule

OsProfile = Literal["legacy", "native"]


@dataclass
class AttentionPolicy:
    max_queue_size: int | None = None


@dataclass
class NativeProfileRequirements:
    os_profile: OsProfile | None = None
    attention_policy: AttentionPolicy | dict | None = None
    governance_policy: GovernancePolicy | None = None
    governance: Governance | None = None


def is_native_profile(opts: NativeProfileRequirements | object) -> bool:
    profile = getattr(opts, "os_profile", None)
    return profile == "native"


def assert_native_profile(opts: NativeProfileRequirements | object) -> None:
    if not is_native_profile(opts):
        return
    if not getattr(opts, "attention_policy", None):
        raise ValueError(
            'os_profile "native" requires RuntimeOptions.attention_policy (in-kernel signal routing)',
        )
    if not getattr(opts, "governance_policy", None):
        raise ValueError(
            'os_profile "native" requires RuntimeOptions.governance_policy (in-kernel syscall gate)',
        )
    if getattr(opts, "governance", None):
        raise ValueError(
            'os_profile "native" forbids legacy RuntimeOptions.governance; use governance_policy only',
        )


DEFAULT_NATIVE_ATTENTION_POLICY = AttentionPolicy(max_queue_size=64)

DEFAULT_NATIVE_GOVERNANCE_POLICY = GovernancePolicy(
    rules=[GovernancePolicyRule(pattern="*", action="allow")],
)
