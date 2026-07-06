"""L1 — Sourced Q&A assistant (Python mirror of 01-sourced-qa/main.ts).

The smallest real agent: a RuntimeRunner wired to a provider, a LocalExecutionPlane holding the
studio's search / read_source tools, and a FileSessionLog so a run is durable and resumable.

Mechanisms introduced here (reused by every later level):
  • Tools + Execution Plane   • Provider   • Session log / replay & recovery

Run:
    cd deepstrike/python && pip install -e .        # once
    ANTHROPIC_API_KEY=sk-... python ../example/01-sourced-qa/main.py "How does prompt caching work?"
    python ../example/01-sourced-qa/main.py --dry-run   # validate wiring, no key, no call

Resume: run once, Ctrl-C mid-answer, re-run with --session my-run (a FileSessionLog persists it).
"""
from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path

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
    FileSessionLog,
    AnthropicProvider,
    OpenAIProvider,
    TextDelta,
    ToolCallEvent,
    ToolResultEvent,
    DoneEvent,
)
from shared.studio_tools import studio_tools  # noqa: E402

HERE = Path(__file__).resolve().parent


def resolve_provider():
    """Real provider from env — mirrors shared/provider.ts. Only override the provider's default
    model when DEEPSTRIKE_MODEL is actually set (passing None would clobber the default)."""
    if os.environ.get("ANTHROPIC_API_KEY"):
        model = os.environ.get("DEEPSTRIKE_MODEL")
        return AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"], **({"model": model} if model else {}))
    if os.environ.get("OPENAI_API_KEY"):
        # Honor standard OpenAI env names (what a typical .env uses); DEEPSTRIKE_* overrides.
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


def parse_args(argv: list[str]):
    positionals, flags = [], {}
    i = 0
    while i < len(argv):
        a = argv[i]
        if a.startswith("--"):
            key = a[2:]
            nxt = argv[i + 1] if i + 1 < len(argv) else None
            if nxt is not None and not nxt.startswith("--"):
                flags[key] = nxt
                i += 1
            else:
                flags[key] = True
        else:
            positionals.append(a)
        i += 1
    return positionals, flags


async def main() -> None:
    load_env()
    positionals, flags = parse_args(sys.argv[1:])
    goal = " ".join(positionals) or "How does prompt caching stay effective across turns? Cite your sources."
    session_id = flags.get("session") if isinstance(flags.get("session"), str) else "l1-sourced-qa"
    dry_run = flags.get("dry-run") is True

    plane = LocalExecutionPlane()
    for t in studio_tools():
        plane.register(t)
    session_log = FileSessionLog(str(HERE / ".sessions"))

    if dry_run:
        print("● L1 wiring check (no provider call)")
        print(f"  session id : {session_id}")
        print(f"  session log: {HERE / '.sessions'}/{session_id}.jsonl")
        print(f"  tools      : {', '.join(t.schema.name for t in studio_tools())}")
        print(f"  goal       : {goal}")
        print("  ✓ runner constructs; set ANTHROPIC_API_KEY and drop --dry-run to run it live.")
        return

    runner = RuntimeRunner(RuntimeOptions(
        provider=resolve_provider(),
        execution_plane=plane,
        session_log=session_log,
        max_tokens=200_000,
        max_turns=12,
    ))

    prior = await session_log.read(session_id)
    if prior:
        print(f"↻ resuming session '{session_id}' ({len(prior)} prior events)\n")

    async for event in runner.run(session_id=session_id, goal=goal):
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


if __name__ == "__main__":
    asyncio.run(main())
