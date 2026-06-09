/**
 * Real-model end-to-end for the workflow primitives: tournament, loop-until-done, and the
 * verify_rules verification shape. Each drives the kernel state machine against a live LLM.
 *
 * Requires a provider key. Run with:
 *   set -a; source .env; set +a; npx jest e2e/primitives --testTimeout 300000
 * Skips cleanly when no key is present.
 */
import {
  RuntimeRunner,
  InMemorySessionLog,
  LocalExecutionPlane,
  createTournament,
  createLoopUntilDone,
} from "../../src/index.js"
import type { WorkflowSpec } from "../../src/index.js"
import { getKernel } from "../../src/kernel.js"
import type { LLMProvider, Message } from "../../src/types.js"
import { loadProviders, anyProvider } from "./providers.js"

const provider = anyProvider(loadProviders())
const maybe = provider ? describe : describe.skip

/** One-shot text completion against the live model. */
async function ask(p: LLMProvider, system: string, user: string): Promise<string> {
  const msg: Message = await p.complete(
    { systemText: system, turns: [{ role: "user", content: user, toolCalls: [] }] },
    [],
  )
  return typeof msg.content === "string" ? msg.content : ""
}

maybe("real-model workflow primitives", () => {
  it("tournament: a live model judges pairwise until one winner", async () => {
    const p = provider!
    const entrants = ["zaptool", "flowcli", "grok-shell", "nimbus"]
    const t = createTournament(entrants)

    let action = t.start()
    let rounds = 0
    while (action.kind === "judgeRound") {
      rounds++
      const winners: string[] = []
      for (const m of action.matches ?? []) {
        const reply = await ask(
          p,
          "You judge CLI tool names. Reply with ONLY the single letter A or B.",
          `Which is the better name for a developer CLI tool?\nA) ${m.left}\nB) ${m.right}`,
        )
        const u = reply.toUpperCase()
        const ai = u.indexOf("A")
        const bi = u.indexOf("B")
        const pickB = bi !== -1 && (ai === -1 || bi < ai)
        winners.push(pickB ? m.right : m.left)
      }
      action = t.feedRound(winners)
    }

    expect(action.kind).toBe("done")
    expect(entrants).toContain(action.winner)
    expect(action.roundsUsed).toBe(rounds)
    expect(rounds).toBe(2) // 4 entrants → 2 rounds
  }, 300_000)

  it("loop-until-done: a live model worker drives the loop to termination", async () => {
    const p = provider!
    // Stop on no-new-findings, with a hard 3-round backstop guaranteeing termination.
    const loop = createLoopUntilDone([{ kind: "noNewFindings" }, { kind: "maxRounds", maxRounds: 3 }])

    let action = loop.start()
    const found: string[] = []
    while (action.kind === "spawn") {
      const reply = await ask(
        p,
        "You audit a config for issues. Reply with ONE short new issue not already listed, or exactly NONE.",
        `Config: timeout=0, retries=-1, debug=true.\nAlready found: ${found.join("; ") || "(none)"}`,
      )
      const isNone = reply.trim().toUpperCase().startsWith("NONE")
      if (!isNone) found.push(reply.trim().slice(0, 60))
      action = loop.feed({ newFindings: isNone ? 0 : 1, errors: 0 })
    }

    expect(action.kind).toBe("done")
    expect(["noNewFindings", "maxRounds"]).toContain(action.reason)
    expect(action.roundsUsed).toBeLessThanOrEqual(3)
    expect(action.roundsUsed).toBeGreaterThanOrEqual(1)
  }, 300_000)

  it("verify_rules: live verifiers + skeptic run as a gated workflow DAG", async () => {
    const runner = new RuntimeRunner({
      provider: provider!,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000,
      maxTurns: 4,
    })
    const kernel = new (getKernel().KernelRuntime)({ maxTokens: 8000 })
    kernel.step(JSON.stringify({ version: 1, event: { kind: "start_run", task: { goal: "review", criteria: [] } } }))
    ;(runner as any).activeKernel = kernel
    ;(runner as any).currentSessionId = "verify-e2e"
    ;(runner as any).pendingObservations = []

    // The shape verify_rules(rules, skeptic) produces: one verify node per rule + a skeptic
    // depending on all of them. Verifiers run with no inherited author context (bias-resistant).
    const spec: WorkflowSpec = {
      nodes: [
        { task: "Check this rule against `price = 9.99` (float money): is it violated? Answer briefly.", role: "verify" },
        { task: "Check this rule against `catch(e){}` (errors must propagate): is it violated? Answer briefly.", role: "verify" },
        { task: "Skeptic: given the two verifier findings, list only the real violations.", role: "verify", dependsOn: [0, 1] },
      ],
    }

    const outcome = await runner.runWorkflow(spec)
    expect(outcome.completed.sort()).toEqual(["wf-node0", "wf-node1", "wf-node2"])
    expect(outcome.failed).toEqual([])
  }, 300_000)
})
