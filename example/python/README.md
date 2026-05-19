# MeetingMind

AI Meeting Intelligence — paste a transcript, get structured action items, decisions, and weekly digests. Built on [`deepstrike`](https://pypi.org/project/deepstrike/).

## What it does

| Input | Output |
|---|---|
| Raw meeting transcript (text/paste/file) | Structured action items with owners & deadlines |
| Multi-meeting history | Cross-meeting weekly digest |
| Person name | Standup update for that person |
| Open action list | Progress report by project |

### Knowledge Flywheel

```text
transcript → RuntimeRunner extracts structure → stored in meetings/ + actions/
                                              ↓
                              KnowledgeSource feeds past context into next run
                                              ↓
                              DreamStore distills patterns (recurring blockers,
                              who takes what, team velocity)
                                              ↓
                              weekly digest + standup answers improve over time
```

## SDK modules exercised

| Module | Where used |
|---|---|
| `RuntimeRunner` | `meetingmind/agent.py` — extract mode (5 turns) and report mode (15 turns) |
| `OpenAIProvider` + `CircuitBreaker` | `meetingmind/provider.py` |
| `@tool` | 5 tools: `search_meetings`, `list_actions`, `update_action`, `export_data`, `web_fetch` |
| `SkillRegistry` | 6 skills: `extract_actions`, `identify_decisions`, `detect_blockers`, `generate_standup`, `weekly_summary`, `assign_owners` |
| `EvalLoopHarness` | `harness/extract_judge.py` — validates extraction has actions + decisions + summary |
| `HarnessLoop` | `harness/report_judge.py` — LLM-as-judge ensures weekly reports are substantive |
| `KnowledgeSource` | `knowledge/meeting_source.py` — injects recent meetings into agent context |
| `DreamStore` | `memory/dream_store.py` — file-based; learns recurring patterns across sessions |
| `Governance` | `governance/policy.py` — blocks `update_action` in read-only mode |
| `SignalGateway` | `signals/inbox_watcher.py` — polls `inbox/` for new `.txt` transcript files |

## Install

```bash
cd example/python
python3 -m venv .venv && source .venv/bin/activate
pip install -e .
cp .env.example .env   # fill in OPENAI_API_KEY
```

## Run — CLI

```bash
python main.py
```

```text
MeetingMind — AI Meeting Intelligence
Type /help for commands

> /process
Paste transcript (end with a line containing only "---"):
Alice: Let's ship the new auth flow by Friday.
Bob: I'll handle the backend, needs review from Carol.
Carol: I can review Thursday. Also we need to decide on the token expiry.
Alice: Let's go with 7 days.
---

── extracting ──────────────────────────────────────────────────────
✓ skill: extract_actions
✓ skill: identify_decisions
✓ skill: assign_owners

✓ saved → store/meetings/mtg_20260513_143201_a3f8c2.json
  actions  : 2 open
  decisions: 1
  blockers : 0

> /actions
[ ] act_20260513_143201_b1d4e7  auth flow backend (Bob) — due Fri
[ ] act_20260513_143201_c9a1f3  code review auth PR (Carol) — due Thu

> /done act_20260513_143201_b1d4e7
✓ marked done

> /standup Bob
── standup: Bob ────────────────────────────────────────────────────
**Yesterday**: completed auth flow backend, ready for review
**Today**: awaiting Carol's review on auth PR
**Blockers**: none

> /weekly
── weekly report ───────────────────────────────────────────────────
# Week of 2026-05-13

## Meetings this week (1)
- 2026-05-13: Auth & token expiry discussion

## Decisions made
- Token expiry set to 7 days

## Action velocity
- 2 assigned · 1 completed · 1 open

## Open items
- [ ] Code review auth PR (Carol) — due Thu

> /stop
✓ dream complete: +3 memories, 1 session processed
```

### CLI commands

```text
/process                        paste transcript interactively
/process <file.txt>             process a transcript file
/actions [--project <name>]     list open action items
/done <action_id>               mark action as done
/blocked <action_id>            mark action as blocked
/standup [person]               standup update for a person
/weekly [project]               weekly digest (all or by project)
/search <query>                 search past meetings
/export [md|json]               export all data
/stop                           save memory and exit
/help                           show this help
```

## Run — Web UI

```bash
python server.py
# open http://localhost:3000
```

The web UI provides:

- **Paste panel** — drop in a raw transcript, pick project/participants, stream the extraction in real time
- **Actions tab** — all open items with checkboxes; clicking marks done via API
- **Meetings grid** — clickable cards showing summary, participants, decision count, action count
- **Weekly report modal** — one-click digest generation, streamed back via SSE

## Project layout

```text
example/python/
├── pyproject.toml
├── .env.example
├── main.py                        # CLI entry point
├── server.py                      # aiohttp web server (SSE)
├── public/
│   └── index.html                 # single-file UI
├── skills/
│   ├── extract_actions.md
│   ├── identify_decisions.md
│   ├── detect_blockers.md
│   ├── generate_standup.md
│   ├── weekly_summary.md
│   └── assign_owners.md
└── meetingmind/
    ├── paths.py
    ├── types.py                   # MeetingRecord, ActionItem, Decision
    ├── store.py                   # save/load meetings + actions
    ├── provider.py
    ├── agent.py
    ├── governance/
    │   └── policy.py
    ├── memory/
    │   └── dream_store.py
    ├── knowledge/
    │   └── meeting_source.py
    ├── harness/
    │   ├── extract_judge.py       # EvalLoopHarness
    │   └── report_judge.py        # HarnessLoop (LLM-as-judge)
    ├── signals/
    │   ├── cli_bridge.py
    │   └── inbox_watcher.py
    └── tools/
        ├── search_meetings.py
        ├── list_actions.py
        ├── update_action.py
        ├── export_data.py
        └── web_fetch.py
```

## Environment variables

| Variable | Default | Notes |
|---|---|---|
| `OPENAI_API_KEY` | — | Required |
| `MODEL` | `gpt-4o` | Any OpenAI-compatible model |
| `OPENAI_BASE_URL` | OpenAI default | Override for local/proxy |
| `TAVILY_API_KEY` | — | Better web search in `/search` |
| `JINA_API_KEY` | — | Better URL content fetching |
| `PORT` | `3000` | Web server port |

## Data storage

All data is local files, no database required:

```text
store/
  meetings/   *.json   one file per meeting
  actions/    *.json   one file per action item
output/
  memory/flashnote/
    sessions/           raw session logs (for DreamStore)
    memories.json       distilled long-term patterns
  exports/    *.md / *.json
```
