# L7 · Specialist Brief Pipeline

The single Agent becomes a team of focused specialists. Two researchers work in parallel, a reducer combines their findings, a writer creates the brief, and a verifier checks the result.

## What you learn

| Capability | What to observe |
| --- | --- |
| Specialist Agents | Each workflow node gets a focused goal and role. |
| Data dependencies | A writer receives the output of the merge step, not the entire history of every researcher. |
| Structured output | Schemas validate findings, briefs, and verdicts; a mismatch can be retried. |
| Deterministic reduction | The merge step uses a pure reducer and does not call a model. |
| Verification | The final Agent checks citations and format before the pipeline is complete. |

The same workflow description also supports loops, classification, tournaments, and runtime node submission. The dry run prints those shapes without spending provider tokens.

## Run

```bash
npx tsx 07-brief-pipeline/main.ts
npx tsx 07-brief-pipeline/main.ts --dry-run
python 07-brief-pipeline/main.py
```

## Next

[L8 turns specialists into peers](../08-editorial-room/), with a shared blackboard, reactions, and a workflow inside one peer's turn.
