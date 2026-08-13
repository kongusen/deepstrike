# L2 · Memory Assistant

The L1 Agent now learns across sessions. It stores useful research facts and recalls them before the next question.

## What you learn

| Capability | What to observe |
| --- | --- |
| Durable memory | A fact written in one session is available in the next. |
| Recall | Run-start recall gives the Agent relevant context before its first turn. |
| Memory quality | Validation, quotas, relevance checks, and deduplication keep the store useful. |

## Run

```bash
npx tsx 02-memory-assistant/main.ts
npx tsx 02-memory-assistant/main.ts --dry-run
python 02-memory-assistant/main.py
```

Watch session A learn a source fact and session B answer a follow-up without searching again.

## Design note

Memory is for durable facts and preferences. The session transcript remains the record of what happened in a particular run; memory is the smaller set of information worth carrying forward.

## Next

[L3 adds skills and knowledge](../03-skills-handbook/), so the Agent can load specialized instructions only when a task needs them.
