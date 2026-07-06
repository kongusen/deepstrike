# L7 · Brief pipeline — a dynamic workflow DAG

Levels 1–6 were one agent. Here `runner.runWorkflow(spec)` lowers a **declarative DAG** to governed
sub-agent spawns and drives it to completion — orchestration as data, not as hand-written glue.

```
 node0 research(src-cache)  ─┐  spawn + outputSchema
 node1 research(src-memory) ─┤
                             ▼
 node2 merge  reducer:"concat"  ← deterministic host-compute, NO llm (dependsOn 0,1)
                             ▼
 node3 writer  spawn + outputSchema  (dependsOn 2 — the edge carries node2's OUTPUT as input)
                             ▼
 node4 gate    role:"verify" + outputSchema  ← an eval/harness node, in the DAG (dependsOn 3)
```

## What you learn here

| Mechanism | Where it shows up |
|---|---|
| **Workflow DAG** | `WorkflowSpec = { nodes: [...] }`; `runner.runWorkflow(spec)` returns `{ completed, failed, outputs }` (outputs keyed by node id `wf-node{N}`). |
| **Sub-agent spawn + trust** | Each spawn node runs a child agent. A **trusted** node inherits the parent's execution plane (so it has `search`/`read_source`); a `trust: "quarantined"` node runs deny-all — untrusted content can't touch tools. |
| **Structured output** | `outputSchema` (a JSON-Schema subset) is carried to the spawn; the runner instructs the agent to emit conforming JSON and **validates + retries once** on mismatch. All four LLM nodes here are schema-typed. |
| **Reducer (host-compute)** | Node 2 has `reducer: "concat"` and runs **no LLM** — the runner routes it to a named pure function over its `dependsOn` outputs. Built-ins: `concat`, `dedupe_lines`, `merge_json_arrays`; add your own via the `reducers` option. |
| **Data edges** | `dependsOn: [2]` doesn't just order — node 3 **receives node 2's output** as input. A DAG edge carries data. |
| **Eval gate in-DAG** | Node 4 (`role: "verify"`) judges the brief and emits a structured verdict — a harness/eval step expressed as an ordinary node. |

Every node spawn passes the one kernel syscall gate (quota, quarantine, per-node caps from L5).

## The rest of the node vocabulary

The same `WorkflowNodeSpec` also expresses control flow — swap the node body:

```ts
{ task: "refine draft", role: "implement", loop: { maxIters: 3 } }   // re-run until loopContinue:false
{ task: "route the request", role: "plan",                           // classify: one branch runs, rest pruned
  classify: { branches: [{ label: "bug", nodes: [1] }, { label: "feature", nodes: [2] }] } }
{ task: "pick the best angle", role: "plan",                          // tournament: N entrants, pairwise-judged
  tournament: { entrants: [{ goal: "angle A" }, { goal: "angle B" }, { goal: "angle C" }] } }
```

`--dry-run` prints the kind of each node in this spec. A live `tournament` fans out many agents, so
this level keeps its live run to the five-node pipeline; the shapes above lower identically.

### Run-level Milestones

Milestones gate a **single run's phases** (orthogonal to the DAG). A `MilestoneContract` declares
phases; `milestonePolicy` decides what happens when a phase needs evaluation; `onMilestoneEvaluate`
is the host verifier:

```ts
new RuntimeRunner({
  ...,
  milestoneContract: { phases: [{ id: "draft", ... }, { id: "review", ... }] },
  milestonePolicy: "evaluate",
  onMilestoneEvaluate: async ({ phase }) => ({ pass: phase === "draft", feedback: "…" }),
})
```

## Run

```sh
npx tsx 07-brief-pipeline/main.ts            # runs the 5-node DAG live
npx tsx 07-brief-pipeline/main.ts --dry-run  # prints the node kinds, no provider call
```

You'll see all five nodes complete: two research spawns emit schema-valid findings, the reducer
merges them, the writer produces a cited brief, and the verify gate returns `{"pass": true}`.

## What's next

**L8 · Editorial room** is the capstone: several *peer* agents share one blackboard via
`ReactiveSession`, governed by a cumulative `RunGroup` budget — the second orchestration surface,
where agents react to each other rather than being scheduled by a DAG. (Node + Python mirror.)
