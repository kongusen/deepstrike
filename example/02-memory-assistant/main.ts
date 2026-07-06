/**
 * L2 — Assistant with memory.
 *
 * The same sourced-Q&A agent from L1, now given a `DreamStore`. Two things change:
 *   • RECALL — at the start of every run the runner recalls relevant memories (`preQueryMemory`,
 *     default-on) and injects them into the decaying history, so the model sees prior knowledge on
 *     turn one; the agent can also query memory on demand via the `memory` meta-tool.
 *   • WRITE  — persisting a memory goes through ONE governed gate, `runner.writeMemory(...)`:
 *     validation + a rolling-window write quota + an advisory score (a near-duplicate write is also
 *     subject to jaccard dedup at this gate). The host decides what is worth keeping — here, the
 *     takeaway from a research run.
 *
 * This example runs TWO sessions in one process under the SAME agentId + store:
 *   session A ("learn")  — research a topic; the host persists the takeaway through the write gate
 *   session B ("recall") — a fresh session id asks a follow-up; the fact is recalled at run-start
 *
 * New mechanism: Memory. Reused: tools, execution plane, provider, session log (L1).
 *
 * Run:  npx tsx 02-memory-assistant/main.ts        (or --dry-run)
 */
import { RuntimeRunner, LocalExecutionPlane, InMemorySessionLog } from "@deepstrike/sdk"
import type { TextDelta } from "@deepstrike/sdk"
import { InMemoryDreamStore } from "@deepstrike/sdk/memory"
import { studioTools } from "../shared/studio-tools.js"
import { resolveProvider, parseArgs, loadEnv } from "../shared/provider.js"
import { render } from "../shared/render.js"

const AGENT_ID = "studio-researcher"

async function main(): Promise<void> {
  loadEnv()
  const { flags } = parseArgs(process.argv.slice(2))
  const dryRun = flags["dry-run"] === true

  const plane = new LocalExecutionPlane()
  for (const t of studioTools()) plane.register(t)
  // One store shared by both sessions. A memory written in session A is recalled in session B.
  const dreamStore = new InMemoryDreamStore()

  if (dryRun) {
    console.log("● L2 wiring check (no provider call)")
    console.log(`  agent id : ${AGENT_ID}  (memory is keyed per agent, not per session)`)
    console.log(`  store    : InMemoryDreamStore  → run-start recall + the 'memory' query tool turn on`)
    console.log(`  write    : runner.writeMemory({content, metadata})  → the one governed gate`)
    console.log("  ✓ configure dreamStore + agentId and the memory mechanism turns on.")
    return
  }

  const runner = new RuntimeRunner({
    provider: resolveProvider(),
    executionPlane: plane,
    sessionLog: new InMemorySessionLog(),
    dreamStore,
    agentId: AGENT_ID, // memory requires BOTH dreamStore and agentId
    maxTokens: 200_000,
    maxTurns: 12,
  })

  // ── Session A: research; capture the takeaway ────────────────────────────────
  console.log("━━ session A · learn ━━ (research a topic; the answer becomes a memory)\n")
  let takeaway = ""
  for await (const event of runner.run({
    sessionId: "l2-learn",
    goal:
      "Using ONLY the studio index (do not answer from prior knowledge): search for the source about " +
      "loop agents, read it, then answer in ONE sentence with its source id in parentheses.",
  })) {
    if (event.type === "text_delta") takeaway += (event as TextDelta).delta
    render(event)
  }
  takeaway = takeaway.trim()

  // ── Write through the one governed gate ──────────────────────────────────────
  const now = Date.now()
  await runner.writeMemory({
    content: takeaway,
    metadata: {
      name: "loop-agent-takeaway",
      description: "One-sentence definition of a loop agent, learned in session A.",
      kind: "reference",
      created_at: now,
      updated_at: now,
    },
  })

  const stored = await dreamStore.loadMemories(AGENT_ID)
  console.log(`\n━━ long-term memory now holds ${stored.length} entry(ies) (via the writeMemory gate) ━━`)
  for (const m of stored) console.log(`  • [score ${m.score.toFixed(2)}] ${m.text}`)

  // ── Session B: recall (fresh session id, same agent + store) ─────────────────
  console.log("\n━━ session B · recall ━━ (a NEW session; the fact surfaces at run-start)\n")
  for await (const event of runner.run({
    sessionId: "l2-recall",
    goal: "Without searching again, what did we already learn about how a loop agent works?",
  })) {
    render(event)
  }
  console.log(
    "\nThe answer came from recalled memory, not a fresh search — run-start recall injected the " +
      "session-A takeaway into session B's history before turn one.",
  )
}

main().catch((err) => {
  console.error("\n✗", err instanceof Error ? err.message : err)
  process.exitCode = 1
})
