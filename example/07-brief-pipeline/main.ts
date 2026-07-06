/**
 * L7 — Brief pipeline: a dynamic workflow DAG.
 *
 * This stops being one agent. `runner.runWorkflow(spec)` lowers a declarative DAG to governed
 * sub-agent spawns and drives it to completion. This pipeline shows the core node vocabulary:
 *
 *   node 0  research(cache)  ─┐   (spawn: a trusted sub-agent on the parent's plane; outputSchema)
 *   node 1  research(memory) ─┤
 *                             ▼
 *   node 2  merge  (reducer "concat" — a DETERMINISTIC host-computed node, no LLM; dependsOn 0,1)
 *                             ▼
 *   node 3  writer (spawn; dependsOn 2 — the DAG edge carries node 2's OUTPUT as input; outputSchema)
 *                             ▼
 *   node 4  gate   (role "verify"; dependsOn 3 — an eval/harness node that judges the brief; outputSchema)
 *
 * Mechanisms: Workflow DAG · sub-agent spawn + trust/isolation (trusted ⇒ inherit parent plane) ·
 * structured output (`outputSchema`, validate-and-retry) · reducer (host-compute) · data edges
 * (`dependsOn`) · an in-DAG verify/eval gate. Every node spawn passes the one kernel syscall gate.
 *
 * The other node kinds — `loop`, `classify`, `tournament`, plus run-level `Milestones` — are shown
 * structurally in --dry-run and documented in the README (a live tournament fans out many agents).
 *
 * Run:  npx tsx 07-brief-pipeline/main.ts        (or --dry-run)
 */
import { RuntimeRunner, LocalExecutionPlane, InMemorySessionLog } from "@deepstrike/sdk"
import type { WorkflowSpec } from "@deepstrike/sdk"
import { studioTools } from "../shared/studio-tools.js"
import { resolveProvider, parseArgs, loadEnv } from "../shared/provider.js"

// A JSON-Schema-subset the kernel validates each node's output against (and retries once on mismatch).
const FINDING_SCHEMA = {
  type: "object",
  properties: { source: { type: "string" }, claim: { type: "string" } },
  required: ["source", "claim"],
}
const BRIEF_SCHEMA = {
  type: "object",
  properties: { brief: { type: "string" }, sources: { type: "array", items: { type: "string" } } },
  required: ["brief", "sources"],
}
const GATE_SCHEMA = {
  type: "object",
  properties: { pass: { type: "boolean" }, reason: { type: "string" } },
  required: ["pass", "reason"],
}

function researchNode(id: string, topic: string) {
  return {
    task: `Using ONLY the studio index, read_source the source '${id}' and output ONLY a JSON object ` +
      `{"source": "${id}", "claim": "<one-sentence factual claim about ${topic} from that source>"}. No prose.`,
    role: "custom" as const,
    outputSchema: FINDING_SCHEMA,
  }
}

const spec: WorkflowSpec = {
  nodes: [
    researchNode("src-cache", "prompt caching"), // node 0
    researchNode("src-memory", "governed memory writes"), // node 1
    { task: "merge findings", role: "custom", reducer: "concat", dependsOn: [0, 1] }, // node 2 — deterministic
    {
      // node 3 — writer; receives node 2's merged findings as input via the DAG edge.
      task:
        "You are given two JSON findings (one per line) from upstream. Write a two-sentence research " +
        "brief that states both claims and cites each source id in parentheses. Output ONLY JSON " +
        '{"brief": "<two sentences with (src-...) citations>", "sources": ["src-cache", "src-memory"]}.',
      role: "implement",
      outputSchema: BRIEF_SCHEMA,
      dependsOn: [2],
    },
    {
      // node 4 — quality gate (eval/harness as a DAG node): judge the brief, structured verdict.
      task:
        "You are given a brief as JSON. Check it cites BOTH src-cache and src-memory and is two " +
        'sentences. Output ONLY JSON {"pass": <bool>, "reason": "<short>"}.',
      role: "verify",
      outputSchema: GATE_SCHEMA,
      dependsOn: [3],
    },
  ],
}

async function main(): Promise<void> {
  loadEnv()
  const { flags } = parseArgs(process.argv.slice(2))
  const dryRun = flags["dry-run"] === true

  const plane = new LocalExecutionPlane()
  for (const t of studioTools()) plane.register(t)

  if (dryRun) {
    console.log("● L7 wiring check (no provider call)")
    spec.nodes.forEach((n, i) => {
      const kind = n.reducer ? `reduce("${n.reducer}")` : n.classify ? "classify" : n.tournament ? "tournament" : n.loop ? "loop" : "spawn"
      const schema = n.outputSchema ? " +outputSchema" : ""
      const deps = n.dependsOn?.length ? ` ←[${n.dependsOn.join(",")}]` : ""
      console.log(`  node ${i}: ${kind}${schema}${deps}  role=${n.role}`)
    })
    console.log("  also available (see README): loop{maxIters}, classify{branches}, tournament{entrants}, run-level Milestones")
    console.log("  ✓ set a key and drop --dry-run to run the DAG live.")
    return
  }

  const runner = new RuntimeRunner({
    provider: resolveProvider(),
    executionPlane: plane,
    sessionLog: new InMemorySessionLog(),
    maxTokens: 200_000,
    maxTurns: 8,
  })

  console.log("━━ running the brief-pipeline DAG ━━ (2 researchers → reduce → writer → gate)\n")
  const outcome = await runner.runWorkflow(spec)

  console.log(`\n━━ workflow outcome ━━`)
  console.log(`  completed nodes : ${outcome.completed.length}   failed: ${outcome.failed.length}`)
  console.log(`\n  node 2 (reduce) merged findings:\n    ${(outcome.outputs["wf-node2"] ?? "—").replace(/\n/g, "\n    ")}`)
  console.log(`\n  node 3 (writer) brief:\n    ${outcome.outputs["wf-node3"] ?? "—"}`)
  console.log(`\n  node 4 (gate) verdict:\n    ${outcome.outputs["wf-node4"] ?? "—"}`)
  console.log(
    "\nFive nodes, one DAG: two spawns fanned out, a deterministic reducer merged them, a writer " +
      "consumed the merge over a data edge, and a verify node gated the result — each spawn through " +
      "the same kernel syscall, each structured output schema-validated.",
  )
}

main().catch((err) => {
  console.error("\n✗", err instanceof Error ? err.message : err)
  process.exitCode = 1
})
