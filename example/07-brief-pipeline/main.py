"""L7 — Brief pipeline: a dynamic workflow DAG (Python mirror of 07-brief-pipeline/main.ts).

This stops being one agent. ``runner.run_workflow(spec)`` lowers a declarative DAG to governed
sub-agent spawns and drives it to completion. This pipeline shows the core node vocabulary:

  node 0  research(cache)  ─┐   (spawn: a trusted sub-agent on the parent's plane; output_schema)
  node 1  research(memory) ─┤
                            ▼
  node 2  merge  (reducer "concat" — a DETERMINISTIC host-computed node, no LLM; depends_on 0,1)
                            ▼
  node 3  writer (spawn; depends_on 2 — the DAG edge carries node 2's OUTPUT as input; output_schema)
                            ▼
  node 4  gate   (role "verify"; depends_on 3 — an eval/harness node that judges the brief; output_schema)

Mechanisms: Workflow DAG · sub-agent spawn + trust/isolation (trusted ⇒ inherit parent plane) ·
structured output (``output_schema``, validate-and-retry) · reducer (host-compute) · data edges
(``depends_on``) · an in-DAG verify/eval gate. Every node spawn passes the one kernel syscall gate.

Run (from this directory):
    ../../python/.venv/bin/python main.py            (or --dry-run)
"""
from __future__ import annotations

import asyncio
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
    RuntimeRunner, RuntimeOptions, LocalExecutionPlane, InMemorySessionLog,
    AnthropicProvider, OpenAIProvider,
    WorkflowSpec, WorkflowNodeSpec,
)
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


# A JSON-Schema-subset the kernel validates each node's output against (and retries once on mismatch).
FINDING_SCHEMA = {
    "type": "object",
    "properties": {"source": {"type": "string"}, "claim": {"type": "string"}},
    "required": ["source", "claim"],
}
BRIEF_SCHEMA = {
    "type": "object",
    "properties": {"brief": {"type": "string"}, "sources": {"type": "array", "items": {"type": "string"}}},
    "required": ["brief", "sources"],
}
GATE_SCHEMA = {
    "type": "object",
    "properties": {"pass": {"type": "boolean"}, "reason": {"type": "string"}},
    "required": ["pass", "reason"],
}


def research_node(id: str, topic: str) -> WorkflowNodeSpec:
    return WorkflowNodeSpec(
        task=(
            f"Using ONLY the studio index, read_source the source '{id}' and output ONLY a JSON object "
            f'{{"source": "{id}", "claim": "<one-sentence factual claim about {topic} from that source>"}}. No prose.'
        ),
        role="custom",
        output_schema=FINDING_SCHEMA,
    )


spec = WorkflowSpec(nodes=[
    research_node("src-cache", "prompt caching"),      # node 0
    research_node("src-memory", "governed memory writes"),  # node 1
    WorkflowNodeSpec(task="merge findings", role="custom", reducer="concat", depends_on=[0, 1]),  # node 2 — deterministic
    WorkflowNodeSpec(
        # node 3 — writer; receives node 2's merged findings as input via the DAG edge.
        task=(
            "You are given two JSON findings (one per line) from upstream. Write a two-sentence research "
            "brief that states both claims and cites each source id in parentheses. Output ONLY JSON "
            '{"brief": "<two sentences with (src-...) citations>", "sources": ["src-cache", "src-memory"]}.'
        ),
        role="implement",
        output_schema=BRIEF_SCHEMA,
        depends_on=[2],
    ),
    WorkflowNodeSpec(
        # node 4 — quality gate (eval/harness as a DAG node): judge the brief, structured verdict.
        task=(
            "You are given a brief as JSON. Check it cites BOTH src-cache and src-memory and is two "
            'sentences. Output ONLY JSON {"pass": <bool>, "reason": "<short>"}.'
        ),
        role="verify",
        output_schema=GATE_SCHEMA,
        depends_on=[3],
    ),
])


async def main() -> None:
    load_env()
    _, flags = parse_args(sys.argv[1:])
    dry_run = flags.get("dry-run") is True

    plane = LocalExecutionPlane()
    for t in studio_tools():
        plane.register(t)

    if dry_run:
        print("● L7 wiring check (no provider call)")
        for i, n in enumerate(spec.nodes):
            kind = (
                f'reduce("{n.reducer}")' if n.reducer
                else "classify" if n.classify
                else "tournament" if n.tournament
                else "loop" if n.loop
                else "spawn"
            )
            schema = " +output_schema" if n.output_schema else ""
            deps = f" ←[{','.join(str(d) for d in n.depends_on)}]" if n.depends_on else ""
            print(f"  node {i}: {kind}{schema}{deps}  role={n.role}")
        print("  also available (see README): loop{max_iters}, classify{branches}, tournament{entrants}, run-level Milestones")
        print("  ✓ set a key and drop --dry-run to run the DAG live.")
        return

    runner = RuntimeRunner(RuntimeOptions(
        provider=resolve_provider(),
        execution_plane=plane,
        session_log=InMemorySessionLog(),
        max_tokens=200_000,
        max_turns=8,
    ))

    print("━━ running the brief-pipeline DAG ━━ (2 researchers → reduce → writer → gate)\n")
    outcome = await runner.run_workflow(spec)

    def show(key: str) -> str:
        return outcome["outputs"].get(key, "—")

    print("\n━━ workflow outcome ━━")
    print(f"  completed nodes : {len(outcome['completed'])}   failed: {len(outcome['failed'])}")
    print("\n  node 2 (reduce) merged findings:\n    " + show("wf-node2").replace("\n", "\n    "))
    print("\n  node 3 (writer) brief:\n    " + show("wf-node3"))
    print("\n  node 4 (gate) verdict:\n    " + show("wf-node4"))
    print(
        "\nFive nodes, one DAG: two spawns fanned out, a deterministic reducer merged them, a writer "
        "consumed the merge over a data edge, and a verify node gated the result — each spawn through "
        "the same kernel syscall, each structured output schema-validated."
    )


if __name__ == "__main__":
    asyncio.run(main())
