# DEC-13: Model-Led Context

## Status

Accepted

## Date

2026-11-13

## Context

Release 0.2.82 set out to make context management cheaper and more reliable, and paid for live
measurement of every change on DeepSeek (`deepseek-flash`), task by task against a baseline on the
same tasks. Ten changes were measured. They split cleanly into two kinds.

**The kernel judging what content means, by rule or by a call of its own:**

| Change | What the kernel did | Result |
|---|---|---|
| 8.3 / 8.3b / 8.3c | decided what to compact from a cache-cost account | long-horizon: never significantly better; up to 6 of 30 runs at the turn limit |
| 8.4 | asked the host for a model-written summary at compaction time | long-horizon 12/30 → 4/30, cost per correct answer ×5 |
| 9.2 | paged out what notes cited first, deferred compaction with a notice | on top of notes: no gain, cost +31% |
| 9.3 | diagnosed notes each turn for the model to repair | barely fired; no gain |
| 9.4 consolidation | a host-side "dreaming" run reconciling memory after each session | multi-session 33/48 → 33/48, cost per correct answer +272% |

**The model deciding what matters, through a tool, with the kernel keeping it:**

| Change | What the model did | Result (paired) |
|---|---|---|
| 9.1 notes | wrote and rewrote its own notes (a running tally, mostly); the kernel kept them through compaction | long-horizon 15/30 → 20/30, cost per correct answer −43%; ledger 25/30 → 30/30; multi-session 9/48 → 33/48, cost per correct answer −68% |
| 9.4 promotion | proposed what should outlive the task with `propose_memory`; later sessions recalled it | (the multi-session result above) |

Two other findings shaped the measurement itself:

- **Protocol bugs outweighed context policy.** DeepSeek writing tool-call markup as text ended 21
  of 90 runs across three variants; Ollama repeating tool-call ids ended 11 of 14 local runs. Both
  were provider-layer bugs, now fixed with shared fixtures, and both had first been read as context
  failures.
- **The gate measured the wrong thing.** The 0.2.81 default gate compared neither success nor cost
  and passed 8.4. The 8.1 paired gate asked cost to be significantly *lower*, a rule written for
  cost-saving changes; it failed notes in every scenario on one side or the other although notes
  improved success by 17–50 points.

## Decision

### 1. The kernel never initiates a model call to manage context

A model is reached only through `CallProvider`, as a turn of the run. The kernel emits no effect
that asks a host to have a model summarize, judge, rank or consolidate context. What deserves
keeping is the model's call, made with its own tools (`note`, `memory`, `propose_memory`). The
closed effect union is pinned by a test whose failure message points here.

### 2. The model decides what matters; the kernel keeps it

The kernel's part is mechanics: keep what the model wrote verbatim (no compaction tier touches
notes), enforce budgets by refusing a write rather than deleting for the model, record provenance
(`derived_from` edges to the evidence), and render it where the model will see it. The kernel adds
no semantic layer of its own — no rule digests standing in for the model's understanding, no
reordering of what to forget based on what it guesses the model needs.

### 3. A mechanism earns its place, or it is deleted

- It enters behind a switch that is off by default.
- It becomes the default only when, in every scenario it targets, over 30 or more paired tasks, it
  makes success or price-weighted cost per correct answer **significantly better**, and neither
  **significantly worse**. This is the lab's default gate (`deepstrike-lab gate`;
  `DEFAULT_IMPROVE`, `DEFAULT_PAIRED`).
- Otherwise it is deleted — code, switches, contracts, tests and docs. Only its measurement stays,
  in the plan. There are no dormant switches.
- An add-on is judged against the configuration without it, not against the original baseline
  (consolidation passed against "no memory" but failed against "notes").

### 4. Measure with the model, at the price of an answer

- Live, paired, on the same tasks; 10 pairs give a direction, 30 a decision.
- The figure of merit is cost per correct answer, not tokens or cost per task.
- Rule out protocol confounders (leaked markup, repeated ids, dropped tool calls) before reading a
  context result.

## Alternatives Considered

### Keep failed mechanisms behind off-by-default switches

That was 8.4's first disposition. A dormant switch still has to be maintained across four SDKs,
documented and tested, and it invites someone to turn it on without the evidence that removed it.

### Keep the 8.1 gate unchanged

It would have deleted notes, the largest measured gain in the project, on a criterion designed for
a different kind of change.

### Relax the gate to "not significantly worse"

Considered and rejected while drafting this decision: over 30 pairs it would have passed 8.3b
(long-horizon 12/30 → 8/30). Requiring a significant improvement is what makes the rule Occam's
razor rather than a ratchet.

## Consequences

- Notes are on by default (`notes_budget_tokens` = 600): every request carries the `note` tool
  (about 320 tokens of schema) and, once the model writes one, a `notes:` block in the state turn.
  `notesBudgetTokens: 0` / `notes_budget_tokens: 0` turns them off; baselines that reproduce
  pre-0.2.82 behavior must say so.
- The gate's default changed after the 0.2.82 data was seen. That is recorded here and in
  `DEFAULT_PAIRED`'s documentation, and the new rule was checked against every earlier decision:
  it rejects 8.3, 8.3b, 8.4 and consolidation and accepts notes, as the decisions did.
- Mechanisms that predate this decision — the rule summarizer, narration collapse, utility-scored
  archiving — have not been measured under it. They are candidates for the same test, not exempt
  from it.
