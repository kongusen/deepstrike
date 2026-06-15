# Dynamic Workflows

> "Claude can now write its own harness on the fly, custom-built for the task at hand."

A **dynamic workflow** is a small harness that spawns and coordinates separate sub-agents — each with its own clean context and a focused goal — instead of planning *and* executing a hard task in one long context window. That single long window reliably hits three failure modes:

- **Agentic laziness** — the model stops after partial progress (20 of 50 items) and calls it done.
- **Self-preferential bias** — it prefers its own results when asked to verify or judge them.
- **Goal drift** — fidelity to the original objective decays across turns, especially after lossy compaction.

In Claude Code that harness is an ephemeral JavaScript file, so its orchestration state isn't replayable, isn't governed, and doesn't cross language boundaries. **DeepStrike makes the harness a kernel primitive.** A workflow does two things — *control flow* (classify / fan-out / loop / barrier / tournament) and *I/O* (run an agent, search the web). DeepStrike puts the control flow in the pure Rust kernel as **scheduling decisions**, and leaves the I/O in your host SDK.

```text
LLM emits a structured plan (WorkflowSpec)
        │
        ▼
deepstrike-core  ──  schedules nodes: gated · budgeted · replayable · resumable · cross-language
        │
        ▼
Host SDK (Node · Python · Rust · WASM)  ──  runs the agents, tools, worktrees, providers, I/O
```

Every node spawn passes the **same syscall gate** as a tool call, so quotas, trust boundaries, and token budgets apply *per node for free*. The orchestration state is serializable, snapshot-restorable, and behaves identically across all four host languages.

## A workflow, end to end

A `WorkflowSpec` is a declarative DAG of nodes. You hand it to the runner; the kernel owns the DAG and gates every spawn, suspends on the join, and advances on completion.

```ts
// One fresh-context verifier per rule (no inherited author context → can't rubber-stamp),
// then a skeptic that reviews their flags to suppress false positives.
const outcome = await runner.runWorkflow({
  nodes: [
    { task: "Rule: money is integer cents — is it violated in the code?", role: "verify" },
    { task: "Rule: all errors propagate — is it violated?",              role: "verify" },
    { task: "Rule: timestamps are UTC — is it violated?",                role: "verify" },
    { task: "Skeptic: of the flags above, which are real violations?",   role: "verify", dependsOn: [0, 1, 2] },
  ],
})
// Kernel spawns the 3 verifiers as one gated batch, suspends on the join,
// then runs the skeptic once they complete — replayable, resumable, audited.
// → { completed: ["wf-node0", "wf-node1", "wf-node2", "wf-node3"], failed: [] }
```

The Python driver is identical in shape (`runner.run_workflow(WorkflowSpec(nodes=[...]))`).

## The six patterns, as first-class kernel nodes

Each composable pattern is a first-class primitive, driven by one workflow executor. A node-spec field selects the control-flow shape (the SDK lowers it to the kernel's serde-tagged `NodeKind`; declaring more than one is a spec error):

| Pattern | Host node-spec field / template | Behavior |
| :--- | :--- | :--- |
| **Spawn** | *(default — no control-flow field)* | Run the node's agent once |
| **Classify-and-act** | `classify: { branches }` | The classifier node's result selects one branch; the others are pruned before they ever run |
| **Fan-out-and-synthesize** | `fanoutSynthesize(workers, synth)` | N parallel read-only workers → a synthesize barrier that waits for all and merges their outputs |
| **Adversarial verification** | `verifyRules(rules, skeptic)` | One fresh-context verifier per rule, each in its own TCB with no inherited author context, so it can't rubber-stamp |
| **Generate-and-filter** | `generateAndFilter(gens, filter)` | N generators → a `Verify` filter / dedupe barrier |
| **Tournament** | `tournament: { entrants }` | A controller node generates N entrants, then runs a pairwise-judge bracket to one winner (comparative judgment beats absolute scoring) |
| **Loop until done** | `loop: { maxIters }` | Re-run until the agent signals it's done (`loopContinue: false`), with a hard `maxIters` backstop |

For the LLM-driven kinds, the SDK runs the node's agent and reads back one additive result signal — a classifier reports `classifyBranch`, a loop iteration reports `loopContinue`, a tournament judge reports `tournamentWinner` — and the kernel uses it to route, stop, or advance the bracket. Each can also carry an `outputSchema` or `modelHint` like any node.

```ts
// loop: re-run "refine" until the agent says it's done, at most 5 times
{ task: "refine the draft", role: "implement", loop: { maxIters: 5 } }
// classify: route to one branch; node 1 runs only if the agent picks "bug", node 2 if "feature"
{ task: "triage this issue", role: "plan",
  classify: { branches: [{ label: "bug", nodes: [1] }, { label: "feature", nodes: [2] }] } }
// tournament: generate 4 ad variants, pairwise-judge to one winner (this goal is the criterion)
{ task: "pick the most persuasive ad", role: "plan",
  tournament: { entrants: ["variant A brief", "variant B brief", "variant C brief", "variant D brief"] } }
```

## Runtime-dynamic: growing the DAG mid-run

A fixed DAG can't express *unknown-size* work — "extract every claim, then verify each one" doesn't know how many verifiers it needs until the extractor runs. The `SubmitNodes` syscall lets a **running node append nodes to the live DAG**.

Give a node the `submitWorkflowNodesTool`; when its agent calls it, the runner routes the new nodes to the parent kernel, which appends them and spawns whatever is ready on the next gated drive. This delivers two shapes the static DAG can't:

- **True loop-until-done** — a coordinator keeps submitting "one more round" until it finds nothing new.
- **Per-item fan-out** — a claim-extractor submits one verifier node per claim it discovered.

Submitted nodes use **batch-relative, backward-only** `depends_on` (a submission can carry its own internal chain), and each appended spawn passes the same quota / depth / quarantine gate as any node — *no new gate*. Submissions are recorded to the session log and replayed on resume, so a workflow that grew at runtime restores exactly.

## Beyond agents: deterministic compute nodes

Not every step needs an LLM. A `NodeKind::Reduce` node runs no agent at all: the kernel schedules it like a spawn but stamps its descriptor with a `reducer` name and the agent ids of its dependencies; the SDK routes it to a **pure registered function** over those dependency outputs and feeds back a synthetic completion — zero tokens, fully deterministic.

Built-in reducers: `dedupe_lines`, `merge_json_arrays`, `concat`, `count`. Register your own through the runner's `reducers` option. This is the "ordinary code between stages" of a script — dedupe / filter / merge — but as a governed, replayable DAG node.

```ts
{ task: "merge findings", role: "implement", reducer: "dedupe_lines", dependsOn: [0, 1, 2] }
```

## Structural answers to the three failure modes

The point of the harness is to defeat the single-context failure modes *by construction*. DeepStrike enforces the mitigations in the kernel:

| Failure mode | Structural answer |
| :--- | :--- |
| **Agentic laziness** | Each node runs in an isolated TCB with its own token budget; a `Loop` node carries an explicit stop condition **and** a hard `maxIters` cap, so "finish all 50" is enforced by structure |
| **Self-preferential bias** | Verifiers and tournament judges run in a separate TCB with no inherited author context; a trust boundary keeps a node from grading its own work |
| **Goal drift** | A durable `task_state` plus a directives channel that survives renewal / compaction — exactly where "don't do X" constraints would otherwise be dropped |

## Trust, schemas, and budgets

Beyond the patterns, four mechanisms make a workflow safe and adaptive:

- **Quarantine, with no escape hatch.** Set `trust: "quarantined"` on a node that reads untrusted public content; a quarantined node that requests write-capable isolation is **denied at the syscall gate** (`NodeTrust`). And because it may have read adversarial content, the topology it asks for is untrusted too: any nodes it submits at runtime are coerced to quarantined (transitive taint), so it can't escape its sandbox by spawning a "trusted" child.
- **Structured output, validated.** Declare an `outputSchema` on a node; the kernel carries it to the spawn descriptor, and the SDK instructs the agent, validates the result against the JSON-Schema subset, and re-runs once with the errors fed back on mismatch. A node that never conforms **fails** (its dependents starve) rather than feeding garbage downstream.
- **Budget as a signal, not just a wall.** Token / node budgets are enforced per spawn (`BudgetLedger`, `max_workflow_nodes`), *and* each spawned node learns its remaining headroom (`WorkflowBudget`), so a coordinator can size its next fan-out to the budget left instead of blindly hitting the cap.
- **Model & intelligence routing.** Every node carries a `model_hint`; a Classify node can research a task and route it to a cheaper or stronger model.

## Why a kernel, not a script

Lifting the control flow into a kernel buys properties an ephemeral JavaScript harness can't have:

- **Replayable** — the control-flow state is a serializable state machine; replay reconstructs a run and strips audit events when rebuilding LLM messages.
- **Governed** — every node spawn flows through the same in-kernel policy as a tool call: quotas, capability checks, trust, vetoes, rate limits, audit.
- **Resumable** — interrupted DAGs restore from the session log / `KernelSnapshot`, **including runtime-appended nodes**, not from scratch (`runner.resumeWorkflow` / `resume_workflow`).
- **Cross-language** — one kernel drives Node, Python, Rust, and WASM hosts with identical semantics.
- **Host-owned I/O** — providers, tools, worktrees, network, and storage stay in your SDK; the kernel only decides *when* and *whether*.

## See also

- SDK drivers: [Node.js SDK](../guides/sdk-nodejs.md#13-动态工作流-dynamic-workflows) · [Python SDK](../guides/sdk-python.md#动态工作流-dynamic-workflows) · per-package `README` **Dynamic workflows** sections.
- [Kernel ABI — Workflow ABI](../reference/kernel-abi.md#workflow-abi-dynamic-workflows) — `load_workflow` / `submit_workflow_nodes` events, the `WorkflowSpec` JSON shape, and workflow observations.
- [Agent OS](./agent-os.md) — the syscall-gate / TCB / process-table substrate the workflow executor is built on.
