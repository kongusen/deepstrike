"""L2 — Assistant with memory (Python mirror of 02-memory-assistant/main.ts).

The same sourced-Q&A agent from L1, now given a DreamStore. Two things change:
  • RECALL — at the start of every run the runner recalls relevant memories (pre_query_memory,
    default-on) and injects them into the decaying history, so the model sees prior knowledge on
    turn one; the agent can also query memory on demand via the `memory` meta-tool.
  • WRITE  — persisting a memory goes through ONE governed gate, runner.write_memory(...):
    validation + a rolling-window write quota + an advisory score (a near-duplicate write is also
    subject to jaccard dedup at this gate). The host decides what is worth keeping — here, the
    takeaway from a research run.

This example runs TWO sessions in one process under the SAME agent_id + store:
  session A ("learn")  — research a topic; the host persists the takeaway through the write gate
  session B ("recall") — a fresh session id asks a follow-up; the fact is recalled at run-start

New mechanism: Memory. Reused: tools, execution plane, provider, session log (L1).

Run (from this directory):
    ../../python/.venv/bin/python main.py
    ../../python/.venv/bin/python main.py --dry-run
"""
from __future__ import annotations

import asyncio
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # make `shared` importable
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
    InMemoryDreamStore,
    AnthropicProvider,
    OpenAIProvider,
    TextDelta,
    ToolCallEvent,
    ToolResultEvent,
    DoneEvent,
)
from shared.studio_tools import studio_tools  # noqa: E402

AGENT_ID = "studio-researcher"


def resolve_provider():
    """Real provider from env — mirrors shared/provider.ts. Only override the provider's default
    model when DEEPSTRIKE_MODEL is actually set (passing None would clobber the default)."""
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


def render(event) -> None:
    """Inline stream-event renderer (mirrors shared/render.ts) — L2's teaching artifact."""
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


async def main() -> None:
    load_env()
    _positionals, flags = parse_args(sys.argv[1:])
    dry_run = flags.get("dry-run") is True

    plane = LocalExecutionPlane()
    for t in studio_tools():
        plane.register(t)
    # One store shared by both sessions. A memory written in session A is recalled in session B.
    dream_store = InMemoryDreamStore()

    if dry_run:
        print("● L2 wiring check (no provider call)")
        print(f"  agent id : {AGENT_ID}  (memory is keyed per agent, not per session)")
        print("  store    : InMemoryDreamStore  → run-start recall + the 'memory' query tool turn on")
        print("  write    : runner.write_memory({content, metadata})  → the one governed gate")
        print("  ✓ configure dream_store + agent_id and the memory mechanism turns on.")
        return

    runner = RuntimeRunner(RuntimeOptions(
        provider=resolve_provider(),
        execution_plane=plane,
        session_log=InMemorySessionLog(),
        dream_store=dream_store,
        agent_id=AGENT_ID,  # memory requires BOTH dream_store and agent_id
        max_tokens=200_000,
        max_turns=12,
    ))

    # ── Session A: research; capture the takeaway ────────────────────────────────
    print("━━ session A · learn ━━ (research a topic; the answer becomes a memory)\n")
    takeaway = ""
    async for event in runner.run(
        session_id="l2-learn",
        goal=(
            "Using ONLY the studio index (do not answer from prior knowledge): search for the source about "
            "loop agents, read it, then answer in ONE sentence with its source id in parentheses."
        ),
    ):
        if isinstance(event, TextDelta):
            takeaway += event.delta
        render(event)
    takeaway = takeaway.strip()

    # ── Write through the one governed gate ──────────────────────────────────────
    now = int(time.time() * 1000)
    await runner.write_memory({
        "content": takeaway,
        "metadata": {
            "name": "loop-agent-takeaway",
            "description": "One-sentence definition of a loop agent, learned in session A.",
            "kind": "reference",
            "created_at": now,
            "updated_at": now,
        },
    })

    stored = await dream_store.load_memories(AGENT_ID)
    print(f"\n━━ long-term memory now holds {len(stored)} entry(ies) (via the write_memory gate) ━━")
    for m in stored:
        print(f"  • [score {m.score:.2f}] {m.text}")

    # ── Session B: recall (fresh session id, same agent + store) ─────────────────
    print("\n━━ session B · recall ━━ (a NEW session; the fact surfaces at run-start)\n")
    async for event in runner.run(
        session_id="l2-recall",
        goal="Without searching again, what did we already learn about how a loop agent works?",
    ):
        render(event)
    print(
        "\nThe answer came from recalled memory, not a fresh search — run-start recall injected the "
        "session-A takeaway into session B's history before turn one."
    )


if __name__ == "__main__":
    asyncio.run(main())
