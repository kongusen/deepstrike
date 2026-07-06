# L2 · Assistant with memory

L1's agent, now given a `DreamStore`. Memory is **keyed per agent, not per session**, so a fact
learned in one session is available in the next.

```
session A  ──research──▶ answer ──┐
                                  ▼
                    runner.writeMemory({content, metadata})   ← the ONE governed write gate
                                  │  (validation · write quota · advisory score · jaccard dedup)
                                  ▼
                          [ DreamStore ]  (keyed by agentId)
                                  │
session B  ──run starts──▶ preQueryMemory recall ──▶ fact injected into history before turn 1
```

## What you learn here

| Mechanism | Where it shows up |
|---|---|
| **Write gate** | `runner.writeMemory(...)` is the single path memories are written through — validation, a rolling-window write quota, an advisory relevance score, and jaccard dedup all live here. The host decides what's worth keeping (here, a research takeaway). |
| **Run-start recall** | `preQueryMemory` (default-on, needs `dreamStore` + `agentId`) searches memory at the start of every run and injects hits into the decaying history, so the model sees prior knowledge on turn one. |
| **On-demand recall** | the `memory` meta-tool appears automatically (store present) so the agent can also query memory mid-run. |

The one new config is `dreamStore` + `agentId` on `RuntimeOptions` — set both and the memory
mechanism turns on. Everything else is L1.

## Run

```sh
npx tsx 02-memory-assistant/main.ts            # runs session A (learn) then session B (recall)
npx tsx 02-memory-assistant/main.ts --dry-run  # wiring only
```

Watch session A search + answer + get written to memory, then session B answer the follow-up
**without searching** — the fact surfaces from run-start recall.

## A note on grounding

The goals say *"using ONLY the studio index, cite the source id."* That phrasing forces the agent to
use its tools instead of answering from the model's own knowledge — a small but load-bearing habit
for every level: a sourced assistant must ground its claims, and grounded goals make the mechanism
being demonstrated actually fire.

## What's next

**L3 · Skills** narrows the toolset: a "citation-style" skill loads on demand through the `skill`
meta-tool, gating which tools are exposed while it's active — the capability plane in action.
