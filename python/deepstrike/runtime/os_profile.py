from __future__ import annotations

from dataclasses import dataclass
from deepstrike.governance import GovernancePolicy, GovernancePolicyRule


@dataclass
class AttentionPolicy:
    max_queue_size: int | None = None


DEFAULT_NATIVE_ATTENTION_POLICY = AttentionPolicy(max_queue_size=64)

DEFAULT_NATIVE_GOVERNANCE_POLICY = GovernancePolicy(
    rules=[GovernancePolicyRule(pattern="*", action="allow")],
)
