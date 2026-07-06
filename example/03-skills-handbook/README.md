# L3 · Skills handbook + Knowledge

L1's agent plus the **capability plane**: capabilities become addressable OS resources that load on
demand and narrow the tool surface, and durable facts get pinned at the front of context.

```
              ┌─ skill catalog (metadata only) ──▶ `skill` meta-tool in every turn
 skillDir ────┤
              └─ skill("citation-style") ─▶ body loads as a tool result
                                          └▶ toolset ← stableCore ∪ allowed_tools   (list_index hidden)

 knowledgeSource ─run start─▶ retrieve(goal) ─▶ pinned into the knowledge slot (front of context)
```

## What you learn here

| Mechanism | Where it shows up |
|---|---|
| **Skill catalog** | `skillDir` scans `skills/*.md`; only each skill's frontmatter (name/description/`allowed_tools`) enters context. The body is **not** loaded until the model calls `skill(name)`. |
| **Tool gating** | While `citation-style` is active the exposed toolset is `stableCore ∪ allowed_tools`. `stableCoreToolIds: ["search","read_source"]` always survive; `list_index` (neither core nor allowed) **disappears** — the model can't wander off-task mid-write. |
| **Gating telemetry** | `onTurnMetrics` reports `toolsExposed` / `activeSkill` per turn. Watch `exposed` drop `6 → 5` the turn `skill=citation-style` appears. |
| **Knowledge partition** | `knowledgeSource.retrieve()` runs once at run start; hits pin into the durable knowledge slot at the front of context — distinct from a skill body (model-loaded, gated, lease-swept) and from memory (recalled, decaying). |

## The three knowledge lifetimes, side by side

This level is where the distinction becomes concrete — all three carry "facts," but they live and die
differently:

| | loaded by | lives in | ends when |
|---|---|---|---|
| **Skill body** | the model (`skill(name)`) | knowledge slot, keyed `skill:<name>` | lease expires / `deactivateSkill` |
| **Knowledge** | the host (`knowledgeSource`) | knowledge slot, pinned | run ends (re-retrieved next run) |
| **Memory** (L2) | run-start recall | decaying history | evicted under pressure |

## Run

```sh
npx tsx 03-skills-handbook/main.ts            # loads the skill, writes a cited brief
npx tsx 03-skills-handbook/main.ts --dry-run  # wiring only
../../python/.venv/bin/python 03-skills-handbook/main.py   # the Python mirror
```

In the live run the agent loads `citation-style`, searches + reads the cache source, cites every
claim through `format_citation`, and closes with a `Sources:` line — while `exposed` shows the
surface narrowed to exactly the tools the task needs.

## What's next

**L4 · Signals + Reactive** opens the agent to the outside world: a `SignalGateway` ingests external
events (a webhook, a cron tick), and the host can `injectNote` mid-run — each drains at a turn
boundary through the kernel's attention policy (queue / soft-interrupt / preempt).
