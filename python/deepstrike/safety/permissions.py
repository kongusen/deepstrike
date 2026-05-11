from __future__ import annotations
from dataclasses import dataclass
from enum import Enum


class PermissionMode(Enum):
    DEFAULT = "default"
    PLAN = "plan"
    AUTO = "auto"


@dataclass
class Permission:
    tool: str
    action: str
    allowed: bool = True
    requires_approval: bool = False
    note: str = ""


@dataclass
class PermissionDecision:
    allowed: bool
    reason: str = ""
    requires_approval: bool = False
    matched_permission: Permission | None = None


class PermissionManager:
    def __init__(self, mode: PermissionMode = PermissionMode.DEFAULT):
        self.permissions: dict[str, Permission] = {}
        self.mode = mode

    def grant(self, tool: str, action: str, *, requires_approval: bool = False, note: str = ""):
        self.permissions[self._key(tool, action)] = Permission(
            tool=tool, action=action, allowed=True, requires_approval=requires_approval, note=note
        )

    def revoke(self, tool: str, action: str, *, note: str = ""):
        self.permissions[self._key(tool, action)] = Permission(
            tool=tool, action=action, allowed=False, note=note
        )

    def check(self, tool: str, action: str) -> tuple[bool, str]:
        decision = self.evaluate(tool, action)
        return decision.allowed, decision.reason

    def evaluate(self, tool: str, action: str) -> PermissionDecision:
        if self.mode == PermissionMode.PLAN:
            return PermissionDecision(allowed=False, reason="Plan mode: execution not allowed")

        permission = self._match_permission(tool, action)
        if permission:
            if not permission.allowed:
                return PermissionDecision(allowed=False, reason=permission.note or "Permission denied", matched_permission=permission)
            if permission.requires_approval and self.mode != PermissionMode.AUTO:
                return PermissionDecision(allowed=False, reason=permission.note or "Approval required", requires_approval=True, matched_permission=permission)
            return PermissionDecision(allowed=True, matched_permission=permission)

        if self.mode == PermissionMode.AUTO:
            return PermissionDecision(allowed=True, reason="Auto mode")

        return PermissionDecision(allowed=False, reason="No explicit permission")

    def _match_permission(self, tool: str, action: str) -> Permission | None:
        for key in [self._key(tool, action), self._key(tool, "*"), self._key("*", action), self._key("*", "*")]:
            if key in self.permissions:
                return self.permissions[key]
        return None

    def _key(self, tool: str, action: str) -> str:
        return f"{tool}:{action}"
