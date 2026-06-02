from __future__ import annotations

from dataclasses import dataclass
from typing import Any
from deepstrike.governance import GovernancePolicy, GovernancePolicyRule


@dataclass
class AttentionPolicy:
  max_queue_size: int | None = None


DEFAULT_NATIVE_ATTENTION_POLICY = AttentionPolicy(max_queue_size=64)

DEFAULT_NATIVE_GOVERNANCE_POLICY = GovernancePolicy(
  rules=[GovernancePolicyRule(pattern="*", action="allow")],
)

DEFAULT_SANDBOX_POLICY = GovernancePolicy(
  rules=[
    GovernancePolicyRule(pattern="read_file", action="allow"),
    GovernancePolicyRule(pattern="write_file", action="ask_user"),
    GovernancePolicyRule(pattern="run_command", action="ask_user"),
    GovernancePolicyRule(pattern="*", action="deny"),
  ],
)


def validate_declarative_policy(
  gov_policy: GovernancePolicy | None = None,
  attention_policy: AttentionPolicy | dict | None = None,
) -> dict[str, Any]:
  errors = []

  if gov_policy is not None:
    rules = getattr(gov_policy, "rules", None)
    if not isinstance(rules, list):
      errors.append("GovernancePolicy rules must be a list")
    else:
      for idx, rule in enumerate(rules):
        pattern = getattr(rule, "pattern", None)
        action = getattr(rule, "action", None)
        if not pattern or not isinstance(pattern, str):
          errors.append(f"Rule[{idx}] pattern is missing or not a string")
        if action not in ("allow", "deny", "ask_user"):
          errors.append(f"Rule[{idx}] action '{action}' is invalid. Allowed: allow, deny, ask_user")

  if attention_policy is not None:
    max_q = (
      attention_policy.get("max_queue_size")
      if isinstance(attention_policy, dict)
      else getattr(attention_policy, "max_queue_size", None)
    )
    if max_q is not None:
      if not isinstance(max_q, int) or max_q <= 0:
        errors.append("AttentionPolicy max_queue_size must be a positive integer")

  return {
    "valid": len(errors) == 0,
    "errors": errors,
  }
