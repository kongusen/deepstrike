"""L5 — Governed studio (Python mirror of 05-governed-studio/main.ts).

L1's agent, now behind a policy. Three control-plane mechanisms sit between the model and the
world, all declarative and kernel-enforced:

  • GOVERNANCE (allow / deny / ask_user). A ``governance_policy`` classifies each tool. ``deny`` tools
    are pre-filtered OUT of the schema — the model never sees ``publish_public``, so it can't even
    try. ``ask_user`` tools reach the model but pause at CALL time: ``email_editor`` raises a
    PermissionRequestEvent that the host (``on_permission_request``) adjudicates.

  • RESOURCE QUOTA. A ``resource_quota`` bounds spawn concurrency / depth / cumulative sub-agents —
    the hard caps the kernel enforces regardless of what the model asks.

  • OS PROFILE snapshot. ``os_profile("native")`` resolves the concrete kernel-owned policy defaults;
    after the run, ``rebuild_os_snapshot_from_session_events`` reconstructs what the kernel actually
    enforced (tool-gated count, signals, memory ops) from the durable session log — an audit trail.

New mechanisms: Governance, Resource quota, OS profile. Reused: tools, execution plane, provider.

Run (from this directory):
    ../../python/.venv/bin/python main.py
    ../../python/.venv/bin/python main.py --dry-run
"""
from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # make `shared` importable
EXAMPLE_ROOT = Path(__file__).resolve().parent.parent


def load_env() -> None:
    for p in (EXAMPLE_ROOT / ".env", EXAMPLE_ROOT.parent / ".env"):
        if p.exists():
            for line in p.read_text(encoding="utf-8").splitlines():
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                k, v = line.split("=", 1)
                os.environ.setdefault(k.strip(), v.strip())
            return


from deepstrike import (  # noqa: E402
    RuntimeRunner, RuntimeOptions, LocalExecutionPlane, InMemorySessionLog,
    ResourceQuota, os_profile, tool,
    AnthropicProvider, OpenAIProvider,
    TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent,
    PermissionRequestEvent, PermissionResolvedEvent,
)
from deepstrike.governance import GovernancePolicy, GovernancePolicyRule  # noqa: E402
from deepstrike.runtime.os_snapshot import rebuild_os_snapshot_from_session_events  # noqa: E402
from shared.studio_tools import studio_tools  # noqa: E402


def resolve_provider():
    if os.environ.get("ANTHROPIC_API_KEY"):
        model = os.environ.get("DEEPSTRIKE_MODEL")
        return AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"], **({"model": model} if model else {}))
    if os.environ.get("OPENAI_API_KEY"):
        model = os.environ.get("DEEPSTRIKE_MODEL") or os.environ.get("OPENAI_MODEL")
        base_url = os.environ.get("DEEPSTRIKE_BASE_URL") or os.environ.get("OPENAI_BASE_URL")
        kw = {}
        if model:
            kw["model"] = model
        if base_url:
            kw["base_url"] = base_url
        return OpenAIProvider(api_key=os.environ["OPENAI_API_KEY"], **kw)
    raise SystemExit("No provider configured. Set ANTHROPIC_API_KEY (or OPENAI_API_KEY), or pass --dry-run.")


@tool
def email_editor(to: str, summary: str) -> str:
    """Notify a recipient that the brief is ready. Args: { to, summary }. Governed — the host approves it."""
    return f"✓ editor notified ({to}). The brief is delivered — the task is COMPLETE, do not notify again."


@tool
def publish_public(text: str) -> str:
    """Publish the brief to the public website (irreversible)."""
    return "PUBLISHED (this should be unreachable — the policy denies this tool)"


def render(event) -> None:
    """Inline stream-event renderer (mirrors shared/render.ts), incl. the ask_user permission gate."""
    if isinstance(event, PermissionRequestEvent):
        print(f"\n  [⚖ ask_user: {event.tool_name}({event.arguments[:80]}) — {event.reason}]")
    elif isinstance(event, PermissionResolvedEvent):
        verdict = "APPROVED" if event.approved else "DENIED"
        tail = f" — {event.reason}" if event.reason else ""
        print(f"  [⚖ {verdict} by {event.responder}{tail}]")
    elif isinstance(event, TextDelta):
        print(event.delta, end="", flush=True)
    elif isinstance(event, ToolCallEvent):
        arg = next(iter(event.arguments.values()), "") if event.arguments else ""
        print(f"\n  [→ {event.name}({arg!r})]")
    elif isinstance(event, ToolResultEvent):
        preview = " ".join(event.content[:100].split())
        print(f"  [← {preview}{'…' if len(event.content) > 100 else ''}]")
    elif isinstance(event, DoneEvent):
        print(f"\n\n[done: {event.status} · {event.iterations} turns · ~{event.total_tokens} tokens]")


async def main() -> None:
    load_env()
    dry_run = "--dry-run" in sys.argv[1:]

    tools = [*studio_tools(), email_editor, publish_public]
    plane = LocalExecutionPlane()
    for t in tools:
        plane.register(t)

    # Declarative policy: default allow, one hard deny, one ask_user gate.
    governance_policy = GovernancePolicy(
        default_action="allow",
        rules=[
            GovernancePolicyRule(pattern="publish_public", action="deny"),   # schema pre-filtered → invisible
            GovernancePolicyRule(pattern="email_editor", action="ask_user"),  # pauses for host adjudication
        ],
    )
    # Hard caps the kernel enforces no matter what the model plans (no sub-agents here, but the caps
    # are lowered into the run and would bound L7/L8's fan-out).
    resource_quota = ResourceQuota(max_concurrent_subagents=2, max_total_subagents=4, max_spawn_depth=2)

    # The host's adjudicator for every ask_user gate. The gate is TOOL-SCOPED (the kernel surfaces the
    # tool name + reason, not the call args), so the host decides per capability: `email_editor` is the
    # studio's own notification tool → approve; anything else escalated → refuse. A one-line policy the
    # MODEL cannot override — approval authority lives with the host, not the prompt.
    def on_permission_request(e: PermissionRequestEvent):
        approved = e.tool_name == "email_editor"
        return {
            "approved": approved,
            "responder": "studio-host",
            "reason": "studio notification tool" if approved else f"unapproved capability '{e.tool_name}'",
        }

    profile = os_profile("native")
    if dry_run:
        print("● L5 wiring check (no provider call)")
        print(f"  base tools     : {', '.join(t.schema.name for t in tools)}")
        print("  governance     : deny publish_public (invisible) · ask_user email_editor (host-adjudicated)")
        print(f"  resource quota : max_concurrent={resource_quota.max_concurrent_subagents} "
              f"max_total={resource_quota.max_total_subagents} max_depth={resource_quota.max_spawn_depth}")
        print(f"  os profile     : {profile.id}  · governance "
              f"{[(r.pattern, r.action) for r in profile.governance_policy.rules]}")
        print("  ✓ set a key and drop --dry-run to watch deny + ask_user gates fire.")
        return

    # In-memory log: each run starts clean (this level teaches governance, not resume — see L1), yet
    # we can still rebuild the OS snapshot from the events it captured this run.
    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=resolve_provider(),
        execution_plane=plane,
        session_log=session_log,
        governance_policy=governance_policy,
        resource_quota=resource_quota,
        os_profile="native",
        on_permission_request=on_permission_request,
        max_tokens=200_000,
        max_turns=14,
    ))

    print("━━ governed run ━━ (publish_public is denied & invisible; email_editor pauses for the host)\n")
    session_id = "l5-governed"
    goal = (
        "Using ONLY the studio index, write a ONE-sentence brief on how memory writes are governed (cite the id). "
        "Then call email_editor EXACTLY ONCE with to='editor' to notify them, and stop. "
        "Do NOT publish it publicly and do NOT notify more than once."
    )
    async for event in runner.run(session_id=session_id, goal=goal):
        render(event)

    # OS profile snapshot: reconstruct what the kernel actually enforced, from the durable log.
    logged = await session_log.read(session_id)
    events = [entry.event for entry in logged]
    snap = rebuild_os_snapshot_from_session_events(events)
    print(f"\n━━ OS snapshot (rebuilt from {len(events)} session events) ━━")
    print(f"  tool-gated (ask_user) : {snap.tool_gated_count}")
    print(f"  memory written        : {snap.memory_written_count}")
    print(f"  signals routed        : {len(snap.signals)}")
    print("\npublish_public never appeared in the toolset (policy deny → schema pre-filter); email_editor "
          "reached the model but the HOST decided whether it fired. The control plane, not the prompt, is authority.")


if __name__ == "__main__":
    asyncio.run(main())
