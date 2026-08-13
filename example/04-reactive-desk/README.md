# L4 · Reactive Assistant

The Agent is no longer limited to its original goal. A webhook, scheduled event, or host note can change what it should pay attention to while it is working.

## What you learn

| Capability | What to observe |
| --- | --- |
| External signals | A gateway delivers a new-source alert to the running Agent. |
| Host notes | The application can inject an urgent editorial note without replacing the session. |
| Attention choices | Normal events wait for the next turn; urgent events can interrupt sooner. |

The example makes both events deterministic by triggering them from tool calls. In production the same paths can be called by a webhook handler, scheduler, or monitor.

## Run

```bash
npx tsx 04-reactive-desk/main.ts
npx tsx 04-reactive-desk/main.ts --dry-run
python 04-reactive-desk/main.py
```

The final brief acknowledges the new source and the editor's note.

## Next

[L5 adds policies and limits](../05-governed-studio/), so the application can control what the Agent may do.
