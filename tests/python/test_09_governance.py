"""
09 — PermissionManager + kernel Governance + Agent-level blocking
"""
import pytest

from deepstrike import (
    PermissionManager, PermissionMode,
    Governance,
    tool,
)
from deepstrike.providers.stream import ErrorEvent, DoneEvent

from conftest import make_agent, collect_events


# ─── PermissionManager (offline) ────────────────────────────────────────────

class TestPermissionManager:
    def test_auto_allows_everything(self):
        assert PermissionManager(PermissionMode.AUTO).evaluate("any", "exec").allowed is True

    def test_plan_blocks_everything(self):
        assert PermissionManager(PermissionMode.PLAN).evaluate("any", "exec").allowed is False

    def test_default_ungranted_denied(self):
        assert PermissionManager().evaluate("tool", "exec").allowed is False

    def test_default_granted_allowed(self):
        pm = PermissionManager()
        pm.grant("tool", "exec")
        assert pm.evaluate("tool", "exec").allowed is True

    def test_revoked_denied_with_note(self):
        pm = PermissionManager()
        pm.grant("tool", "exec")
        pm.revoke("tool", "exec", note="security policy")
        d = pm.evaluate("tool", "exec")
        assert d.allowed is False
        assert "security policy" in d.reason

    def test_wildcard_grant(self):
        pm = PermissionManager()
        pm.grant("*", "*")
        assert pm.evaluate("foo", "bar").allowed is True

    def test_requires_approval_blocks(self):
        pm = PermissionManager()
        pm.grant("tool", "exec", requires_approval=True)
        d = pm.evaluate("tool", "exec")
        assert d.allowed is False
        assert d.requires_approval is True


# ─── Kernel Governance (offline) ────────────────────────────────────────────

class TestGovernanceKernel:
    def test_block_tool(self):
        gov = Governance()
        gov.block_tool("dangerous")
        agent = make_agent(governance=gov)
        agent.block_tool("dangerous")
        assert "dangerous" in agent._blocked_tools

    def test_set_time_does_not_throw(self):
        gov = Governance()
        gov.set_time(1000)


# ─── Agent with Governance (real API) ───────────────────────────────────────

class TestAgentGovernance:
    @pytest.mark.timeout(120)
    async def test_blocked_tool_yields_error_and_terminates(self):
        gov = Governance()
        gov.block_tool("forbidden_action")

        @tool
        def forbidden_action() -> str:
            """Perform a forbidden action."""
            return "done"

        @tool
        def safe_reply(msg: str) -> str:
            """Reply safely."""
            return msg

        agent = make_agent(governance=gov)
        agent.register(forbidden_action)
        agent.register(safe_reply)
        agent.block_tool("forbidden_action")

        events = await collect_events(
            agent.run_streaming("First call forbidden_action. If blocked, call safe_reply with msg='ok'.")
        )

        errors = [e for e in events if isinstance(e, ErrorEvent)]
        if errors:
            assert any("forbidden_action" in e.message or "blocked" in e.message for e in errors), \
                f"errors: {[e.message for e in errors]}"
        assert sum(1 for e in events if isinstance(e, DoneEvent)) == 1
