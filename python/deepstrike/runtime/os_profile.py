from __future__ import annotations

from dataclasses import dataclass
from typing import Any
from deepstrike.governance import GovernancePolicy, GovernancePolicyRule


@dataclass
class SignalPolicy:
  queue_max: int
  ttl_ms: int | None = None
  deadline_escalation: bool | None = None


@dataclass
class OsProfile:
  id: str
  signal_policy: SignalPolicy | dict
  governance_policy: GovernancePolicy


DEFAULT_NATIVE_SIGNAL_POLICY = SignalPolicy(queue_max=64)

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


def os_profile(profile: str | OsProfile = "native") -> OsProfile:
  """Resolve a named OS profile into concrete kernel-owned policy defaults."""
  if isinstance(profile, OsProfile):
    return profile
  if profile != "native":
    raise ValueError(f"Unsupported OS profile: {profile}")
  return OsProfile(
    id="native",
    signal_policy=DEFAULT_NATIVE_SIGNAL_POLICY,
    governance_policy=DEFAULT_NATIVE_GOVERNANCE_POLICY,
  )


def assert_native_profile(profile: str | OsProfile = "native") -> OsProfile:
  """Assert that a runtime is using a valid native microkernel policy profile."""
  resolved = os_profile(profile)
  if resolved.id != "native":
    raise ValueError(f"Unsupported OS profile: {resolved.id}")
  validation = validate_declarative_policy(
    resolved.governance_policy,
    resolved.signal_policy,
  )
  if not validation["valid"]:
    raise ValueError(f"Invalid native OS profile: {'; '.join(validation['errors'])}")
  return resolved


def validate_declarative_policy(
  gov_policy: GovernancePolicy | None = None,
  signal_policy: SignalPolicy | dict | None = None,
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

  if signal_policy is not None:
    queue_max = (
      signal_policy.get("queue_max")
      if isinstance(signal_policy, dict)
      else getattr(signal_policy, "queue_max", None)
    )
    ttl_ms = (
      signal_policy.get("ttl_ms")
      if isinstance(signal_policy, dict)
      else getattr(signal_policy, "ttl_ms", None)
    )
    deadline_escalation = (
      signal_policy.get("deadline_escalation")
      if isinstance(signal_policy, dict)
      else getattr(signal_policy, "deadline_escalation", None)
    )
    if type(queue_max) is not int or queue_max <= 0:
      errors.append("SignalPolicy queue_max must be a positive integer")
    if ttl_ms is not None and (type(ttl_ms) is not int or ttl_ms <= 0):
      errors.append("SignalPolicy ttl_ms must be a positive integer")
    if deadline_escalation is not None and type(deadline_escalation) is not bool:
      errors.append("SignalPolicy deadline_escalation must be a boolean")

  return {
    "valid": len(errors) == 0,
    "errors": errors,
  }
