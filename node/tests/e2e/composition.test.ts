/**
 * MECHANISM TEST — organic composition of the three orchestration mechanisms.
 *
 * Goal: empirically decide Tier 0 (narrative/docs only) vs Tier 1 (recursive body-kind) from
 * `.local-docs/specs/orchestration-composition-5why.md`, against a LIVE model (deepseek/minimax).
 *
 * The decisive question: does ONE shared governance domain (`RunGroup`) actually span all three
 * mechanisms today — the `run()`/agent path, the `runWorkflow()` DAG path, and the `ReactiveSession`
 * peer path — so that one cumulative budget caps the whole composite tree?
 *
 * We wire ONE RunGroup `g` and drive each mechanism under it, reading the shared budget ledger
 * before/after each. A mechanism "joins the governance domain" iff its spend lands in `g`'s ledger.
 *
 * Run with:
 *   set -a; source .env; set +a; E2E_PROVIDER=deepseek npx jest e2e/composition --testTimeout 300000
 * Skips cleanly when no provider key is present.
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane, collectText } from "../../src/index.js"
import { InMemoryGroupBudgetStore } from "../../src/runtime/run-group.js"
import type { RunGroup } from "../../src/runtime/run-group.js"
import { ReactiveSession, readRecentTool } from "../../src/runtime/reactive-session.js"
import type { WorkflowSpec } from "../../src/index.js"
import { loadProviders, anyProvider } from "./providers.js"

const provider = anyProvider(loadProviders())
const maybe = provider ? describe : describe.skip

maybe("composition mechanism test (live model)", () => {
  it("probes whether one RunGroup spans run() / runWorkflow() / ReactiveSession", async () => {
    const store = new InMemoryGroupBudgetStore()
    const g: RunGroup = { id: "compose-probe", budgetStore: store }
    const led = () => store.read(g.id) // {tokensSpent, subagentsSpawned}

    const baseOpts = {
      provider: provider!,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 4000,
      maxTurns: 2,
      runGroup: g,
    }

    // ── Probe 1: the run()/agent path under group g ────────────────────────────
    const before1 = led()
    const r1 = new RuntimeRunner({ ...baseOpts, sessionLog: new InMemorySessionLog(), agentId: "agent-A" })
    await collectText(r1.run({ sessionId: "agent-A", goal: "Reply with exactly one word: PING" }))
    const after1 = led()
    const d1 = after1.tokensSpent - before1.tokensSpent
    console.log(`\n[P1 run()]        ledger Δtokens=${d1}  Δspawns=${after1.subagentsSpawned - before1.subagentsSpawned}`)

    // ── Probe 2: the standalone runWorkflow() DAG path under the SAME group g ───
    const before2 = led()
    const r2 = new RuntimeRunner({ ...baseOpts, sessionLog: new InMemorySessionLog(), agentId: "wf-runner" })
    const spec: WorkflowSpec = {
      nodes: [
        { task: "Reply with exactly one word: APPLE", role: "explore" },
        { task: "Reply with exactly one word: BANANA", role: "explore" },
      ],
    }
    const wf = await r2.runWorkflow(spec)
    const after2 = led()
    const d2 = after2.tokensSpent - before2.tokensSpent
    const d2spawns = after2.subagentsSpawned - before2.subagentsSpawned
    console.log(`[P2 runWorkflow()] completed=${wf.completed.length} failed=${wf.failed.length}  ledger Δtokens=${d2}  Δspawns=${d2spawns}  (gap-a fix: nodes now counted)`)

    // ── Probe 3: the ReactiveSession (L2) peer path under the SAME group g ──────
    const before3 = led()
    const session = new ReactiveSession({
      runGroup: g,
      // All visible peers react (2 peers, 1 emit → cheap).
      turnPolicy: (_e, peers) => peers.map(p => p.personaId),
      makeRunner: (personaId, shared) =>
        new RuntimeRunner({
          provider: provider!,
          sessionLog: new InMemorySessionLog(),
          executionPlane: (() => {
            const plane = new LocalExecutionPlane()
            plane.register(readRecentTool(shared.eventStream, { personaId }))
            return plane
          })(),
          maxTokens: 4000,
          maxTurns: 2,
          runGroup: shared.runGroup,
          signalSource: shared.signalSource,
          agentId: personaId,
        }),
      goalFor: (personaId) => `You are ${personaId}. Reply with exactly one word: ACK`,
    })
    session.addPeer("peer-1", { role: "explore" })
    session.addPeer("peer-2", { role: "explore" })
    const reactions = await session.emit({ payload: { msg: "ping the team" }, source: "director" })
    const after3 = led()
    const d3 = after3.tokensSpent - before3.tokensSpent
    console.log(`[P3 ReactiveSession] reactions=${reactions.length}  ledger Δtokens=${d3}  Δspawns=${after3.subagentsSpawned - before3.subagentsSpawned}`)
    console.log(`[P3 members] ${(await store.members(g.id)).map(m => m.sessionId).join(", ")}`)

    // ── Probe 4 (gap-b fix): DAG-in-Peer — a peer's turn IS a workflow, via the `react` seam ───
    const before4 = led()
    const dagSession = new ReactiveSession({
      runGroup: g,
      turnPolicy: (_e, peers) => peers.map(p => p.personaId),
      makeRunner: (personaId, shared) =>
        new RuntimeRunner({
          provider: provider!,
          sessionLog: new InMemorySessionLog(),
          executionPlane: new LocalExecutionPlane(),
          maxTokens: 4000,
          maxTurns: 2,
          runGroup: shared.runGroup,
          agentId: personaId,
        }),
    })
    // This peer reacts by driving a 2-node DAG (not a single run()). Its runner carries runGroup g,
    // so the workflow nodes charge the shared domain (gap-a + gap-b compose).
    dagSession.addPeer("designer", {
      role: "plan",
      react: async ({ runner, personaId }) => {
        const dag: WorkflowSpec = {
          nodes: [
            { task: "Reply with exactly one word: RED", role: "explore" },
            { task: "Reply with exactly one word: BLUE", role: "explore" },
          ],
        }
        const out = await runner.runWorkflow(dag, { sessionId: `${personaId}-wf` })
        return `ran DAG: ${out.completed.length} nodes`
      },
    })
    const dagReactions = await dagSession.emit({ payload: { msg: "design something" }, source: "director" })
    const after4 = led()
    const d4spawns = after4.subagentsSpawned - before4.subagentsSpawned
    console.log(`[P4 DAG-in-Peer]  reaction="${dagReactions[0]?.output}"  ledger Δspawns=${d4spawns}  (gap-b: peer turn = DAG)`)

    // ── Verdict ────────────────────────────────────────────────────────────────
    console.log(`\n═══ COMPOSITION VERDICT ═══`)
    console.log(`run() joins governance domain:           ${d1 > 0 ? "YES ✅" : "NO ❌"}`)
    console.log(`runWorkflow() joins governance domain:   ${d2 > 0 ? "YES ✅" : "NO ❌"}`)
    console.log(`  └ gap-a: workflow nodes counted as spawns: ${d2spawns >= 2 ? `YES ✅ (Δspawns=${d2spawns})` : `NO ❌ (Δspawns=${d2spawns})`}`)
    console.log(`ReactiveSession joins governance domain: ${d3 > 0 ? "YES ✅" : "NO ❌"}`)
    console.log(`gap-b: DAG-in-Peer via react seam:       ${dagReactions.length > 0 && d4spawns >= 2 ? `YES ✅ (Δspawns=${d4spawns})` : "NO ❌"}`)
    console.log(`total ledger tokensSpent=${led().tokensSpent} subagentsSpawned=${led().subagentsSpawned}`)
    console.log(`═══════════════════════════\n`)

    expect(d1).toBeGreaterThan(0) // run() path charges the group (execute() seed+charge)
    expect(wf.completed.length).toBe(2) // workflow runs fine
    expect(d2).toBeGreaterThan(0) // workflow nodes charge tokens to the shared ledger (via parentOpts)
    expect(d2spawns).toBeGreaterThanOrEqual(2) // GAP-A FIX: standalone workflow counts its nodes as spawns
    expect(d3).toBeGreaterThan(0) // ReactiveSession peers run via run() → charge the group
    // GAP-B FIX: a peer's turn can be a DAG via the react seam, and its nodes charge the shared group.
    expect(dagReactions[0]?.output).toContain("ran DAG: 2 nodes")
    expect(d4spawns).toBeGreaterThanOrEqual(2)
  }, 300_000)
})
