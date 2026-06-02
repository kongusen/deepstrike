import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    Message,
    RuntimeOptions,
    RuntimeRunner,
    collect_text,
    tool,
)
from deepstrike.governance import GovernancePolicy, GovernancePolicyRule
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime.os_profile import (
    DEFAULT_NATIVE_ATTENTION_POLICY,
    DEFAULT_NATIVE_GOVERNANCE_POLICY,
)
from deepstrike.runtime.os_snapshot import (
    rebuild_os_snapshot_from_session_events,
    session_log_has_required_categories,
)


class _StaticProvider:
    def __init__(self, *, tool_once: bool = False) -> None:
        self._tool_once = tool_once
        self._n = 0

    async def complete(self) -> Message:
        return Message(role="assistant", content="done", tool_calls=[])

    async def stream(self, context, tools, extensions=None, state=None):
        self._n += 1
        if self._tool_once and self._n == 1:
            yield ToolCallEvent(id="c1", name="needs_approval", arguments={})
            return
        yield TextDelta(delta="ok")


@pytest.mark.asyncio
async def test_native_profile_writes_categorized_kernel_events():
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StaticProvider(),
        session_log=InMemorySessionLog(),
        attention_policy=DEFAULT_NATIVE_ATTENTION_POLICY,
        governance_policy=DEFAULT_NATIVE_GOVERNANCE_POLICY,
    ))
    await collect_text(runner.run(session_id="native-ok", goal="work"))
    events = [e.event for e in await runner._opts.session_log.read("native-ok")]
    assert session_log_has_required_categories(events)
    snap = rebuild_os_snapshot_from_session_events(events)
    assert snap.page_out_count >= 0


@pytest.mark.asyncio
async def test_native_profile_ask_user_emits_syscall_sched_events():
    plane = LocalExecutionPlane()

    @tool
    def needs_approval() -> str:
        """Needs approval."""
        return "ok"

    plane.register(needs_approval)
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StaticProvider(tool_once=True),
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        os_profile="native",
        attention_policy=DEFAULT_NATIVE_ATTENTION_POLICY,
        governance_policy=GovernancePolicy(
            rules=[GovernancePolicyRule(pattern="needs_approval", action="ask_user")],
        ),
        on_permission_request=lambda _req: {"approved": True, "responder": "test"},
        max_turns=6,
    ))
    await collect_text(runner.run(session_id="native-gov", goal="go"))
    events = [e.event for e in await runner._opts.session_log.read("native-gov")]
    assert any(e.get("kind") == "tool_gated" and e.get("category") == "syscall" for e in events)
    assert any(e.get("kind") == "suspended" and e.get("category") == "sched" for e in events)
    assert session_log_has_required_categories(events)
    snap = rebuild_os_snapshot_from_session_events(events)
    assert snap.tool_gated_count >= 1
