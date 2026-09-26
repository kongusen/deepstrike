// WASM SDK × real kernel (pkg-node) regressions for the 0.2.74 boundary P2 audit.
// Same harness as boundary-p1-regressions.node.mjs.
import assert from "node:assert/strict"
import { register } from "node:module"
import { pathToFileURL } from "node:url"

const kernelUrl = new URL("../pkg-node/deepstrike_wasm.js", import.meta.url).href
register(
  `data:text/javascript,${encodeURIComponent(`
    export async function resolve(specifier, context, next) {
      if (specifier === "@deepstrike/wasm-kernel") return { url: ${JSON.stringify(kernelUrl)}, shortCircuit: true }
      return next(specifier, context)
    }`)}`,
  pathToFileURL("./"),
)

const dist = new URL("../dist/", import.meta.url)
const { CanonicalKernel } = await import("@deepstrike/wasm-kernel")
const { InMemoryKernelJournal } = await import(new URL("runtime/kernel-journal.js", dist).href)
const { CanonicalRunnerRuntime } = await import(new URL("runtime/canonical-kernel-step.js", dist).href)
const { RuntimeRunner } = await import(new URL("runtime/runner.js", dist).href)
const { InMemorySessionLog } = await import(new URL("runtime/session-log.js", dist).href)
const { governancePolicyPatch } = await import(new URL("governance.js", dist).href)

let passed = 0
const ok = name => { passed += 1; console.log(`ok ${passed} - ${name}`) }
const runtime = (journal, id) => new CanonicalRunnerRuntime(new CanonicalKernel(), journal, id, { maxContextTokens: 100_000 })

// P2-2 · a refused input leaves the host where the kernel is.
{
  const rt = runtime(new InMemoryKernelJournal(), "op-p2-2")
  await rt.startAgent({ goal: "answer" })
  rt.drainNewMessages()
  const turns = rt.turn()
  await assert.rejects(rt.applyHostEvent({
    kind: "provider_result", effect_id: "step:999:effect:0",
    message: { role: "assistant", content: "stale", toolCalls: [] },
  }))
  assert.equal(rt.turn(), turns)
  assert.deepEqual(rt.drainNewMessages(), [])
  ok("P2-2 a refused provider result does not advance the host's view")
}

// P2-3 · transitions of one operation never overlap on the outbound slot; a drained envelope's
// observations survive the restore.
{
  const journal = new InMemoryKernelJournal()
  const rt = runtime(journal, "op-serial")
  await rt.startAgent({ goal: "answer" })
  const events = []
  const stage = journal.stageOutboundEnvelope.bind(journal)
  const clear = journal.clearOutboundEnvelope.bind(journal)
  journal.stageOutboundEnvelope = async (op, json) => { events.push("stage"); await new Promise(r => setTimeout(r, 5)); return stage(op, json) }
  journal.clearOutboundEnvelope = async op => { events.push("clear"); return clear(op) }
  const update = progress => rt.host.transition({ kind: "host_control", command: { kind: "update_task", update: { progress } } })
  await Promise.all([update("a"), update("b")])
  assert.deepEqual(events, ["stage", "clear", "stage", "clear"])
  ok("P2-3 two transitions of one operation never share the outbound slot")
}
{
  const journal = new InMemoryKernelJournal()
  const rt = runtime(journal, "op-drain")
  await rt.startAgent({ goal: "answer" })
  await journal.stageOutboundEnvelope("op-drain", JSON.stringify({
    operation_id: "op-drain", input_id: "crash-window", observed_at_ms: String(Date.now() + 1_000),
    input: { kind: "host_control", command: { kind: "apply_policy_patch", expected_revision: "0", patch: governancePolicyPatch({ vetoes: ["x"] }) } },
  }))
  const woken = runtime(journal, "op-drain")
  await woken.restore()
  assert.ok(woken.drainHostObservations().some(o => o.kind === "live_policy_changed"))
  ok("P2-3 a restore keeps the observations of the envelope it drains")
}

// P2-4 · a fast node's dependent starts while its slow sibling still works.
{
  let releaseSlow
  const slowReleased = new Promise(resolve => { releaseSlow = resolve })
  const runner = new RuntimeRunner({
    sessionLog: new InMemorySessionLog(), maxTokens: 8000,
    provider: { async complete() { return { role: "assistant", content: "", toolCalls: [] } }, async *stream() {} },
    subAgentOrchestrator: {
      async run(ctx) {
        const goal = ctx.spec.goal
        if (goal.includes("slow")) {
          await Promise.race([slowReleased, new Promise((_, reject) => setTimeout(() => reject(new Error("round barrier")), 2_000))])
        }
        if (goal.includes("after fast")) releaseSlow()
        const id = ctx.manifest.agent_id
        return { agentId: id, result: { termination: "completed", finalMessage: { role: "assistant", content: id, toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 } }
      },
    },
  })
  const outcome = await runner.runWorkflow({
    nodes: [
      { task: "slow worker", role: "explore" },
      { task: "fast worker", role: "explore" },
      { task: "after fast", role: "plan", dependsOn: [1] },
    ],
  }, { sessionId: "p2-4" })
  assert.deepEqual(outcome.nodeOutcomes.map(node => node.status).sort(), ["completed", "completed", "completed"])
  ok("P2-4 workflow completions stream without a round barrier")
}

console.log(`# ${passed} P2 boundary checks passed against the real kernel`)
