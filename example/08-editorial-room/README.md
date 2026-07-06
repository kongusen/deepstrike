# L8 · Editorial room — the peer-ensemble capstone

Every earlier level scheduled work **top-down**: one loop, or a DAG the kernel drives. L8 is the
**second orchestration surface** — several peer agents share a blackboard and react to *each other*,
under one governance budget. And a peer's turn can itself be a whole DAG, so the two surfaces compose.

```
        ┌──────────────── RunGroup "editorial-room" (one cumulative budget + membership) ───────────────┐
        │                                                                                                │
 director ─emit─▶ [ blackboard / EventStream ] ◀─read_recent─ editor, factchecker                        │
                        │  reactByMention + audience picks who reacts                                    │
                        ▼                                                                                 │
                  scribe.react = runWorkflow(DAG)   ← DAG-in-Peer: a peer's turn IS a workflow (L7)       │
                        │  its node spawns charge THIS SAME RunGroup                                      │
        └────────────────────────────────────────────────────────────────────────────────────────────┘
```

## What you learn here

| Mechanism | Where it shows up |
|---|---|
| **ReactiveSession** | Personas subscribe to a shared `EventStream`. `session.emit(event)` runs a `TurnPolicy` (`reactByMention` + `audience`) to pick reactors; each reaction is a normal `run()` with the persona's own SessionLog for continuity. |
| **RunGroup** | One `RunGroup { id, budgetStore }` passed to every persona's runner. The shared ledger accrues **every** persona's tokens; `store.members()` records all of them (plus sub-agents) as lineage. |
| **DAG-in-Peer** | `scribe` overrides `react` to call `runner.runWorkflow(...)` — its whole turn is the L7 pipeline. Because its runner carries the shared `runGroup`, the DAG's node spawns charge the **same** domain (see `subagents: 2` and the `wf-node*` members). |
| **Blackboard read** | Reviewers pull what the scribe wrote via the `read_recent` tool (`readRecentTool(eventStream, {personaId})`), respecting per-event visibility (channel / audience). |
| **Turn policy** | `reactByMention` selects a peer when its id or role appears in the payload, or when it's in the event `audience`. Round 1 mentions only `scribe`; round 2 addresses both reviewers. |

## The composition that matters

The load-bearing line is `scribe.react = async ({ runner }) => runner.runWorkflow(SCRIBE_WORKFLOW)`.
A peer reaction (surface #2) contains a workflow DAG (surface #1), and the RunGroup ledger proves
they share **one** governance domain: the scribe's two `wf-node*` children show up as members and in
`subagents`, and their tokens sum into the same `tokensSpent` as the reviewers' single turns. No new
mechanism was added to compose them — they meet at the shared floor (RunGroup + signal routing).

## Run

```sh
npx tsx 08-editorial-room/main.ts            # Node
python ../python/.venv/bin/python main.py    # Python mirror (from this dir; see below)
npx tsx 08-editorial-room/main.ts --dry-run  # wiring only
```

Round 1: the `scribe` drafts via its DAG-in-Peer. Round 2: the `editor` and `factchecker`
`read_recent` the draft and each reply with one sentence. Finally the `RunGroup` ledger prints one
shared budget across all three peers *and* the scribe's workflow nodes.

## Python mirror

`main.py` is the same room in the Python SDK — `ReactiveSession`, `RunGroup`, `InMemoryEventStream`,
`read_recent_tool`, and a `react` override that calls `runner.run_workflow(...)`. It is the second of
the two levels (with L1) mirrored to Python, to show the peer + workflow surfaces are cross-language.

## That's the curriculum

L1→L8 walked one agent (tools → memory → skills → signals → governance → loop) up to many agents
(workflow DAG → reactive peers). Every mechanism in the framework appeared at least once, each on the
same small Research Brief Studio domain. See the top-level [README](../README.md) for the full map.
