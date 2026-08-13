from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal

GovernancePolicyAction = Literal["allow", "deny", "ask_user"]


@dataclass
class GovernancePolicyRule:
    pattern: str
    action: GovernancePolicyAction


@dataclass
class GovernanceRateLimit:
    tool: str
    max_calls: int
    window_ms: int


@dataclass
class GovernancePolicy:
    default_action: GovernancePolicyAction | None = None
    rules: list[GovernancePolicyRule] = field(default_factory=list)
    vetoes: list[str] = field(default_factory=list)
    rate_limits: list[GovernanceRateLimit] = field(default_factory=list)
    constraints: list[dict[str, Any]] = field(default_factory=list)
    # I5: when True (default), the runner pre-filters denied tools out of the schema. Mirrors Node.
    surface_denied_in_system: bool = True


def governance_filter_schema(tools: list, policy: "GovernancePolicy | None") -> tuple[list, list[str]]:
    """I5: bucket tools into (allowed, denied) per the policy. Pure. Mirrors Node ``governanceFilterSchema``."""
    if policy is None:
        return tools, []
    vetoes = set(policy.vetoes or [])
    allowed: list = []
    denied: list[str] = []
    def matches(pat: str, name: str) -> bool:
        return pat == name or (pat.endswith("*") and name.startswith(pat[:-1]))
    for t in tools:
        name = t.name if hasattr(t, "name") else (t.get("name") if isinstance(t, dict) else None)
        if name is None:
            allowed.append(t)
            continue
        if name in vetoes:
            denied.append(name)
            continue
        action = policy.default_action or "allow"
        for r in (policy.rules or []):
            pat = r.pattern if hasattr(r, "pattern") else r.get("pattern")
            act = r.action if hasattr(r, "action") else r.get("action")
            if pat is not None and matches(pat, name):
                action = act
        if action == "deny":
            denied.append(name)
        else:
            allowed.append(t)
    return allowed, denied


def governance_policy_to_kernel_event(policy: GovernancePolicy) -> dict[str, Any]:
    constraints: list[dict[str, Any]] = []
    for c in policy.constraints:
        if c.get("kind") == "enum":
            constraints.append({
                "kind": "enum",
                "tool": c["tool"],
                "path": c["path"],
                "values": c["values"],
            })
        elif c.get("kind") == "range":
            entry: dict[str, Any] = {"kind": "range", "tool": c["tool"], "path": c["path"]}
            if "min" in c:
                entry["min"] = c["min"]
            if "max" in c:
                entry["max"] = c["max"]
            constraints.append(entry)
        else:
            constraints.append({"kind": "required", "tool": c["tool"], "path": c["path"]})
    return {
        "kind": "load_governance_policy",
        **({"default_action": policy.default_action} if policy.default_action else {}),
        "rules": [{"tool_pattern": r.pattern, "action": r.action} for r in policy.rules],
        "vetoed_tools": list(policy.vetoes),
        "rate_limits": [
            {"tool": rl.tool, "max_calls": rl.max_calls, "window_ms": rl.window_ms}
            for rl in policy.rate_limits
        ],
        "constraints": constraints,
    }
