"""L4 — Reactive desk: signals + the attention policy (Python mirror of 04-reactive-desk/main.ts).

L1's agent, now OPEN to the outside world. Two inbound channels feed events into a running loop;
both drain at a turn boundary and route through the kernel's attention policy (queue /
soft-interrupt / preempt by urgency):

  • SignalGateway (external) — a webhook / cron / upstream job calls ``gateway.ingest(signal)``.
    The gateway is the ``signal_source``; the loop pulls the next signal each turn. Here a "wire
    alert" is ingested the first time the agent searches — a real external event arriving mid-run.

  • inject_note (host) — ``runner.inject_note(text, urgency)`` pushes a contextual note on the same
    channel without wiring a full source. Here the host fires a ``"high"`` editor's note the first
    time the agent reads a source; ``"high"`` soft-interrupts (vs ``"normal"`` queue, ``"critical"``
    preempt). The note surfaces to the model as a ``[SIGNAL] …`` line.

To make the demo deterministic, both events fire as SIDE EFFECTS of the agent's own tool calls
(so they land mid-run every time, no wall-clock race). In production they'd come from a webhook
handler and a host monitor — the wiring the agent sees is identical.

New mechanism: Signals + reactive attention. Reused: tools, execution plane, provider (L1).

Run (from this directory):
    ../../python/.venv/bin/python main.py
    ../../python/.venv/bin/python main.py --dry-run
"""
from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path
from typing import Callable

# Make the example root importable so `from shared...` resolves when run as a script.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
EXAMPLE_ROOT = Path(__file__).resolve().parent.parent


def load_env() -> None:
    """Load example/.env then repo-root .env into os.environ (tiny parser, no dependency)."""
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
    RuntimeRunner,
    RuntimeOptions,
    LocalExecutionPlane,
    InMemorySessionLog,
    SignalGateway,
    RuntimeSignal,
    RegisteredTool,
    AnthropicProvider,
    OpenAIProvider,
    TextDelta,
    ToolCallEvent,
    ToolResultEvent,
    DoneEvent,
)
from shared.studio_tools import studio_tools  # noqa: E402


def resolve_provider():
    """Real provider from env — mirrors shared/provider.ts."""
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
    raise SystemExit(
        "No provider configured. Set ANTHROPIC_API_KEY (or OPENAI_API_KEY), "
        "or pass --dry-run to validate wiring without a live call."
    )


def once_after(base: RegisteredTool, effect: Callable[[], None]) -> RegisteredTool:
    """Wrap a tool so ``effect()`` fires once, when it is first invoked — a deterministic stand-in
    for an external event that happens to arrive while the agent is mid-task. Delegates to the base
    tool unchanged (its raw return value flows back through the base RegisteredTool wrapper, so
    async / streaming tools keep working)."""
    fired = {"v": False}

    def wrapped(**kwargs):
        if not fired["v"]:
            fired["v"] = True
            effect()
        return base.fn(**kwargs)

    # Reuse the base tool's schema verbatim — same model-facing name/args/description.
    return RegisteredTool(wrapped, base.schema)


async def main() -> None:
    load_env()
    dry_run = "--dry-run" in sys.argv[1:]

    gateway = SignalGateway()
    holder: dict[str, RuntimeRunner] = {}  # late-bound so tool side effects can reach inject_note

    search, read_source = studio_tools()
    plane = LocalExecutionPlane()

    # search → an external wire alert lands via the gateway (source="gateway", normal ⇒ queues).
    plane.register(
        once_after(
            search,
            lambda: gateway.ingest(
                RuntimeSignal(
                    kind="external",
                    source="gateway",
                    signal_type="alert",
                    urgency="normal",
                    payload={
                        "goal": "Wire alert (webhook): a correction to the signals source just landed "
                        "— treat src-signals as freshly revised."
                    },
                    dedupe_key="wire-correction",
                )
            ),
        )
    )
    # read_source → the host injects a HIGH-urgency editor's note (soft-interrupt).
    plane.register(
        once_after(
            read_source,
            lambda: holder["runner"].inject_note(
                "Editor's note: name the attention-policy ladder explicitly — "
                "queue (normal) / soft-interrupt (high) / preempt (critical).",
                "high",
            ),
        )
    )

    if dry_run:
        print("● L4 wiring check (no provider call)")
        print("  signal source : SignalGateway (implements SignalSource; pulled each turn)")
        print("  channel 1     : gateway.ingest(...)  → external event (fires on first search, normal ⇒ queue)")
        print('  channel 2     : runner.inject_note(..., "high")  → host note (fires on first read, high ⇒ soft-interrupt)')
        print("  ladder        : normal=queue · high=soft-interrupt · critical=preempt")
        print("  ✓ both drain at a turn boundary and surface as [SIGNAL] lines to the model.")
        return

    runner = RuntimeRunner(RuntimeOptions(
        provider=resolve_provider(),
        execution_plane=plane,
        session_log=InMemorySessionLog(),
        signal_source=gateway,  # the gateway IS the source the loop pulls from each turn
        max_tokens=200_000,
        max_turns=14,
    ))
    holder["runner"] = runner

    print("━━ reactive brief ━━ (events arrive mid-run; watch for [SIGNAL] lines in the reasoning)\n")
    async for event in runner.run(
        session_id="l4-reactive",
        goal=(
            "Using ONLY the studio index, write a short brief on how external events reach an agent. Search first, then "
            "read the most relevant source. If any wire alerts or editor's notes arrive while you work, acknowledge them "
            "and fold them into the brief. Cite the source id."
        ),
    ):
        if isinstance(event, TextDelta):
            print(event.delta, end="", flush=True)
        elif isinstance(event, ToolCallEvent):
            arg = next(iter(event.arguments.values()), "") if event.arguments else ""
            print(f"\n  [→ {event.name}({arg!r})]")
        elif isinstance(event, ToolResultEvent):
            preview = " ".join(event.content[:100].split())
            print(f"  [← {preview}{'…' if len(event.content) > 100 else ''}]")
        elif isinstance(event, DoneEvent):
            print(f"\n\n[done: {event.status} · {event.iterations} turns · ~{event.total_tokens} tokens]")

    gateway.destroy()
    print(
        "\nTwo events reached a running loop: an external gateway alert (queued) and a high-urgency host "
        "note (soft-interrupt). Both drained at a turn boundary through the kernel's attention policy — "
        "the agent never had to poll."
    )


if __name__ == "__main__":
    asyncio.run(main())
