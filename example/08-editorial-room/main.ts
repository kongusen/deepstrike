/**
 * L8 — Editorial room: the peer-ensemble capstone.
 *
 * Every earlier level scheduled work top-down (one loop, or a DAG the kernel drives). Here several
 * PEER agents share one blackboard and react to each other — the second orchestration surface.
 *
 *   • ReactiveSession — personas subscribe to a shared `EventStream` (blackboard). `emit(event)` runs
 *     a `TurnPolicy` (here `reactByMention` + explicit `audience`) to pick who reacts; each reaction
 *     is a normal `run()` whose continuity comes from that persona's own SessionLog.
 *   • RunGroup — ALL personas run under one cumulative-budget + membership domain. The shared ledger
 *     accrues every persona's (and every sub-agent's) tokens; both peers are recorded as members.
 *   • DAG-in-Peer — a persona's turn body is a seam. The `scribe` overrides `react` to run a whole
 *     WORKFLOW DAG (L7) as its single reaction — and because its runner is wired to the shared
 *     RunGroup, the DAG's node spawns charge the same governance domain. Composition, not a new engine.
 *   • Blackboard read — peers pull what others wrote via the `read_recent` tool (L0/L2 wiring).
 *
 * New mechanisms: ReactiveSession, RunGroup, DAG-in-Peer. Reused: workflow (L7), tools, provider.
 *
 * Run:  npx tsx 08-editorial-room/main.ts        (or --dry-run)
 */
import {
  RuntimeRunner, LocalExecutionPlane, InMemorySessionLog,
  InMemoryGroupBudgetStore, InMemoryEventStream, ReactiveSession, readRecentTool, reactByMention,
} from "@deepstrike/sdk"
import type { RunGroup, BlackboardEvent, WorkflowSpec, SignalSource } from "@deepstrike/sdk"
import { studioTools } from "../shared/studio-tools.js"
import { resolveProvider, parseArgs, loadEnv } from "../shared/provider.js"

// The scribe's reaction IS a workflow DAG: research a source, then write a brief. Runs under the
// shared RunGroup (its node spawns charge the same ledger as the peers).
const SCRIBE_WORKFLOW: WorkflowSpec = {
  nodes: [
    {
      task: "Using ONLY the studio index, read_source 'src-cache' and output ONLY JSON {\"source\":\"src-cache\",\"claim\":\"<one sentence>\"}.",
      role: "custom",
      outputSchema: { type: "object", properties: { source: { type: "string" }, claim: { type: "string" } }, required: ["source", "claim"] },
    },
    {
      task: "Given the JSON finding, write ONE sentence: the claim followed by (src-cache). Output plain text only.",
      role: "implement",
      dependsOn: [0],
    },
  ],
}

async function main(): Promise<void> {
  loadEnv()
  const { flags } = parseArgs(process.argv.slice(2))
  const dryRun = flags["dry-run"] === true

  const store = new InMemoryGroupBudgetStore()
  const runGroup: RunGroup = { id: "editorial-room", budgetStore: store }
  const eventStream = new InMemoryEventStream()

  if (dryRun) {
    console.log("● L8 wiring check (no provider call)")
    console.log(`  run group    : ${runGroup.id}  (shared cumulative budget + membership)`)
    console.log(`  blackboard   : InMemoryEventStream  (personas read via read_recent)`)
    console.log(`  turn policy  : reactByMention + audience`)
    console.log(`  peers        : scribe (DAG-in-Peer → runWorkflow), editor (run()), factchecker (run())`)
    console.log("  ✓ set a key and drop --dry-run to run the room live.")
    return
  }

  const session = new ReactiveSession({
    runGroup,
    turnPolicy: reactByMention(),
    eventStream,
    // Each persona gets its own runner, wired to the SHARED governance + signal routing + blackboard.
    makeRunner: (personaId, shared) => {
      const plane = new LocalExecutionPlane()
      for (const t of studioTools()) plane.register(t)
      plane.register(readRecentTool(shared.eventStream, { personaId }))
      return new RuntimeRunner({
        provider: resolveProvider(),
        executionPlane: plane,
        sessionLog: new InMemorySessionLog(),
        agentId: personaId,
        runGroup: shared.runGroup, // ← the shared domain: every persona's tokens accrue to one ledger
        signalSource: shared.signalSource as SignalSource,
        maxTokens: 200_000,
        maxTurns: 6,
      })
    },
    goalFor: (personaId, event) => {
      const task = typeof event.payload === "string" ? event.payload : JSON.stringify(event.payload)
      if (personaId === "editor")
        return `You are the editor. First call read_recent once to see the latest draft on the blackboard. Then reply with ONE plain-text sentence of concrete feedback — no JSON, no tool syntax in your text. Context: ${task}`
      if (personaId === "factchecker")
        return `You are the fact-checker. First call read_recent once to see the latest draft. Then reply with ONE plain-text sentence stating whether its (src-cache) citation is legitimate — no JSON, no tool syntax in your text. Context: ${task}`
      return `React in one sentence. Context: ${task}`
    },
  })

  // scribe's turn body is a DAG, not a single agent turn (DAG-in-Peer).
  session.addPeer("scribe", {
    role: "writer",
    react: async ({ runner }) => {
      const wf = await runner.runWorkflow(SCRIBE_WORKFLOW)
      return wf.outputs["wf-node1"] ?? "(scribe produced no draft)"
    },
  })
  session.addPeer("editor", { role: "editor" })
  session.addPeer("factchecker", { role: "factchecker" })

  // ── Round 1: the director asks the scribe to draft. Only scribe is mentioned → only scribe reacts.
  console.log("━━ round 1 · director → scribe (a DAG-in-Peer reaction) ━━")
  const r1 = await session.emit({ payload: "scribe, draft a one-sentence brief on prompt caching.", source: "director" })
  const draft = r1.find((r) => r.personaId === "scribe")?.output ?? "(no draft)"
  console.log(`  scribe drafted: ${draft}\n`)

  // Put the draft on the shared blackboard so the reviewers can read_recent it.
  await eventStream.append({ payload: `DRAFT: ${draft}`, source: "scribe" })

  // ── Round 2: ask the reviewers. Both are addressed → both react, reading the blackboard.
  console.log("━━ round 2 · director → editor + factchecker (peers react to the blackboard) ━━")
  const r2 = await session.emit({
    payload: "editor and factchecker, please review the latest draft.",
    source: "director",
    audience: ["editor", "factchecker"],
  })
  for (const r of r2) console.log(`  ${r.personaId}: ${r.output}`)

  // ── The shared governance domain: one ledger, all personas + their sub-agents.
  const ledger = await store.read(runGroup.id)
  const members = await store.members(runGroup.id)
  console.log(`\n━━ RunGroup '${runGroup.id}' (one shared domain) ━━`)
  console.log(`  members       : ${members.map((m) => m.sessionId).join(", ")}`)
  console.log(`  tokens spent  : ${ledger.tokensSpent}  (scribe's DAG nodes + editor + factchecker, all on one ledger)`)
  console.log(`  subagents     : ${ledger.subagentsSpawned}  (the scribe's workflow nodes count here)`)
  console.log(
    "\nThree peers, one blackboard, one budget. The scribe's reaction was an entire workflow DAG, yet " +
      "it charged the same RunGroup as the reviewers' single turns — orchestration surfaces compose.",
  )
}

main().catch((err) => {
  console.error("\n✗", err instanceof Error ? err.message : err)
  process.exitCode = 1
})
