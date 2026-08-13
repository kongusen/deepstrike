# L6 · Long-running Digest Agent

The Agent now works in bounded rounds. It decides whether to continue, sleep, or stop, while the application keeps the session available for a later wake-up.

## What you learn

| Capability | What to observe |
| --- | --- |
| Continuity | Every round sees the digest written by earlier rounds. |
| Self-pacing | The Agent chooses `continue`, `sleep`, or `stop` after each round. |
| Completion checks | A verdict function can reject an early stop and provide feedback for the next round. |
| Dormant sessions | A sleeping run can hand its wake-up to an external scheduler. |

## Run

```bash
npx tsx 06-daily-digest/main.ts
npx tsx 06-daily-digest/main.ts --dry-run
python 06-daily-digest/main.py
```

You should see the digest grow one source per round and stop only after the completion check passes or the round limit is reached.

## Next

[L7 introduces specialist Agents](../07-brief-pipeline/), with parallel research, structured output, deterministic merging, and verification.
