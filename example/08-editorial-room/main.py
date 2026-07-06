"""L8 — Editorial room (Python mirror of 08-editorial-room/main.ts).

Several PEER agents share one blackboard and react to each other — the second orchestration surface —
under one cumulative RunGroup budget. And a peer's turn can itself be a workflow DAG (DAG-in-Peer), so
the two surfaces compose on the shared governance floor.

  • ReactiveSession — personas subscribe to a shared EventStream; emit() runs a TurnPolicy
    (react_by_mention + audience) to pick reactors; each reaction is a normal run().
  • RunGroup — every persona (and every sub-agent) charges one shared ledger; all are recorded members.
  • DAG-in-Peer — the scribe's `react` override calls runner.run_workflow(...); its node spawns charge
    the SAME RunGroup.
  • Blackboard read — reviewers pull the draft via the read_recent tool.

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
    InMemoryGroupBudgetStore, InMemoryEventStream, EventViewer,
    ReactiveSession, read_recent_tool, react_by_mention, RunGroup,
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
    raise SystemExit("No provider configured. Set OPENAI_API_KEY (or ANTHROPIC_API_KEY), or pass --dry-run.")


# The scribe's reaction IS a workflow DAG: research src-cache, then write one cited sentence.
SCRIBE_WORKFLOW = WorkflowSpec(nodes=[
    WorkflowNodeSpec(
        task='Using ONLY the studio index, read_source "src-cache" and output ONLY JSON '
             '{"source":"src-cache","claim":"<one sentence>"}.',
        role="custom",
        output_schema={
            "type": "object",
            "properties": {"source": {"type": "string"}, "claim": {"type": "string"}},
            "required": ["source", "claim"],
        },
    ),
    WorkflowNodeSpec(
        task="Given the JSON finding, write ONE sentence: the claim followed by (src-cache). Plain text only.",
        role="implement",
        depends_on=[0],
    ),
])


def make_runner(persona_id: str, shared: dict):
    plane = LocalExecutionPlane()
    for t in studio_tools():
        plane.register(t)
    plane.register(read_recent_tool(shared["event_stream"], EventViewer(persona_id)))
    return RuntimeRunner(RuntimeOptions(
        provider=resolve_provider(),
        execution_plane=plane,
        session_log=InMemorySessionLog(),
        agent_id=persona_id,
        run_group=shared["run_group"],   # the shared governance domain
        signal_source=shared["signal_source"],
        max_tokens=200_000,
        max_turns=6,
    ))


def goal_for(persona_id: str, event) -> str:
    task = event.payload if isinstance(event.payload, str) else str(event.payload)
    if persona_id == "editor":
        return (f"You are the editor. First call read_recent once to see the latest draft on the blackboard. "
                f"Then reply with ONE plain-text sentence of concrete feedback — no JSON, no tool syntax. Context: {task}")
    if persona_id == "factchecker":
        return (f"You are the fact-checker. First call read_recent once to see the latest draft. Then reply with "
                f"ONE plain-text sentence stating whether its (src-cache) citation is legitimate — no JSON. Context: {task}")
    return f"React in one sentence. Context: {task}"


async def scribe_react(ctx) -> str:
    wf = await ctx.runner.run_workflow(SCRIBE_WORKFLOW)  # DAG-in-Peer, under the shared RunGroup
    return wf["outputs"].get("wf-node1", "(scribe produced no draft)")


async def main() -> None:
    load_env()
    dry_run = "--dry-run" in sys.argv[1:]

    store = InMemoryGroupBudgetStore()
    run_group = RunGroup(id="editorial-room", budget_store=store)
    event_stream = InMemoryEventStream()

    if dry_run:
        print("● L8 wiring check (no provider call)")
        print(f"  run group    : {run_group.id}  (shared cumulative budget + membership)")
        print("  peers        : scribe (DAG-in-Peer → run_workflow), editor (run()), factchecker (run())")
        print("  ✓ set a key and drop --dry-run to run the room live.")
        return

    session = ReactiveSession(
        run_group=run_group,
        turn_policy=react_by_mention(),
        event_stream=event_stream,
        make_runner=make_runner,
        goal_for=goal_for,
    )
    session.add_peer("scribe", role="writer", react=scribe_react)
    session.add_peer("editor", role="editor")
    session.add_peer("factchecker", role="factchecker")

    print("━━ round 1 · director → scribe (a DAG-in-Peer reaction) ━━")
    r1 = await session.emit("scribe, draft a one-sentence brief on prompt caching.", source="director")
    draft = next((r.output for r in r1 if r.persona_id == "scribe"), "(no draft)")
    print(f"  scribe drafted: {draft}\n")

    await event_stream.append(f"DRAFT: {draft}", source="scribe")

    print("━━ round 2 · director → editor + factchecker (peers react to the blackboard) ━━")
    r2 = await session.emit(
        "editor and factchecker, please review the latest draft.",
        source="director", audience=["editor", "factchecker"],
    )
    for r in r2:
        print(f"  {r.persona_id}: {r.output}")

    ledger = await store.read(run_group.id)
    members = await store.members(run_group.id)
    print(f"\n━━ RunGroup '{run_group.id}' (one shared domain) ━━")
    print(f"  members       : {', '.join(m.session_id for m in members)}")
    print(f"  tokens spent  : {ledger.tokens_spent}  (scribe's DAG nodes + editor + factchecker, one ledger)")
    print(f"  subagents     : {ledger.subagents_spawned}  (the scribe's workflow nodes count here)")
    print("\nThree peers, one blackboard, one budget — a peer reaction that was a whole DAG charged the "
          "same RunGroup as the reviewers' single turns. Orchestration surfaces compose.")


if __name__ == "__main__":
    asyncio.run(main())
