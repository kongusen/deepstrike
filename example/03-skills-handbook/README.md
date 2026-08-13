# L3 · Skill-based Assistant

The Agent now has a small handbook of reusable skills. It loads the citation skill on demand, receives the relevant knowledge, and works with a task-specific tool set.

## What you learn

| Capability | What to observe |
| --- | --- |
| Skill catalog | The Agent sees available skill summaries without loading every skill body. |
| On-demand knowledge | `citation-style` is loaded only when the task calls for it. |
| Focused tools | An active skill can narrow the tools exposed during its phase. |

The example skill lives in [`skills/citation-style.md`](./skills/citation-style.md).

## Run

```bash
npx tsx 03-skills-handbook/main.ts
npx tsx 03-skills-handbook/main.ts --dry-run
python 03-skills-handbook/main.py
```

The live run produces a cited brief and reports which tools were exposed before and after the skill loaded.

## Next

[L4 adds external signals](../04-reactive-desk/), allowing a running Agent to respond to a webhook, schedule, or host note.
