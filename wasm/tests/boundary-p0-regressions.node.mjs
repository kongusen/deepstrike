// WASM SDK × real kernel (pkg-node) regressions for the 0.2.74 boundary P0 audit.
// Run after `wasm-pack build ../crates/deepstrike-wasm --target nodejs --dev --out-dir ../../wasm/pkg-node`
// and `npm run build`. The loader hook maps `@deepstrike/wasm-kernel` onto the node-target build so
// the runner exercises the real canonical kernel instead of the Jest mock.
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
const { messageToKernelMessage } = await import(new URL("runtime/kernel-step.js", dist).href)
const { RuntimeRunner } = await import(new URL("runtime/runner.js", dist).href)
const { InMemorySessionLog } = await import(new URL("runtime/session-log.js", dist).href)
const { LocalExecutionPlane } = await import(new URL("runtime/execution-plane.js", dist).href)

const runtime = (id, maxContextTokens = 100_000) =>
  new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), `op-${id}`, { maxContextTokens })
const tools = [{ name: "search", description: "s", parameters: { type: "object", properties: {} } }]
let passed = 0
const ok = name => { passed += 1; console.log(`ok ${passed} - ${name}`) }

// P0-1 · preloaded history keeps the assistant call paired with its result.
{
  const rt = runtime("history")
  await rt.applyHostEvent({ kind: "set_tools", tools })
  await rt.applyHostEvent({
    kind: "preload_history",
    messages: [
      { role: "user", content: "find x", toolCalls: [] },
      { role: "assistant", content: "", toolCalls: [{ id: "call_1", name: "search", arguments: "{\"q\":\"x\"}" }] },
      { role: "tool", content: "", toolCalls: [], contentParts: [{ type: "tool_result", callId: "call_1", output: "result-x", isError: false }] },
    ].map(messageToKernelMessage),
  })
  const action = await rt.startAgent({ goal: "continue" })
  assert.equal(action.kind, "call_provider")
  const turns = action.context.turns
  const call = turns.findIndex(turn => turn.toolCalls?.some(c => c.id === "call_1"))
  const result = turns.findIndex(turn => turn.contentParts?.some(p => p.type === "tool_result" && p.callId === "call_1"))
  assert.ok(call >= 0, "assistant tool call survived preload")
  assert.ok(result > call, "tool result follows its call")
  assert.equal(turns[result].content, "result-x")
  ok("P0-1 preloaded history keeps tool-call pairing")
}

// P0-2 · an inexpressible model workflow is answered by the kernel, not thrown by the host.
{
  const rt = runtime("loop")
  await rt.applyHostEvent({ kind: "set_tools", tools: [...tools, { name: "start_workflow", description: "w", parameters: { type: "object" } }] })
  const action = await rt.startAgent({ goal: "g" }, { goal: "g", exposure_baseline: ["start_workflow"] })
  const next = await rt.applyHostEvent({
    kind: "provider_result",
    effect_id: action.effectId,
    message: { role: "assistant", content: "", tool_calls: [{ id: "c1", name: "start_workflow", arguments: { spec: { nodes: [{ task: "t", role: "implement", loop: { maxIters: 2 } }] } } }] },
  })
  assert.equal(next?.kind, "call_provider", "the run continues with a model-visible rejection")
  ok("P0-2 unsupported workflow kind becomes a kernel rejection")
}

// P0-3 · caller node ids are the wire identity; anonymous appended nodes never collide.
{
  const rt = runtime("ids")
  await rt.applyHostEvent({ kind: "configure_run", config: { resource_quota: { max_concurrent_subagents: 4 } } })
  const start = await rt.startWorkflow({ nodes: [{ nodeId: "alpha", task: "a", role: "implement" }] })
  assert.equal(start.kind, "spawn_workflow")
  assert.deepEqual(start.nodes.map(n => [n.task_id, n.node_id]), [["wf-node0", "alpha"]])
  await rt.applyHostEvent({ kind: "workflow_spawn_result", effect_id: start.effectId })
  const grown = await rt.applyHostEvent({
    kind: "sub_agent_completed",
    result: {
      agent_id: "wf-node0",
      result: { termination: "completed", final_message: { content: "done" }, turns_used: 1, total_tokens_used: 1 },
      submitted_nodes: [{ task: "anonymous", role: "implement" }],
    },
  })
  assert.equal(grown?.kind, "spawn_workflow")
  assert.deepEqual(grown.nodes.map(n => [n.task_id, n.node_id]), [["wf-node1", "wf-node1"]])
  ok("P0-3 caller ids preserved and appended anonymous ids unique")
}

// A live tool result reaches the provider as a structural tool_result paired with its call; the
// projection used to read `tc.id` (the wire says `call_id`) and ignore `tool_call_id` entirely.
{
  const rt = runtime("live")
  await rt.applyHostEvent({ kind: "set_tools", tools: [{ name: "ping", description: "ping", parameters: { type: "object" } }] })
  const first = await rt.startAgent({ goal: "use ping" }, { goal: "use ping", exposure_baseline: ["ping"] })
  const execute = await rt.applyHostEvent({
    kind: "provider_result",
    effect_id: first.effectId,
    message: { role: "assistant", content: "", tool_calls: [{ id: "call_ping", name: "ping", arguments: {} }] },
    stop_reason: "tool_use",
  })
  assert.equal(execute.kind, "execute_tool")
  const next = await rt.applyHostEvent({
    kind: "tool_results",
    effect_id: execute.effectId,
    results: [{ call_id: "call_ping", output: "pong", is_error: false }],
  })
  assert.equal(next.kind, "call_provider")
  const turns = next.context.turns
  assert.ok(turns.some(turn => turn.toolCalls.some(call => call.id === "call_ping")), "assistant call keeps its id")
  assert.ok(turns.some(turn => turn.contentParts?.some(p => p.type === "tool_result" && p.callId === "call_ping")), "tool result is structural")
  ok("live tool results stay paired with their calls")
}

// P0-5 · an onToolResult replacement is what every consumer sees, the session record included.
{
  const plane = {
    register() { return this },
    unregister() { return this },
    schemas() { return [{ name: "fetch", description: "fetch", parameters: '{"type":"object"}' }] },
    async *executeAll(calls) {
      for (const call of calls) {
        yield {
          type: "tool_result", callId: call.id, name: call.name, content: "SECRET-TOKEN", isError: false,
          contentParts: [{ type: "text", text: "SECRET-TOKEN" }],
        }
      }
    },
  }
  const contexts = []
  const provider = {
    async complete() { return { role: "assistant", content: "", toolCalls: [] } },
    async *stream(context) {
      contexts.push(context)
      if (contexts.length === 1) yield { type: "tool_call", id: "call_fetch", name: "fetch", arguments: {} }
      else yield { type: "text_delta", delta: "done" }
    },
  }
  const sessionLog = new InMemorySessionLog()
  const runner = new RuntimeRunner({
    provider, sessionLog, executionPlane: plane, maxTokens: 8000, maxTurns: 4,
    baselineToolIds: ["fetch"],
    onToolResult: () => ({ replaceOutput: "[redacted]" }),
  })
  for await (const _event of runner.run({ sessionId: "p0-5", goal: "fetch it" })) { /* drain */ }
  assert.ok(contexts.length >= 2)
  assert.ok(!JSON.stringify(contexts[1]).includes("SECRET-TOKEN"), "next request carries no redacted content")
  assert.ok(!JSON.stringify(await sessionLog.read("p0-5")).includes("SECRET-TOKEN"), "session record carries no redacted content")
  ok("P0-5 onToolResult redaction covers every consumer")
}

// P0-6 · a page-out whose store returned no ref fails instead of resolving under a minted ref.
{
  const pressured = async id => {
    // A small budget so a few large tool turns force a page-out.
    const rt = runtime(id, 4_000)
    await rt.applyHostEvent({ kind: "set_tools", tools: [{ name: "ping", description: "ping", parameters: { type: "object" } }] })
    let action = await rt.startAgent({ goal: "keep pinging" }, { goal: "keep pinging", exposure_baseline: ["ping"] })
    for (let turn = 0; turn < 40 && action; turn += 1) {
      if (action.kind === "archive_page_out") return { rt, action }
      if (action.kind === "call_provider") {
        action = await rt.applyHostEvent({
          kind: "provider_result", effect_id: action.effectId,
          message: { role: "assistant", content: "", tool_calls: [{ id: `call_${turn}`, name: "ping", arguments: { n: turn } }] },
          observed_input_tokens: 3_900, stop_reason: "tool_use",
        })
      } else if (action.kind === "execute_tool") {
        action = await rt.applyHostEvent({
          kind: "tool_results", effect_id: action.effectId,
          results: [{ call_id: action.calls[0].id, output: `pong ${turn}: ${"a long tool body worth compacting ".repeat(20)}`, is_error: false }],
        })
      } else throw new Error(`unexpected effect while building pressure: ${action.kind}`)
    }
    throw new Error("context pressure never published an archive_page_out")
  }
  const missing = await pressured("page-out-missing")
  await missing.rt.applyHostEvent({ kind: "page_out_archive_result", effect_id: missing.action.effectId })
  const kinds = missing.rt.drainHostObservations().map(obs => obs.kind)
  assert.ok(kinds.includes("page_out_archive_failed"), "the kernel records the page-out as failed")
  assert.ok(!kinds.includes("payload_residency_changed"), "nothing is marked paged-out under a minted ref")
  const stored = await pressured("page-out-stored")
  await stored.rt.applyHostEvent({ kind: "page_out_archive_result", effect_id: stored.action.effectId, payload_ref: "payload:stored" })
  const residency = stored.rt.drainHostObservations().find(obs => obs.kind === "payload_residency_changed")
  assert.equal(residency?.payload_ref, "payload:stored")
  ok("P0-6 page-out receipts name a stored ref or fail")
}

// P0-4 · the milestone verifier receives the phase's host-side requirements.
{
  const provider = {
    async complete() { return { role: "assistant", content: "done", toolCalls: [] } },
    async *stream() { yield { type: "text_delta", delta: "done" } },
  }
  const seen = []
  const runner = new RuntimeRunner({
    provider,
    sessionLog: new InMemorySessionLog(),
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 4000,
    maxTurns: 4,
    milestoneContract: { phases: [{ id: "phase1", criteria: ["tests pass"], requiredEvidence: ["test log"] }] },
    onMilestoneEvaluate: ctx => { seen.push(ctx); return { phaseId: ctx.phaseId, passed: true } },
  })
  const events = []
  for await (const event of runner.run({ sessionId: "p0-4", goal: "ship it" })) events.push(event)
  if (process.env.DEBUG_P0) console.log(JSON.stringify(events))
  assert.ok(seen.length >= 1, "milestone evaluated")
  assert.deepEqual(seen[0], { phaseId: "phase1", criteria: ["tests pass"], requiredEvidence: ["test log"] })
  ok("P0-4 milestone criteria reach onMilestoneEvaluate")
}

console.log(`boundary P0 regressions: ${passed} checks passed`)
