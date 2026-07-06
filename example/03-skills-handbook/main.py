"""L3 — Skills handbook + Knowledge (Python mirror of 03-skills-handbook/main.ts).

L1's sourced agent, now with two capability-plane mechanisms:

  • SKILLS (on-demand capability + tool gating). A ``skill_dir`` catalog exposes a ``skill`` meta-tool.
    The catalog carries only each skill's *metadata*; the body loads lazily when the model calls
    ``skill(name)``. Loading ``citation-style`` narrows the exposed toolset to
    ``stable_core ∪ allowed_tools`` — so the off-task ``list_index`` tool DISAPPEARS while the skill is
    active. ``on_turn_metrics`` prints ``tools_exposed`` per turn so you can watch the surface shrink.

  • KNOWLEDGE (durable pinned partition). A ``KnowledgeSource`` is queried once at run start and its
    hits are pinned into the knowledge slot at the front of context — distinct from a skill body
    (loaded by the model, gated, lease-swept) and from memory (recalled, decaying). Here it pins the
    studio's non-negotiable style rule.

New mechanisms: Skills, tool gating, Knowledge. Reused: tools, execution plane, provider (L1).

Run (from this directory):
    ../../python/.venv/bin/python main.py            (or --dry-run)
"""
from __future__ import annotations

import asyncio
import json
import os
import sys
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
    AnthropicProvider,
    OpenAIProvider,
    KnowledgeSource,
    tool,
    TextDelta,
    ToolCallEvent,
    ToolResultEvent,
    DoneEvent,
)
from shared.studio_tools import studio_tools, CORPUS  # noqa: E402

HERE = Path(__file__).resolve().parent


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


@tool
def format_citation(id: str) -> str:
    """Render a source id into the studio's canonical citation form `[Title — id]`. Use for EVERY cited claim."""
    src = next((s for s in CORPUS if s["id"] == id), None)
    return f"[{src['title']} — {src['id']}]" if src else f"[unknown source '{id}']"


@tool
def list_index() -> str:
    """List every source in the studio index as {id, title}. A browsing aid, not a citation tool."""
    return json.dumps([{"id": s["id"], "title": s["title"]} for s in CORPUS])


class StyleGuide:
    """A tiny static KnowledgeSource: one pinned house rule, retrieved at run start. A real one would
    wrap a vector index or a docs API; the contract is just ``init()`` + ``retrieve(goal, top_k)``."""

    async def init(self) -> None:
        pass

    async def retrieve(self, goal: str, top_k: int = 4) -> list[str]:
        return [
            "STUDIO STYLE (non-negotiable): every factual sentence in a brief must carry a citation "
            "produced by the format_citation tool; uncited claims are rejected in review."
        ]


def on_metrics(m) -> None:
    # Tool-gating telemetry: watch the exposed surface shrink the turn the skill activates.
    print(f"\n  · turn {m.turn}: exposed={m.tools_exposed} called={m.tools_called} skill={m.active_skill or '—'}")


async def main() -> None:
    load_env()
    _positionals, flags = parse_args(sys.argv[1:])
    dry_run = flags.get("dry-run") is True

    plane = LocalExecutionPlane()
    tools = [*studio_tools(), format_citation, list_index]
    for t in tools:
        plane.register(t)

    style_guide: KnowledgeSource = StyleGuide()

    if dry_run:
        print("● L3 wiring check (no provider call)")
        print(f"  skill dir      : {HERE / 'skills'}  → 'skill' meta-tool over the catalog")
        print(f"  base tools     : {', '.join(t.schema.name for t in tools)}")
        print("  stable core    : search, read_source  (always exposed)")
        print("  skill gating   : citation-style allows [format_citation] → list_index hides while active")
        print("  knowledge      : styleGuide  → 1 pinned rule retrieved at run start")
        print("  ✓ set a key and drop --dry-run to watch tools_exposed shrink when the skill loads.")
        return

    runner = RuntimeRunner(RuntimeOptions(
        provider=resolve_provider(),
        execution_plane=plane,
        session_log=InMemorySessionLog(),
        skill_dir=str(HERE / "skills"),
        stable_core_tool_ids=["search", "read_source"],  # survive skill gating; everything else is gated
        knowledge_source=style_guide,
        max_tokens=200_000,
        max_turns=14,
        on_turn_metrics=on_metrics,
    ))

    print("━━ write a cited brief ━━ (the agent loads the citation-style skill, then writes)\n")
    async for event in runner.run(
        session_id="l3-brief",
        goal=(
            "Load the citation-style skill first. Then, using ONLY the studio index, write a two-sentence "
            "brief on how prompt caching stays effective across turns. Cite every claim with format_citation "
            "and end with a Sources: line."
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

    print(
        "\nNote the turn where `skill=citation-style` appears: `exposed` drops because `list_index` is "
        "gated away — only stable-core (search, read_source) + the skill's format_citation remain."
    )


if __name__ == "__main__":
    asyncio.run(main())
