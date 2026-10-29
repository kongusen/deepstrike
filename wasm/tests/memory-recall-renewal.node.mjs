// K4 · M5 on the real WASM kernel: a sprint renewal re-fires the memory prefetch, and the hit
// reaches the new sprint as a keyed knowledge entry carrying the canonical recall text. A running
// operation refuses host history commands, so a prefetch that pushes history after renewal is
// dropped by the kernel — a mocked commit cannot show that.
import assert from "node:assert/strict"
import { register } from "node:module"
import { pathToFileURL } from "node:url"
const kernelUrl = new URL("../pkg-node/deepstrike_wasm.js", import.meta.url).href
register(`data:text/javascript,${encodeURIComponent(`export async function resolve(s,c,n) {
  if (s === "@deepstrike/wasm-kernel") return {url:${JSON.stringify(kernelUrl)},shortCircuit:true}; return n(s,c)
}`)}`, pathToFileURL("./"))
const { RuntimeRunner, collectText } = await import("../dist/runtime/runner.js")
const { InMemorySessionLog } = await import("../dist/runtime/session-log.js")
const { LocalExecutionPlane } = await import("../dist/runtime/execution-plane.js")
const { tool } = await import("../dist/tools/index.js")

const RECALL = "LONGTERM_FACT_FOR_SPRINT"
const RECALL_TURN = `[MEMORY record_id=record-renewal kind=reference trust=host_verified] ${RECALL}`
const scope = { tenant_id: "agent-k4", namespace: "renewal" }
const phases = []
let sawRenewal = false
let sawRecallAfterRenewal = false
let call = 0

const memoryStore = {
  put: async () => {},
  get: async () => null,
  delete: async () => {},
  saveSession: async () => {},
  search: async () => [{
    record: {
      record_id: "record-renewal", scope, name: "renewal-fact", kind: "reference", content: RECALL,
      description: "renewal fixture", provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
      created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
    },
    score: 0.9,
    why: "fixture",
  }],
}

const provider = {
  async complete() { return { role: "assistant", content: "", toolCalls: [] } },
  async *stream(context) {
    call += 1
    // The renewal summary may carry the turn-1 hit forward in `turns`; only the re-fetch puts
    // the recall text in knowledge.
    if (sawRenewal && (context.systemKnowledge ?? "").includes(RECALL_TURN)) sawRecallAfterRenewal = true
    if (call <= 10 && !sawRecallAfterRenewal) {
      yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
      return
    }
    yield { type: "text_delta", delta: "done" }
  },
}

const plane = new LocalExecutionPlane()
// Output shrinks after the first renewal so pressure subsides and the re-fetched entry survives to
// the next render.
plane.register(tool("bulk", "bulk", { type: "object", properties: {} }, async () =>
  (sawRenewal ? "ok" : "The quick fox jumps over lazy dogs and writes long context reports daily. ".repeat(7))))
const sessionLog = new InMemorySessionLog()
const runner = new RuntimeRunner({
  provider,
  sessionLog,
  executionPlane: plane,
  baselineToolIds: ["bulk"],
  maxTokens: 200,
  maxTurns: 30,
  agentId: "agent-k4",
  memoryScope: scope,
  memoryStore,
  repeatFuse: false,
  preQueryMemory: ctx => {
    phases.push(ctx.phase)
    if (ctx.phase === "renewal") sawRenewal = true
    return [{ scope, query: "relevant facts", top_k: 5, kinds: [] }]
  },
})

await collectText(runner.run({ sessionId: "renewal-requery", goal: "long sprint work" }))

const events = (await sessionLog.read("renewal-requery")).map(entry => entry.event)
assert.ok(events.some(event => event.kind === "context_renewed"), "the run renewed its context")
assert.equal(phases[0], "initial")
assert.ok(phases.includes("renewal"), "the prefetch re-fired on renewal")
assert.ok(sawRecallAfterRenewal, "the re-fetched hit reached the new sprint's knowledge")
assert.equal(events.filter(event => event.kind === "memory_retrieval_result").length, phases.length,
  "every prefetch is audited")
console.log("memory recall renewal: real WASM kernel re-fetch and audit pass")
