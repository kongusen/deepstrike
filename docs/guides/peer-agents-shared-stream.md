# Peer agents over a shared event stream

For a **long-lived set of equal-status agent personas** that each observe one shared, append-only
event stream and decide when to act — a multi-NPC live session, a debate, a classroom, a simulation —
use a **`ReactiveSession`**. It is the blessed pattern for "N peer agents over a shared blackboard",
and it composes the framework's lower layers so you don't hand-roll turn-taking, governance, or
signal routing.

This is **not** `spawnSubAgent` (there is no persistent parent — personas are peers, activated
statelessly) and **not** a single `runWorkflow` DAG (the personas are long-lived and event-driven,
not a one-shot graph).

## The shape

A `ReactiveSession` ties together:

| Piece | Layer | Role |
|-------|-------|------|
| `RunGroup` | L1 | One governance domain across all personas — cumulative token/spawn budget + lineage that span the logical run, even across stateless invocations and replicas. |
| `EventStream` (blackboard) | L2 | The shared append-only stream every persona observes. Pluggable storage; default in-memory. Optional `channel` / `audience` tags scope visibility. |
| `TurnPolicy` | L2 | **The one thing you customize:** who reacts to each event. Built-in: `reactByMention`, `directorDriven`, `roundRobin`, composed with `firstNonEmpty` / `union`. |
| `SignalGateway` | L0 | Targeted `interrupt(personaId, …)` (only that persona's loop preempts) and `broadcast(…)`. |

Each persona is its own `RuntimeRunner` session (own `sessionId` + `SessionLog`); continuity comes
from the log, so `emit` can run inside a stateless HTTP handler and `ReactiveSession.resume` rebuilds
the peer set from the persisted `RunGroup` membership — no hot in-process loop.

## Minimal example (Node)

```ts
import {
  ReactiveSession, RuntimeRunner, LocalExecutionPlane, InMemorySessionLog,
  InMemoryGroupBudgetStore, InMemoryEventStream, readRecentTool,
  reactByMention, firstNonEmpty, directorDriven,
} from "@deepstrike/sdk"

const budgetStore = new InMemoryGroupBudgetStore()      // swap for a Postgres-backed store in prod
const eventStream = new InMemoryEventStream()           // the shared blackboard
const runGroup = { id: "scenario-42", budgetStore }     // one governance domain for the whole run

const session = new ReactiveSession({
  runGroup,
  eventStream,
  // Director decides who responds; fall back to @-mention addressing.
  turnPolicy: firstNonEmpty(
    reactByMention(),
    directorDriven("director", async (ev, peers) => /* an LLM call, or your rule */ ["seller"]),
  ),
  // You own provider / tools; spread `shared` to wire governance + signal routing in.
  makeRunner: (personaId, shared) => {
    const plane = new LocalExecutionPlane()
    plane.register(readRecentTool(shared.eventStream, { personaId }))  // persona reads the blackboard
    return new RuntimeRunner({
      provider: myProvider,
      sessionLog: new InMemorySessionLog(),     // a persistent SessionLog in prod
      executionPlane: plane,
      maxTokens: 8192,
      maxTotalTokens: 200_000,                  // run-level cap, enforced across ALL personas
      agentId: personaId,
      runGroup: shared.runGroup,                // share the governance domain
      signalSource: shared.signalSource,        // pull only this persona's + broadcast signals
    })
  },
})

session.addPeer("director", { role: "director" })
session.addPeer("seller", { role: "seller", channels: ["deal-room"] })
session.addPeer("coach", { role: "coach" })

// A learner message arrives (stateless handler): append to the blackboard, let the policy pick
// reactors, drive their turns under the shared budget, and return what they said.
const reactions = await session.emit({ payload: "I'd like a better price.", source: "learner" })
// → e.g. [{ personaId: "seller", output: "…counteroffer…" }]

// Targeted nudge — only the seller's in-flight turn is preempted:
await session.interrupt("seller", { payload: { directive: "hold firm on price" } })
```

The same shape works in Python — `ReactiveSession`, `InMemoryEventStream`, `react_by_mention`,
`read_recent_tool`, etc. are exported from `deepstrike`.

## What you customize vs. what you get

- **You customize** the `TurnPolicy` ("who reacts") and `makeRunner` (your provider / tools). Turn
  selection is deliberately yours — the framework provides the spanning default set and combinators,
  not a fixed policy.
- **You get** for free: one shared governance domain (a run-level token/spawn cap enforced across
  every persona, not per-persona), cross-persona lineage that survives restarts, blackboard
  visibility scoping (`channel` / `audience`), and targeted vs. broadcast signal routing.

## Going stateless / multi-replica

Back the two seams with durable stores and the whole logical run survives process boundaries:

- `SessionLogGroupBudgetStore(sessionLog)` persists the budget ledger **and** membership (lineage) by
  folding group-anchor events on read — a fresh replica rebuilds the same governance state.
- A persistent `EventStream` (your DB) makes the blackboard durable.

Then each external event is one `session.emit(...)` in a stateless handler, and
`ReactiveSession.resume({ runGroup, turnPolicy, makeRunner })` reconstructs the peer set from the
persisted membership. No long-lived loop is required.

## See also

- [collaboration.md](collaboration.md) — parent→sub-agent shapes (`AgentPool`, `HandoffBus`) for the
  hierarchical case; use `ReactiveSession` instead when personas are equal-status peers.
- [dynamic-workflows.md](../concepts/dynamic-workflows.md) — one-shot DAG orchestration.
