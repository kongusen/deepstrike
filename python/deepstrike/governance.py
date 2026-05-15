from __future__ import annotations

from dataclasses import dataclass

from deepstrike._kernel import Governance as KernelGovernance


@dataclass(frozen=True)
class GovernanceVerdict:
    kind: str
    reason: str | None = None
    retry_after_ms: float | None = None


class Governance:
    """Public Python facade for the native governance pipeline."""

    def __init__(self, default_action: str = "allow"):
        self._inner = KernelGovernance(default_action)

    def set_identity(self, agent_id: str, session_id: str) -> None:
        self._inner.set_identity(agent_id, session_id)

    def add_permission_rule(self, pattern: str, action: str) -> None:
        self._inner.add_permission_rule(pattern, action)

    def block_tool(self, name: str) -> None:
        self._inner.block_tool(name)

    def set_rate_limit(self, tool_name: str, max_calls: int, window_ms: int) -> None:
        self._inner.set_rate_limit(tool_name, max_calls, window_ms)

    def require_param(self, tool_name: str, param_path: str) -> None:
        self._inner.require_param(tool_name, param_path)

    def allow_param_values(
        self,
        tool_name: str,
        param_path: str,
        allowed_values: list[str],
    ) -> None:
        self._inner.allow_param_values(tool_name, param_path, allowed_values)

    def limit_param_range(
        self,
        tool_name: str,
        param_path: str,
        min_value: float | None = None,
        max_value: float | None = None,
    ) -> None:
        self._inner.limit_param_range(tool_name, param_path, min_value, max_value)

    def set_time(self, now_ms: int) -> None:
        self._inner.set_time(now_ms)

    def evaluate(self, tool_name: str, args_json: str) -> GovernanceVerdict:
        verdict = self._inner.evaluate(tool_name, args_json)
        return GovernanceVerdict(
            kind=verdict.kind,
            reason=verdict.reason,
            retry_after_ms=verdict.retry_after_ms,
        )
